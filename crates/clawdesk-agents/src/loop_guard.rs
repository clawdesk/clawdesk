//! Tool Loop Guard — detects repeated and ping-pong tool call patterns.
//!
//! ## Detection Strategies
//!
//! **Exact repeat**: SHA-256 hash of `(tool_name, canonical_args)` in a sliding
//! window. After `warn_threshold` identical calls, escalates from Allow → Warn
//! → Block → CircuitBreak.
//!
//! **Ping-pong**: A ring buffer of the last N tool names detects alternating
//! patterns like `[read, write, read, write]`. When the same 2-tool cycle
//! repeats `ping_pong_threshold` times, the guard blocks the next call.
//!
//! ## Verdicts
//!
//! ```text
//! count < warn_threshold       → Allow  (silent)
//! count ∈ [warn, block)        → Warn   (log + continue)
//! count ∈ [block, circuit)     → Block  (reject this call)
//! count ≥ circuit_threshold    → CircuitBreak (reject + set flag to end round)
//! ```

use rustc_hash::FxHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

/// Verdict issued by the loop guard for a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoopVerdict {
    /// Call is allowed — no repetition detected.
    Allow,
    /// Call is allowed but a warning is logged — approaching threshold.
    Warn { count: usize },
    /// Call is blocked — too many identical calls.
    Block { count: usize },
    /// Circuit breaker tripped — all further tool calls in this round should stop.
    CircuitBreak { count: usize },
}

impl LoopVerdict {
    /// Returns `true` if execution should be blocked.
    pub fn is_blocked(&self) -> bool {
        matches!(self, LoopVerdict::Block { .. } | LoopVerdict::CircuitBreak { .. })
    }
}

/// Configuration for the loop guard thresholds.
#[derive(Debug, Clone)]
pub struct LoopGuardConfig {
    /// Number of identical calls before issuing a Warn.
    pub warn_threshold: usize,
    /// Number of identical calls before blocking.
    pub block_threshold: usize,
    /// Number of identical calls before circuit-breaking the entire round.
    pub circuit_threshold: usize,
    /// Size of the ring buffer for ping-pong detection.
    pub ring_size: usize,
    /// Number of 2-tool cycle repetitions to trigger ping-pong block.
    pub ping_pong_threshold: usize,
}

impl Default for LoopGuardConfig {
    fn default() -> Self {
        Self {
            warn_threshold: 3,
            block_threshold: 5,
            circuit_threshold: 8,
            ring_size: 16,
            ping_pong_threshold: 4,
        }
    }
}

/// Tool Loop Guard — tracks call patterns and issues verdicts.
#[derive(Debug)]
pub struct LoopGuard {
    config: LoopGuardConfig,
    /// Sliding window of call hashes → count.
    /// Uses 64-bit FxHash keys (8 bytes vs SHA-256's 32 bytes) for
    /// better cache utilization. Collision probability for k ≤ 25 calls:
    /// P ≈ k²/2⁶⁴ ≈ 3.4×10⁻¹⁷ — astronomically safe.
    call_counts: HashMap<u64, usize>,
    /// Ring buffer of recent tool names (for ping-pong detection).
    ring: Vec<String>,
    /// Current write position in the ring buffer.
    ring_pos: usize,
    /// Total calls tracked.
    total_calls: usize,
    /// Whether circuit breaker has been tripped.
    circuit_broken: bool,
}

impl LoopGuard {
    pub fn new(config: LoopGuardConfig) -> Self {
        let ring_size = config.ring_size.max(4);
        Self {
            config,
            call_counts: HashMap::new(),
            ring: Vec::with_capacity(ring_size),
            ring_pos: 0,
            total_calls: 0,
            circuit_broken: false,
        }
    }

