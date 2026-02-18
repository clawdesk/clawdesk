//! Event subscriptions — pattern-based event filtering for pipeline triggers.

use crate::event::{EventKind, Priority};
use serde::{Deserialize, Serialize};

/// A subscription pattern that filters events for a pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    /// Unique subscription identifier
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Topic patterns to match (supports glob: "email.*", "social.*")
    pub topic_patterns: Vec<String>,
    /// Event kinds to match (empty = match all)
    pub event_kinds: Vec<EventKind>,
    /// Minimum priority to accept (None = accept all)
    pub min_priority: Option<Priority>,
    /// Pipeline ID to trigger when matched
    pub pipeline_id: String,
    /// Whether the subscription is active
    pub enabled: bool,
    /// Maximum batch size before flushing to pipeline
    pub batch_size: usize,
    /// Maximum wait time (seconds) before flushing a partial batch
    pub flush_interval_secs: u64,
}

impl Subscription {
    /// Check whether an event matches this subscription's filters.
    pub fn matches(&self, topic: &str, kind: &EventKind, priority: Priority) -> bool {
        if !self.enabled {
            return false;
        }

        // Check priority threshold
        if let Some(min_p) = self.min_priority {
            if (priority as u8) > (min_p as u8) {
                return false;
            }
        }

        // Check event kind filter
        if !self.event_kinds.is_empty() && !self.event_kinds.contains(kind) {
            return false;
        }

        // Check topic pattern (simple glob: * matches any segment)
        self.topic_patterns.iter().any(|pat| topic_matches(pat, topic))
    }
}

/// Simple glob matching: "*" matches any single segment, "**" not supported.
fn topic_matches(pattern: &str, topic: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    let pat_parts: Vec<&str> = pattern.split('.').collect();
    let top_parts: Vec<&str> = topic.split('.').collect();

    if pat_parts.len() != top_parts.len() {
        return false;
    }

    pat_parts
        .iter()
        .zip(top_parts.iter())
        .all(|(p, t)| *p == "*" || p == t)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matching() {
        assert!(topic_matches("email.*", "email.inbound"));
        assert!(topic_matches("email.*", "email.outbound"));
        assert!(!topic_matches("email.*", "social.metrics"));
        assert!(topic_matches("*", "anything"));
        assert!(topic_matches("social.metrics", "social.metrics"));
        assert!(!topic_matches("social.metrics", "social.alerts"));
    }
}
