//! Context window manager — intelligent token budget allocation.
//!
//! Manages the finite context window of LLMs by tracking token usage across
//! system prompt, conversation history, RAG context, and tool outputs, then
//! applying prioritized compaction when the window is near capacity.
//!
//! # Strategy
//!
//! The context window is partitioned into reserved zones:
//!
//! ```text
//! ┌────────────────────────────────────────────────┐
//! │  System Prompt (reserved, never compacted)     │  ~5-10%
//! ├────────────────────────────────────────────────┤
//! │  Conversation History (FIFO compaction)        │  ~40-50%
//! ├────────────────────────────────────────────────┤
//! │  RAG Context (relevance-ranked compaction)     │  ~20-30%
//! ├────────────────────────────────────────────────┤
//! │  Tool Outputs (recency compaction)             │  ~10-15%
//! ├────────────────────────────────────────────────┤
//! │  Response Budget (reserved for generation)     │  ~10-15%
//! └────────────────────────────────────────────────┘
//! ```

use std::collections::VecDeque;

/// Identifies the zone a context segment belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ContextZone {
    /// System prompt — never compacted.
    SystemPrompt,
    /// Conversation messages — oldest dropped first.
    History,
    /// RAG document chunks — lowest relevance dropped first.
    Retrieval,
    /// Tool call outputs — oldest dropped first.
    ToolOutput,
}

/// A segment of content within the context window.
#[derive(Debug, Clone)]
pub struct ContextSegment {
    /// Which zone this segment belongs to.
    pub zone: ContextZone,
    /// Estimated token count for this segment.
    pub tokens: usize,
    /// Priority score (higher = more important to keep). System prompt = u32::MAX.
    pub priority: u32,
    /// Identifier for deduplication / removal.
    pub id: String,
    /// The text content.
    pub content: String,
}

/// Configuration for the context window manager.
#[derive(Debug, Clone)]
pub struct ContextWindowConfig {
    /// Total model context window size in tokens.
    pub max_context_tokens: usize,
    /// Tokens reserved for the model's response generation.
    pub response_budget: usize,
    /// Fraction of remaining budget allocated to history (0.0–1.0).
    pub history_fraction: f64,
    /// Fraction of remaining budget allocated to retrieval (0.0–1.0).
    pub retrieval_fraction: f64,
    /// Fraction of remaining budget allocated to tool outputs (0.0–1.0).
    pub tool_fraction: f64,
    /// Threshold (0.0–1.0) of capacity usage that triggers compaction.
    pub compaction_trigger: f64,
}

impl Default for ContextWindowConfig {
    fn default() -> Self {
        Self {
            max_context_tokens: 128_000,
            response_budget: 4_096,
            history_fraction: 0.50,
            retrieval_fraction: 0.30,
            tool_fraction: 0.20,
            compaction_trigger: 0.85,
        }
    }
}

impl ContextWindowConfig {
    /// Usable tokens = max_context_tokens − response_budget.
    pub fn usable_tokens(&self) -> usize {
        self.max_context_tokens.saturating_sub(self.response_budget)
    }
}

/// Result of a compaction operation.
#[derive(Debug, Clone, Default)]
pub struct CompactionResult {
    /// Number of segments removed.
    pub segments_removed: usize,
    /// Tokens freed.
    pub tokens_freed: usize,
    /// Segments remaining.
    pub segments_remaining: usize,
    /// Token usage after compaction.
    pub tokens_after: usize,
}

/// Token usage summary by zone.
#[derive(Debug, Clone, Default)]
pub struct ContextUsage {
    pub system_prompt_tokens: usize,
    pub history_tokens: usize,
    pub retrieval_tokens: usize,
    pub tool_output_tokens: usize,
    pub total_tokens: usize,
    pub usable_tokens: usize,
    pub utilization: f64,
}

/// Manages the context window for a single conversation turn.
///
/// Segments are added via `push()`, and when the window nears capacity
/// the manager can `compact()` to shed low-priority content.
pub struct ContextWindowManager {
    config: ContextWindowConfig,
    /// System prompt segments (never compacted).
    system: Vec<ContextSegment>,
    /// History segments (FIFO order).
    history: VecDeque<ContextSegment>,
    /// Retrieval segments (priority-ordered, lowest priority dropped first).
    retrieval: Vec<ContextSegment>,
    /// Tool output segments (FIFO order).
    tool_outputs: VecDeque<ContextSegment>,
    /// Running total of tokens across all zones.
    total_tokens: usize,
}

impl ContextWindowManager {
    /// Create a new context window manager with the given config.
    pub fn new(config: ContextWindowConfig) -> Self {
        Self {
            config,
            system: Vec::new(),
            history: VecDeque::new(),
            retrieval: Vec::new(),
            tool_outputs: VecDeque::new(),
            total_tokens: 0,
        }
    }

