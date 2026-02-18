//! Heartbeat — periodic agent check-ins that only surface when relevant.
//!
//! Distinguished from cron jobs by two predicates:
//! 1. **Quiet check**: only fires if the agent hasn't messaged the user in Δ_quiet seconds
//! 2. **Relevance check**: only fires if there's something worth surfacing  
//!
//! ## Anti-Spam Design
//! Without the quiet check, heartbeats degenerate into cron spam.
//! Without the relevance check, heartbeats send "nothing to report" messages.
//!
//! ## Jitter
//! Fire time includes ±10% jitter to prevent thundering-herd when multiple agents
//! share a gateway: `actual_time = scheduled_time + Uniform(-J, +J)`.

use serde::{Deserialize, Serialize};
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::time::{Duration, Instant};
use tracing::{debug, info};

/// The sentinel string that agents return when there's nothing to report.
/// The gateway suppresses delivery when the response contains this.
pub const HEARTBEAT_SKIP: &str = "[HEARTBEAT_SKIP]";

/// Heartbeat configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatConfig {
    /// Base interval in seconds between heartbeat checks.
    pub interval_secs: u64,
    /// Minimum quiet period in seconds since the agent's last message.
    /// The heartbeat only fires if `now - last_message_time > quiet_secs`.
    pub quiet_secs: u64,
    /// Whether to apply jitter to the fire time (±10% of interval).
    pub jitter: bool,
    /// The system prompt fragment prepended to heartbeat agent turns.
    pub prompt: String,
    /// Whether this heartbeat is enabled.
    pub enabled: bool,
}

impl Default for HeartbeatConfig {
    fn default() -> Self {
        Self {
            interval_secs: 30 * 60, // 30 minutes
            quiet_secs: 15 * 60,    // 15 minutes quiet period
            jitter: true,
            prompt: "You are checking in. Summarize anything the user should know. \
                     If there is nothing important, respond exactly with [HEARTBEAT_SKIP]."
                .to_string(),
            enabled: true,
        }
    }
}

/// Session state snapshot for quiet-check evaluation.
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    /// When the agent last sent a message to the user.
    pub last_agent_message: Option<Instant>,
    /// When the user last sent a message to the agent.
    pub last_user_message: Option<Instant>,
    /// Number of pending items (reminders, tasks, etc.).
    pub pending_items: usize,
    /// Whether there are unread summaries.
    pub has_unread_summaries: bool,
}

impl Default for SessionSnapshot {
    fn default() -> Self {
        Self {
            last_agent_message: None,
            last_user_message: None,
            pending_items: 0,
            has_unread_summaries: false,
        }
    }
}

/// Result of heartbeat evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeartbeatDecision {
    /// Fire the heartbeat — both quiet and relevance checks passed.
    Fire,
    /// Skip: agent has been active recently (quiet check failed).
    SkipNotQuiet { last_message_ago_secs: u64 },
    /// Skip: nothing relevant to surface (relevance check failed).
    SkipNotRelevant,
    /// Skip: heartbeat is disabled.
    SkipDisabled,
}

/// Heartbeat evaluator — wraps a cron schedule with quiet + relevance predicates.
pub struct Heartbeat {
    config: HeartbeatConfig,
    last_fire: Option<Instant>,
    rng: RandomState,
    fire_count: u64,
}

impl Heartbeat {
    pub fn new(config: HeartbeatConfig) -> Self {
        Self {
            config,
            last_fire: None,
            rng: RandomState::new(),
            fire_count: 0,
        }
    }

    /// Evaluate whether the heartbeat should fire given current session state.
    pub fn evaluate(&self, session: &SessionSnapshot) -> HeartbeatDecision {
        if !self.config.enabled {
            return HeartbeatDecision::SkipDisabled;
        }

        // Quiet check: O(1) timestamp comparison.
        if let Some(last_msg) = session.last_agent_message {
            let elapsed = last_msg.elapsed().as_secs();
            if elapsed < self.config.quiet_secs {
                debug!(
                    elapsed_secs = elapsed,
                    quiet_threshold = self.config.quiet_secs,
                    "heartbeat: not quiet enough"
                );
                return HeartbeatDecision::SkipNotQuiet {
                    last_message_ago_secs: elapsed,
                };
            }
        }

        // Relevance check: are there pending items or unread summaries?
        if session.pending_items == 0 && !session.has_unread_summaries {
            debug!("heartbeat: nothing relevant to surface");
            return HeartbeatDecision::SkipNotRelevant;
        }

        HeartbeatDecision::Fire
    }

    /// Mark that the heartbeat has fired (for interval tracking).
    pub fn mark_fired(&mut self) {
        self.last_fire = Some(Instant::now());
        self.fire_count += 1;
        info!(count = self.fire_count, "heartbeat fired");
    }

