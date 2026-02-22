//! Gateway grace window — debounce transient errors before surfacing.
//!
//! Prevents a single transient error (e.g., provider timeout) from
//! immediately triggering user-visible error messages. Instead, errors
//! are held in a grace window; if the operation succeeds within the
//! window, the error is silently discarded.
//!
//! ## Use Cases
//!
//! - Provider returns 503 but recovers after retry within 2s
//! - Network blip during streaming that self-heals
//! - Subagent manager reconnect races
//!
//! ## Algorithm
//!
//! Per-key (session/channel) grace state:
//! 1. On first error: record timestamp, do NOT emit.
//! 2. On success within window: clear grace state, discard error.
//! 3. On window expiry (no success): emit the original error.
//! 4. On subsequent error within window: replace with latest error.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Configuration for the grace window.
#[derive(Debug, Clone)]
pub struct GraceWindowConfig {
    /// Duration to wait before surfacing an error.
    pub window: Duration,
    /// Maximum consecutive errors before forcing immediate emission.
    pub max_suppressed: usize,
}

impl Default for GraceWindowConfig {
    fn default() -> Self {
        Self {
            window: Duration::from_secs(2),
            max_suppressed: 3,
        }
    }
}

/// Outcome of submitting an event to the grace window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraceOutcome {
    /// Error is being held — do NOT emit to the user yet.
    Suppressed,
    /// Grace window expired or max suppressed reached — emit this error.
    Emit(String),
    /// A success cleared the pending error — nothing to emit.
    Cleared,
}

/// Internal state for a pending grace window.
struct GraceSlot {
    /// The most recent error message.
    error: String,
    /// When the first error in this burst was recorded.
    first_error_at: Instant,
    /// Number of errors suppressed in this burst.
    suppressed_count: usize,
}

/// Grace window manager — tracks pending errors per key.
pub struct GraceWindowManager {
    config: GraceWindowConfig,
    slots: HashMap<String, GraceSlot>,
}

impl GraceWindowManager {
    pub fn new(config: GraceWindowConfig) -> Self {
        Self {
            config,
            slots: HashMap::new(),
        }
    }

    /// Record an error for the given key.
    ///
    /// Returns `Suppressed` if the error is being held, or `Emit` if the
    /// grace window has expired or max suppressed count is reached.
    pub fn record_error(&mut self, key: &str, error: String) -> GraceOutcome {
        let now = Instant::now();

        if let Some(slot) = self.slots.get_mut(key) {
            slot.error = error.clone();
            slot.suppressed_count += 1;

            // Force emission if max suppressed reached
            if slot.suppressed_count >= self.config.max_suppressed {
                self.slots.remove(key);
                return GraceOutcome::Emit(error);
            }

            // Force emission if window expired
            if now.duration_since(slot.first_error_at) >= self.config.window {
                self.slots.remove(key);
                return GraceOutcome::Emit(error);
            }

            GraceOutcome::Suppressed
        } else {
            self.slots.insert(
                key.to_string(),
                GraceSlot {
                    error,
                    first_error_at: now,
                    suppressed_count: 1,
                },
            );
            GraceOutcome::Suppressed
        }
    }

    /// Record a success for the given key, clearing any pending error.
    pub fn record_success(&mut self, key: &str) -> GraceOutcome {
        if self.slots.remove(key).is_some() {
            GraceOutcome::Cleared
        } else {
            GraceOutcome::Cleared
        }
    }

    /// Check all slots for expired grace windows and return errors that
    /// should now be emitted.
    ///
    /// Call this periodically (e.g., on a timer tick) to flush expired errors.
    pub fn flush_expired(&mut self) -> Vec<(String, String)> {
        let now = Instant::now();
        let mut expired = Vec::new();

        self.slots.retain(|key, slot| {
            if now.duration_since(slot.first_error_at) >= self.config.window {
                expired.push((key.clone(), slot.error.clone()));
                false
            } else {
                true
            }
        });

        expired
    }

    /// Number of active grace windows.
    pub fn active_count(&self) -> usize {
        self.slots.len()
    }

    /// Check if a key has a pending error in the grace window.
    pub fn has_pending(&self, key: &str) -> bool {
        self.slots.contains_key(key)
    }

    /// Clear all pending grace windows.
    pub fn clear(&mut self) {
        self.slots.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_first_error_suppressed() {
        let mut mgr = GraceWindowManager::new(GraceWindowConfig::default());
        let result = mgr.record_error("session-1", "provider timeout".into());
        assert_eq!(result, GraceOutcome::Suppressed);
        assert!(mgr.has_pending("session-1"));
    }

    #[test]
    fn test_success_clears_error() {
        let mut mgr = GraceWindowManager::new(GraceWindowConfig::default());
        mgr.record_error("session-1", "provider timeout".into());
        let result = mgr.record_success("session-1");
        assert_eq!(result, GraceOutcome::Cleared);
        assert!(!mgr.has_pending("session-1"));
    }

    #[test]
    fn test_max_suppressed_forces_emit() {
        let mut mgr = GraceWindowManager::new(GraceWindowConfig {
            window: Duration::from_secs(10),
            max_suppressed: 3,
        });

        assert_eq!(
            mgr.record_error("s1", "err1".into()),
            GraceOutcome::Suppressed
        );
        assert_eq!(
            mgr.record_error("s1", "err2".into()),
            GraceOutcome::Suppressed
        );
        // Third error hits max_suppressed
        assert_eq!(
            mgr.record_error("s1", "err3".into()),
            GraceOutcome::Emit("err3".into())
        );
        assert!(!mgr.has_pending("s1"));
    }

    #[test]
    fn test_flush_expired() {
        let mut mgr = GraceWindowManager::new(GraceWindowConfig {
            window: Duration::from_millis(1),
            max_suppressed: 100,
        });

        mgr.record_error("s1", "err1".into());
        mgr.record_error("s2", "err2".into());

        // Sleep to let window expire
        std::thread::sleep(Duration::from_millis(5));

        let expired = mgr.flush_expired();
        assert_eq!(expired.len(), 2);
        assert_eq!(mgr.active_count(), 0);
    }

    #[test]
    fn test_independent_keys() {
        let mut mgr = GraceWindowManager::new(GraceWindowConfig::default());
        mgr.record_error("s1", "err1".into());
        mgr.record_error("s2", "err2".into());

        mgr.record_success("s1");
        assert!(!mgr.has_pending("s1"));
        assert!(mgr.has_pending("s2"));
    }
}
