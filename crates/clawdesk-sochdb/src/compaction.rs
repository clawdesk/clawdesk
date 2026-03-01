//! Cross-system conversation compaction — bridges ClawDesk's two-tier
//! retention model with the single-tier session storage.
//!
//! ## Compaction Port (P2)
//!
//! ClawDesk already has `compact_session()` in `conversation.rs` that
//! implements HOT_TIER_SIZE=200 compaction. This module adds:
//!
//! 1. **Compaction scheduling**: Policy-driven automatic compaction.
//! 2. **Cross-system sync**: Export compacted summaries for legacy consumption.
//! 3. **Compaction statistics**: Track compaction events for observability.
//!
//! ## Architecture
//!
//! ```text
//! Messages arrive → conversation.rs (append)
//!                        ↓
//!              CompactionScheduler (periodic check)
//!                        ↓ (if count > threshold)
//!              compact_session() → summary stored
//!                        ↓
//!              CompactionNotifier → emit event for legacy sync
//! ```

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::SochStore;
use clawdesk_types::session::SessionKey;

/// Default hot tier size — matches conversation.rs HOT_TIER_SIZE.
const DEFAULT_HOT_TIER: usize = 200;

/// Compaction policy for automatic session maintenance.
#[derive(Debug, Clone)]
pub struct CompactionPolicy {
    /// Hot tier threshold (compact when message count exceeds this).
    pub hot_tier_size: usize,
    /// Minimum interval between compactions for the same session.
    pub min_interval_secs: u64,
    /// Maximum summary length for the default summarizer.
    pub max_summary_chars: usize,
    /// Whether to emit notifications for cross-system sync.
    pub emit_notifications: bool,
}

impl Default for CompactionPolicy {
    fn default() -> Self {
        Self {
            hot_tier_size: DEFAULT_HOT_TIER,
            min_interval_secs: 300, // 5 minutes
            max_summary_chars: 2000,
            emit_notifications: true,
        }
    }
}

/// Record of a compaction event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactionEvent {
    /// Session that was compacted.
    pub session_id: String,
    /// Number of messages compacted.
    pub messages_compacted: usize,
    /// Number of messages remaining in hot tier.
    pub messages_remaining: usize,
    /// Summary length in characters.
    pub summary_chars: usize,
    /// When the compaction occurred.
    pub timestamp: DateTime<Utc>,
}

/// Aggregate compaction statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompactionStats {
    /// Total compaction events across all sessions.
    pub total_compactions: u64,
    /// Total messages compacted.
    pub total_messages_compacted: u64,
    /// Per-session compaction counts.
    pub per_session: HashMap<String, u64>,
    /// Last compaction time per session.
    pub last_compacted: HashMap<String, DateTime<Utc>>,
}

/// Notification sink for compaction events (cross-system sync).
pub trait CompactionNotifier: Send + Sync + 'static {
    /// Called after a compaction completes — allows the legacy system to receive
    /// the summary for its own session state.
    fn on_compaction(&self, event: &CompactionEvent, summary: &str);
}

/// No-op notifier for when cross-system sync is disabled.
struct NoopNotifier;
impl CompactionNotifier for NoopNotifier {
    fn on_compaction(&self, _event: &CompactionEvent, _summary: &str) {}
}

/// Compaction scheduler — manages policy-driven automatic compaction
/// across sessions.
pub struct CompactionScheduler {
    store: Arc<SochStore>,
    policy: CompactionPolicy,
    stats: Mutex<CompactionStats>,
    notifier: Box<dyn CompactionNotifier>,
}

impl CompactionScheduler {
    /// Create a scheduler with default policy and no cross-system notifier.
    pub fn new(store: Arc<SochStore>) -> Self {
        Self {
            store,
            policy: CompactionPolicy::default(),
            stats: Mutex::new(CompactionStats::default()),
            notifier: Box::new(NoopNotifier),
        }
    }

    /// Create a scheduler with a custom policy.
    pub fn with_policy(store: Arc<SochStore>, policy: CompactionPolicy) -> Self {
        Self {
            store,
            policy,
            stats: Mutex::new(CompactionStats::default()),
            notifier: Box::new(NoopNotifier),
        }
    }

    /// Set the notifier for cross-system compaction events.
    pub fn with_notifier(mut self, notifier: Box<dyn CompactionNotifier>) -> Self {
        self.notifier = notifier;
        self
    }