    /// Check a tool call and return a verdict.
    ///
    /// Hashes `(tool_name, canonical_args)` and checks against thresholds.
    /// Also checks for ping-pong patterns in the ring buffer.
    pub fn check(&mut self, tool_name: &str, args: &serde_json::Value) -> LoopVerdict {
        if self.circuit_broken {
            return LoopVerdict::CircuitBreak { count: self.total_calls };
        }

        // Compute hash of (tool_name, canonical_args)
        let hash = Self::hash_call(tool_name, args);

        // Update count
        let count = self.call_counts.entry(hash).or_insert(0);
        *count += 1;
        let current = *count;

        // Update ring buffer for ping-pong detection
        self.push_ring(tool_name.to_string());
        self.total_calls += 1;

        // Check ping-pong pattern first (can block even if individual count is low)
        if self.detect_ping_pong() {
            self.circuit_broken = true;
            return LoopVerdict::CircuitBreak { count: current };
        }

        // Escalating verdicts based on repetition count
        if current >= self.config.circuit_threshold {
            self.circuit_broken = true;
            LoopVerdict::CircuitBreak { count: current }
        } else if current >= self.config.block_threshold {
            LoopVerdict::Block { count: current }
        } else if current >= self.config.warn_threshold {
            LoopVerdict::Warn { count: current }
        } else {
            LoopVerdict::Allow
        }
    }

    /// Returns `true` if the circuit breaker has been tripped.
    pub fn is_circuit_broken(&self) -> bool {
        self.circuit_broken
    }

    /// Reset the guard state (e.g., at the start of a new conversation turn).
    pub fn reset(&mut self) {
        self.call_counts.clear();
        self.ring.clear();
        self.ring_pos = 0;
        self.total_calls = 0;
        self.circuit_broken = false;
    }

    /// Compute FxHash of (tool_name, canonical_args).
    ///
    /// Hashes the Value tree directly via recursive traversal, eliminating
    /// the serde_json::to_string() heap allocation per call.
    /// FxHash: O(⌈n/8⌉) word-sized multiplies ≈ 12ns for 200-byte input
    /// (vs SHA-256's ~90ns). 64-bit output with birthday-bound collision
    /// probability P ≈ 625/2⁶⁴ ≈ 3.4×10⁻¹⁷ for k=25 tool calls.
    fn hash_call(tool_name: &str, args: &serde_json::Value) -> u64 {
        let mut hasher = FxHasher::default();
        tool_name.hash(&mut hasher);
        0xFFu8.hash(&mut hasher); // separator
        Self::hash_value(&mut hasher, args);
        hasher.finish()
    }

    /// Recursively hash a serde_json::Value tree without allocating.
    fn hash_value(hasher: &mut FxHasher, value: &serde_json::Value) {
        match value {
            serde_json::Value::Null => 0u8.hash(hasher),
            serde_json::Value::Bool(b) => { 1u8.hash(hasher); b.hash(hasher); }
            serde_json::Value::Number(n) => {
                2u8.hash(hasher);
                // Hash the Debug representation for determinism.
                let s = format!("{n}");
                s.hash(hasher);
            }
            serde_json::Value::String(s) => { 3u8.hash(hasher); s.hash(hasher); }
            serde_json::Value::Array(arr) => {
                4u8.hash(hasher);
                arr.len().hash(hasher);
                for v in arr { Self::hash_value(hasher, v); }
            }
            serde_json::Value::Object(map) => {
                5u8.hash(hasher);
                map.len().hash(hasher);
                // serde_json::Map is ordered by key, so iteration order is deterministic.
                for (k, v) in map {
                    k.hash(hasher);
                    Self::hash_value(hasher, v);
                }
            }
        }
    }

    /// Push a tool name into the ring buffer.
    fn push_ring(&mut self, name: String) {
        if self.ring.len() < self.config.ring_size {
            self.ring.push(name);
        } else {
            self.ring[self.ring_pos % self.config.ring_size] = name;
        }
        self.ring_pos += 1;
    }