    /// Check if enough time has elapsed since the last fire, with optional jitter.
    pub fn interval_elapsed(&self) -> bool {
        match self.last_fire {
            None => true, // Never fired → fire immediately
            Some(last) => {
                let base = Duration::from_secs(self.config.interval_secs);
                let jittered = if self.config.jitter {
                    self.apply_jitter(base)
                } else {
                    base
                };
                last.elapsed() >= jittered
            }
        }
    }

    /// Get the heartbeat prompt to use for the agent turn.
    pub fn prompt(&self) -> &str {
        &self.config.prompt
    }

    /// Check if a response should be suppressed (contains the skip sentinel).
    pub fn should_suppress(response: &str) -> bool {
        response.contains(HEARTBEAT_SKIP)
    }

    /// Get number of times this heartbeat has fired.
    pub fn fire_count(&self) -> u64 {
        self.fire_count
    }

    /// Get the heartbeat configuration.
    pub fn config(&self) -> &HeartbeatConfig {
        &self.config
    }

    /// Apply ±10% jitter to a duration to prevent thundering-herd.
    fn apply_jitter(&self, base: Duration) -> Duration {
        let jitter_range = base.as_millis() as u64 / 10; // 10% of base
        if jitter_range == 0 {
            return base;
        }

        let mut hasher = self.rng.build_hasher();
        hasher.write_u64(self.fire_count);
        hasher.write_u64(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64,
        );
        let hash = hasher.finish();
        let offset = (hash % (jitter_range * 2)) as i64 - jitter_range as i64;
        let jittered_ms = (base.as_millis() as i64 + offset).max(0) as u64;

        Duration::from_millis(jittered_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fire_when_quiet_and_relevant() {
        let hb = Heartbeat::new(HeartbeatConfig {
            quiet_secs: 1,
            ..Default::default()
        });
        let session = SessionSnapshot {
            last_agent_message: Some(Instant::now() - Duration::from_secs(60)),
            pending_items: 3,
            ..Default::default()
        };
        assert_eq!(hb.evaluate(&session), HeartbeatDecision::Fire);
    }

    #[test]
    fn skip_when_not_quiet() {
        let hb = Heartbeat::new(HeartbeatConfig {
            quiet_secs: 600, // 10 minutes
            ..Default::default()
        });
        let session = SessionSnapshot {
            last_agent_message: Some(Instant::now()), // just now
            pending_items: 5,
            ..Default::default()
        };
        match hb.evaluate(&session) {
            HeartbeatDecision::SkipNotQuiet { .. } => {} // expected
            other => panic!("expected SkipNotQuiet, got {:?}", other),
        }
    }

    #[test]
    fn skip_when_not_relevant() {
        let hb = Heartbeat::new(HeartbeatConfig {
            quiet_secs: 1,
            ..Default::default()
        });
        let session = SessionSnapshot {
            last_agent_message: Some(Instant::now() - Duration::from_secs(600)),
            pending_items: 0,
            has_unread_summaries: false,
            ..Default::default()
        };
        assert_eq!(hb.evaluate(&session), HeartbeatDecision::SkipNotRelevant);
    }

    #[test]
    fn skip_when_disabled() {
        let hb = Heartbeat::new(HeartbeatConfig {
            enabled: false,
            ..Default::default()
        });
        let session = SessionSnapshot {
            pending_items: 10,
            ..Default::default()
        };
        assert_eq!(hb.evaluate(&session), HeartbeatDecision::SkipDisabled);
    }

    #[test]
    fn suppress_skip_sentinel() {
        assert!(Heartbeat::should_suppress(
            "I checked but [HEARTBEAT_SKIP] nothing to report."
        ));
        assert!(!Heartbeat::should_suppress("Here are your tasks for today."));
    }

    #[test]
    fn interval_elapsed_when_never_fired() {
        let hb = Heartbeat::new(HeartbeatConfig::default());
        assert!(hb.interval_elapsed());
    }

    #[test]
    fn mark_fired_increments_count() {
        let mut hb = Heartbeat::new(HeartbeatConfig::default());
        assert_eq!(hb.fire_count(), 0);
        hb.mark_fired();
        assert_eq!(hb.fire_count(), 1);
        hb.mark_fired();
        assert_eq!(hb.fire_count(), 2);
    }

    #[test]
    fn fire_on_first_heartbeat_with_no_prior_messages() {
        let hb = Heartbeat::new(HeartbeatConfig {
            quiet_secs: 600,
            ..Default::default()
        });
        let session = SessionSnapshot {
            last_agent_message: None, // never messaged
            pending_items: 1,
            ..Default::default()
        };
        assert_eq!(hb.evaluate(&session), HeartbeatDecision::Fire);
    }
}