    /// Push a segment into the appropriate zone.
    ///
    /// Returns `true` if the segment was added, `false` if it would exceed
    /// the absolute limit even after compaction.
    pub fn push(&mut self, segment: ContextSegment) -> bool {
        let tokens = segment.tokens;

        // Reject if adding this segment would exceed the absolute limit
        // and compaction can't help (e.g., system prompt alone fills it).
        if tokens > self.config.usable_tokens() {
            return false;
        }

        // Auto-compact if we'd exceed capacity.
        if self.total_tokens + tokens > self.config.usable_tokens() {
            self.compact_to_fit(tokens);
        }

        // If still no room after compaction, reject.
        if self.total_tokens + tokens > self.config.usable_tokens() {
            return false;
        }

        self.total_tokens += tokens;

        match segment.zone {
            ContextZone::SystemPrompt => self.system.push(segment),
            ContextZone::History => self.history.push_back(segment),
            ContextZone::Retrieval => {
                self.retrieval.push(segment);
                // Keep sorted by priority descending for efficient eviction.
                self.retrieval.sort_by(|a, b| b.priority.cmp(&a.priority));
            }
            ContextZone::ToolOutput => self.tool_outputs.push_back(segment),
        }

        true
    }

    /// Compute current token usage by zone.
    pub fn usage(&self) -> ContextUsage {
        let system_prompt_tokens: usize = self.system.iter().map(|s| s.tokens).sum();
        let history_tokens: usize = self.history.iter().map(|s| s.tokens).sum();
        let retrieval_tokens: usize = self.retrieval.iter().map(|s| s.tokens).sum();
        let tool_output_tokens: usize = self.tool_outputs.iter().map(|s| s.tokens).sum();
        let usable = self.config.usable_tokens();
        let total = self.total_tokens;

        ContextUsage {
            system_prompt_tokens,
            history_tokens,
            retrieval_tokens,
            tool_output_tokens,
            total_tokens: total,
            usable_tokens: usable,
            utilization: if usable > 0 {
                total as f64 / usable as f64
            } else {
                1.0
            },
        }
    }

    /// Should we trigger compaction based on utilization threshold?
    pub fn needs_compaction(&self) -> bool {
        let usable = self.config.usable_tokens();
        if usable == 0 {
            return false;
        }
        (self.total_tokens as f64 / usable as f64) >= self.config.compaction_trigger
    }

    /// Compact the context to free at least `needed` tokens.
    ///
    /// Eviction order:
    /// 1. Tool outputs (oldest first)
    /// 2. Retrieval chunks (lowest priority first)
    /// 3. History messages (oldest first)
    /// System prompt is never evicted.
    pub fn compact(&mut self) -> CompactionResult {
        let target = (self.config.usable_tokens() as f64 * self.config.compaction_trigger) as usize;
        let before = self.total_tokens;
        let mut removed = 0;

        // Phase 1: Evict tool outputs (oldest first).
        while self.total_tokens > target {
            if let Some(seg) = self.tool_outputs.pop_front() {
                self.total_tokens -= seg.tokens;
                removed += 1;
            } else {
                break;
            }
        }

        // Phase 2: Evict retrieval (lowest priority = end of sorted vec).
        while self.total_tokens > target {
            if let Some(seg) = self.retrieval.pop() {
                self.total_tokens -= seg.tokens;
                removed += 1;
            } else {
                break;
            }
        }

        // Phase 3: Evict history (oldest first).
        while self.total_tokens > target {
            if let Some(seg) = self.history.pop_front() {
                self.total_tokens -= seg.tokens;
                removed += 1;
            } else {
                break;
            }
        }

        let remaining = self.system.len()
            + self.history.len()
            + self.retrieval.len()
            + self.tool_outputs.len();

        CompactionResult {
            segments_removed: removed,
            tokens_freed: before.saturating_sub(self.total_tokens),
            segments_remaining: remaining,
            tokens_after: self.total_tokens,
        }
    }

    /// Collect all segments in presentation order (system → history → retrieval → tools).
    pub fn render(&self) -> Vec<&ContextSegment> {
        let mut out: Vec<&ContextSegment> = Vec::new();
        out.extend(self.system.iter());
        out.extend(self.history.iter());
        out.extend(self.retrieval.iter());
        out.extend(self.tool_outputs.iter());
        out
    }

    /// Remove a segment by ID from any zone.
    pub fn remove(&mut self, id: &str) -> bool {
        if let Some(pos) = self.system.iter().position(|s| s.id == id) {
            self.total_tokens -= self.system[pos].tokens;
            self.system.remove(pos);
            return true;
        }
        if let Some(pos) = self.history.iter().position(|s| s.id == id) {
            self.total_tokens -= self.history[pos].tokens;
            self.history.remove(pos);
            return true;
        }
        if let Some(pos) = self.retrieval.iter().position(|s| s.id == id) {
            self.total_tokens -= self.retrieval[pos].tokens;
            self.retrieval.remove(pos);
            return true;
        }
        if let Some(pos) = self.tool_outputs.iter().position(|s| s.id == id) {
            self.total_tokens -= self.tool_outputs[pos].tokens;
            self.tool_outputs.remove(pos);
            return true;
        }
        false
    }