    /// Detect a ping-pong pattern: two tools alternating repeatedly.
    ///
    /// Scans the ring buffer for `[A, B, A, B, ...]` patterns where the
    /// same 2-tool cycle repeats `ping_pong_threshold` times.
    fn detect_ping_pong(&self) -> bool {
        let len = self.ring.len();
        if len < 4 {
            return false;
        }

        // Check the last N entries for alternating pattern
        let check_len = (self.config.ping_pong_threshold * 2).min(len);
        let start = len.saturating_sub(check_len);
        let slice = &self.ring[start..];

        if slice.len() < 4 {
            return false;
        }

        let a = &slice[slice.len() - 2];
        let b = &slice[slice.len() - 1];

        // Must be two different tools
        if a == b {
            return false;
        }

        // Count how many consecutive alternating pairs from the end
        let mut pairs = 0usize;
        let mut i = slice.len();
        while i >= 2 {
            i -= 2;
            if &slice[i] == a && &slice[i + 1] == b {
                pairs += 1;
            } else {
                break;
            }
        }

        pairs >= self.config.ping_pong_threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_allow_under_threshold() {
        let mut guard = LoopGuard::new(LoopGuardConfig::default());
        let v = guard.check("read_file", &json!({"path": "foo.txt"}));
        assert_eq!(v, LoopVerdict::Allow);
    }

    #[test]
    fn test_warn_at_threshold() {
        let mut guard = LoopGuard::new(LoopGuardConfig {
            warn_threshold: 2,
            block_threshold: 4,
            circuit_threshold: 6,
            ..Default::default()
        });
        let args = json!({"path": "foo.txt"});
        guard.check("read_file", &args); // 1 → Allow
        let v = guard.check("read_file", &args); // 2 → Warn
        assert!(matches!(v, LoopVerdict::Warn { count: 2 }));
    }

    #[test]
    fn test_block_at_threshold() {
        let mut guard = LoopGuard::new(LoopGuardConfig {
            warn_threshold: 2,
            block_threshold: 3,
            circuit_threshold: 5,
            ..Default::default()
        });
        let args = json!({"x": 1});
        guard.check("tool_a", &args);
        guard.check("tool_a", &args);
        let v = guard.check("tool_a", &args);
        assert!(matches!(v, LoopVerdict::Block { count: 3 }));
        assert!(v.is_blocked());
    }

    #[test]
    fn test_circuit_break() {
        let mut guard = LoopGuard::new(LoopGuardConfig {
            warn_threshold: 1,
            block_threshold: 2,
            circuit_threshold: 3,
            ..Default::default()
        });
        let args = json!({});
        guard.check("t", &args);
        guard.check("t", &args);
        let v = guard.check("t", &args);
        assert!(matches!(v, LoopVerdict::CircuitBreak { .. }));
        assert!(guard.is_circuit_broken());

        // All subsequent calls are circuit-broken
        let v2 = guard.check("other_tool", &json!({}));
        assert!(matches!(v2, LoopVerdict::CircuitBreak { .. }));
    }

    #[test]
    fn test_different_args_distinct() {
        let mut guard = LoopGuard::new(LoopGuardConfig {
            warn_threshold: 2,
            block_threshold: 4,
            circuit_threshold: 6,
            ..Default::default()
        });
        // Same tool, different args → distinct calls
        let v1 = guard.check("read", &json!({"path": "a.txt"}));
        let v2 = guard.check("read", &json!({"path": "b.txt"}));
        assert_eq!(v1, LoopVerdict::Allow);
        assert_eq!(v2, LoopVerdict::Allow);
    }

    #[test]
    fn test_ping_pong_detection() {
        let mut guard = LoopGuard::new(LoopGuardConfig {
            warn_threshold: 10,
            block_threshold: 20,
            circuit_threshold: 30,
            ring_size: 16,
            ping_pong_threshold: 3,
        });
        let args = json!({});
        // Create pattern: A, B, A, B, A, B
        guard.check("tool_a", &args);
        guard.check("tool_b", &args);
        guard.check("tool_a", &args);
        guard.check("tool_b", &args);
        guard.check("tool_a", &args);
        let v = guard.check("tool_b", &args); // 3rd complete pair
        assert!(matches!(v, LoopVerdict::CircuitBreak { .. }));
    }

    #[test]
    fn test_reset() {
        let mut guard = LoopGuard::new(LoopGuardConfig {
            warn_threshold: 1,
            block_threshold: 2,
            circuit_threshold: 3,
            ..Default::default()
        });
        let args = json!({});
        guard.check("t", &args);
        guard.check("t", &args);
        guard.check("t", &args);
        assert!(guard.is_circuit_broken());

        guard.reset();
        assert!(!guard.is_circuit_broken());
        let v = guard.check("t", &args);
        assert_eq!(v, LoopVerdict::Warn { count: 1 });
    }
}