    /// Check a session and compact if needed.
    ///
    /// Returns `Some(CompactionEvent)` if compaction occurred, `None` otherwise.
    pub async fn maybe_compact(
        &self,
        key: &SessionKey,
    ) -> Result<Option<CompactionEvent>, String> {
        // Check cooldown
        {
            let stats = self.stats.lock().await;
            let sid = key.as_str();
            if let Some(last) = stats.last_compacted.get(&sid) {
                let elapsed = Utc::now()
                    .signed_duration_since(*last)
                    .num_seconds()
                    .unsigned_abs();
                if elapsed < self.policy.min_interval_secs {
                    debug!(
                        session = %key,
                        elapsed_secs = elapsed,
                        cooldown = self.policy.min_interval_secs,
                        "compaction cooldown active"
                    );
                    return Ok(None);
                }
            }
        }

        // Check message count
        let count = self
            .store
            .message_count(key)
            .await
            .map_err(|e| e.to_string())?;

        if count <= self.policy.hot_tier_size {
            return Ok(None);
        }

        // Run compaction
        let compacted = self
            .store
            .compact_session(key, None)
            .await
            .map_err(|e| e.to_string())?;

        if compacted == 0 {
            return Ok(None);
        }

        let remaining = count - compacted;
        let event = CompactionEvent {
            session_id: key.as_str().to_string(),
            messages_compacted: compacted,
            messages_remaining: remaining,
            summary_chars: self.policy.max_summary_chars.min(compacted * 50), // estimate
            timestamp: Utc::now(),
        };

        // Update stats
        {
            let mut stats = self.stats.lock().await;
            stats.total_compactions += 1;
            stats.total_messages_compacted += compacted as u64;
            *stats
                .per_session
                .entry(key.as_str().to_string())
                .or_insert(0) += 1;
            stats
                .last_compacted
                .insert(key.as_str().to_string(), event.timestamp);
        }

        // Notify cross-system consumers
        if self.policy.emit_notifications {
            let summary_text = format!(
                "[Compacted {} messages from session {}]",
                compacted,
                key.as_str()
            );
            self.notifier.on_compaction(&event, &summary_text);
        }

        info!(
            session = %key,
            compacted,
            remaining,
            "session compacted"
        );

        Ok(Some(event))
    }

    /// Run compaction check across multiple sessions.
    pub async fn compact_batch(
        &self,
        sessions: &[SessionKey],
    ) -> Vec<CompactionEvent> {
        let mut events = Vec::new();
        for key in sessions {
            match self.maybe_compact(key).await {
                Ok(Some(event)) => events.push(event),
                Ok(None) => {}
                Err(e) => {
                    warn!(session = %key, error = %e, "compaction failed");
                }
            }
        }
        events
    }

    /// Get compaction statistics.
    pub async fn stats(&self) -> CompactionStats {
        self.stats.lock().await.clone()
    }

    /// Get the configured policy.
    pub fn policy(&self) -> &CompactionPolicy {
        &self.policy
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingNotifier {
        count: AtomicUsize,
    }

    impl CountingNotifier {
        fn new() -> Self {
            Self {
                count: AtomicUsize::new(0),
            }
        }

        fn count(&self) -> usize {
            self.count.load(Ordering::SeqCst)
        }
    }

    impl CompactionNotifier for CountingNotifier {
        fn on_compaction(&self, _event: &CompactionEvent, _summary: &str) {
            self.count.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn default_policy_matches_hot_tier() {
        let policy = CompactionPolicy::default();
        assert_eq!(policy.hot_tier_size, DEFAULT_HOT_TIER);
        assert_eq!(policy.hot_tier_size, 200);
    }

    #[test]
    fn compaction_event_serializable() {
        let event = CompactionEvent {
            session_id: "test-session".into(),
            messages_compacted: 150,
            messages_remaining: 200,
            summary_chars: 1500,
            timestamp: Utc::now(),
        };
        let json = serde_json::to_string(&event).unwrap();
        let parsed: CompactionEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.messages_compacted, 150);
        assert_eq!(parsed.messages_remaining, 200);
    }

    #[test]
    fn stats_track_per_session() {
        let mut stats = CompactionStats::default();
        stats.total_compactions = 3;
        stats.total_messages_compacted = 450;
        stats.per_session.insert("s1".into(), 2);
        stats.per_session.insert("s2".into(), 1);
        assert_eq!(stats.per_session.len(), 2);
        assert_eq!(*stats.per_session.get("s1").unwrap(), 2);
    }

    #[test]
    fn custom_policy() {
        let policy = CompactionPolicy {
            hot_tier_size: 100,
            min_interval_secs: 60,
            max_summary_chars: 1000,
            emit_notifications: false,
        };
        assert_eq!(policy.hot_tier_size, 100);
        assert!(!policy.emit_notifications);
    }

    #[test]
    fn counting_notifier_works() {
        let notifier = CountingNotifier::new();
        let event = CompactionEvent {
            session_id: "s1".into(),
            messages_compacted: 10,
            messages_remaining: 200,
            summary_chars: 500,
            timestamp: Utc::now(),
        };
        notifier.on_compaction(&event, "summary");
        notifier.on_compaction(&event, "summary 2");
        assert_eq!(notifier.count(), 2);
    }
}