    /// Internal: compact enough to fit `needed` additional tokens.
    fn compact_to_fit(&mut self, needed: usize) {
        let target = self.config.usable_tokens().saturating_sub(needed);

        while self.total_tokens > target {
            if let Some(seg) = self.tool_outputs.pop_front() {
                self.total_tokens -= seg.tokens;
            } else if let Some(seg) = self.retrieval.pop() {
                self.total_tokens -= seg.tokens;
            } else if let Some(seg) = self.history.pop_front() {
                self.total_tokens -= seg.tokens;
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(zone: ContextZone, tokens: usize, priority: u32, id: &str) -> ContextSegment {
        ContextSegment {
            zone,
            tokens,
            priority,
            id: id.to_string(),
            content: "x".repeat(tokens),
        }
    }

    #[test]
    fn basic_push_and_usage() {
        let config = ContextWindowConfig {
            max_context_tokens: 1000,
            response_budget: 100,
            ..Default::default()
        };
        let mut mgr = ContextWindowManager::new(config);

        assert!(mgr.push(seg(ContextZone::SystemPrompt, 50, u32::MAX, "sys")));
        assert!(mgr.push(seg(ContextZone::History, 200, 10, "h1")));
        assert!(mgr.push(seg(ContextZone::Retrieval, 100, 5, "r1")));

        let usage = mgr.usage();
        assert_eq!(usage.system_prompt_tokens, 50);
        assert_eq!(usage.history_tokens, 200);
        assert_eq!(usage.retrieval_tokens, 100);
        assert_eq!(usage.total_tokens, 350);
        assert_eq!(usage.usable_tokens, 900);
    }

    #[test]
    fn auto_compaction_on_push() {
        let config = ContextWindowConfig {
            max_context_tokens: 500,
            response_budget: 100,
            compaction_trigger: 0.8,
            ..Default::default()
        };
        let mut mgr = ContextWindowManager::new(config);

        // Fill to 350/400 usable tokens.
        assert!(mgr.push(seg(ContextZone::SystemPrompt, 50, u32::MAX, "sys")));
        assert!(mgr.push(seg(ContextZone::History, 100, 10, "h1")));
        assert!(mgr.push(seg(ContextZone::ToolOutput, 100, 1, "t1")));
        assert!(mgr.push(seg(ContextZone::ToolOutput, 100, 1, "t2")));

        // This push should trigger auto-compact to make room.
        assert!(mgr.push(seg(ContextZone::History, 100, 10, "h2")));

        // System prompt should never be evicted.
        assert_eq!(mgr.system.len(), 1);
    }

    #[test]
    fn explicit_compaction() {
        let config = ContextWindowConfig {
            max_context_tokens: 1000,
            response_budget: 100,
            compaction_trigger: 0.5,
            ..Default::default()
        };
        let mut mgr = ContextWindowManager::new(config);

        mgr.push(seg(ContextZone::SystemPrompt, 100, u32::MAX, "sys"));
        mgr.push(seg(ContextZone::ToolOutput, 200, 1, "t1"));
        mgr.push(seg(ContextZone::Retrieval, 150, 3, "r1"));
        mgr.push(seg(ContextZone::History, 100, 5, "h1"));

        assert!(mgr.needs_compaction()); // 550/900 > 0.5

        let result = mgr.compact();
        assert!(result.segments_removed > 0);
        assert!(result.tokens_after <= (900.0 * 0.5) as usize);
    }

    #[test]
    fn remove_by_id() {
        let config = ContextWindowConfig::default();
        let mut mgr = ContextWindowManager::new(config);

        mgr.push(seg(ContextZone::History, 100, 10, "h1"));
        mgr.push(seg(ContextZone::History, 200, 10, "h2"));

        assert!(mgr.remove("h1"));
        assert_eq!(mgr.total_tokens, 200);
        assert!(!mgr.remove("h1")); // Already removed.
    }

    #[test]
    fn reject_oversized_segment() {
        let config = ContextWindowConfig {
            max_context_tokens: 100,
            response_budget: 50,
            ..Default::default()
        };
        let mut mgr = ContextWindowManager::new(config);

        // 200 tokens > 50 usable, should be rejected.
        assert!(!mgr.push(seg(ContextZone::History, 200, 10, "big")));
    }

    #[test]
    fn render_order() {
        let config = ContextWindowConfig::default();
        let mut mgr = ContextWindowManager::new(config);

        mgr.push(seg(ContextZone::ToolOutput, 10, 1, "t1"));
        mgr.push(seg(ContextZone::SystemPrompt, 10, u32::MAX, "sys"));
        mgr.push(seg(ContextZone::History, 10, 5, "h1"));
        mgr.push(seg(ContextZone::Retrieval, 10, 3, "r1"));

        let rendered = mgr.render();
        assert_eq!(rendered[0].id, "sys");
        assert_eq!(rendered[1].id, "h1");
        assert_eq!(rendered[2].id, "r1");
        assert_eq!(rendered[3].id, "t1");
    }
}
