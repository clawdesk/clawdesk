//! # Heartbeat-Driven Scheduling
//!
//! Extends ClawDesk's existing cron system with event-driven wake triggers.
//!
//! Standard cron is time-only: "run at 8 AM every day."
//! Heartbeat-driven scheduling adds: "run when event X happens."
//!
//! ## Wake Triggers
//!
//! - **on_event**: Wake when a system event matches a pattern
//! - **on_idle**: Wake after N seconds of user inactivity
//! - **on_file_change**: Wake when a watched file changes
//! - **on_agent_complete**: Wake when another agent finishes
//!
//! These compose with cron schedules:
//! - Time-only: `0 8 * * *` (run at 8 AM)
//! - Event-only: `on_event:cron:daily-brief` (run when daily-brief fires)
//! - Time + event: `0 8 * * * && on_idle:300` (8 AM, only if user was idle 5 min)

use serde::{Deserialize, Serialize};

/// A wake trigger — extends cron schedules with event-driven firing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WakeTrigger {
    /// Wake on a system event matching the pattern.
    OnEvent {
        pattern: String,
    },
    /// Wake after N seconds of user inactivity.
    OnIdle {
        idle_seconds: u64,
    },
    /// Wake when a file at the given path changes.
    OnFileChange {
        path: String,
    },
    /// Wake when a specific agent's run completes.
    OnAgentComplete {
        agent_id: String,
    },
    /// Compound: all triggers must be true.
    All(Vec<WakeTrigger>),
    /// Compound: any trigger fires.
    Any(Vec<WakeTrigger>),
}

/// Extended schedule: cron expression + optional wake triggers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatSchedule {
    /// Standard cron expression (None = event-only).
    pub cron: Option<String>,
    /// Additional wake triggers.
    pub triggers: Vec<WakeTrigger>,
    /// Active hours window (24h format). None = always active.
    pub active_hours: Option<ActiveHours>,
    /// Stagger offset to prevent thundering herd (seconds).
    pub stagger_secs: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActiveHours {
    /// Start hour (0-23).
    pub start_hour: u8,
    /// End hour (0-23, exclusive). If end < start, wraps around midnight.
    pub end_hour: u8,
    /// Timezone (e.g., "America/Los_Angeles"). Default: local.
    pub timezone: Option<String>,
}

impl HeartbeatSchedule {
    /// Is this schedule eligible to fire at the given hour?
    pub fn is_within_active_hours(&self, current_hour: u8) -> bool {
        match &self.active_hours {
            None => true,
            Some(ah) => {
                if ah.start_hour <= ah.end_hour {
                    // Normal range: e.g., 8-22
                    current_hour >= ah.start_hour && current_hour < ah.end_hour
                } else {
                    // Wraps midnight: e.g., 22-6
                    current_hour >= ah.start_hour || current_hour < ah.end_hour
                }
            }
        }
    }
}

/// Check if a wake trigger is satisfied given current state.
pub fn is_trigger_satisfied(
    trigger: &WakeTrigger,
    pending_events: &[String],
    idle_seconds: u64,
    changed_files: &[String],
    completed_agents: &[String],
) -> bool {
    match trigger {
        WakeTrigger::OnEvent { pattern } => {
            pending_events.iter().any(|e| e.contains(pattern.as_str()))
        }
        WakeTrigger::OnIdle { idle_seconds: threshold } => {
            idle_seconds >= *threshold
        }
        WakeTrigger::OnFileChange { path } => {
            changed_files.iter().any(|f| f == path)
        }
        WakeTrigger::OnAgentComplete { agent_id } => {
            completed_agents.iter().any(|a| a == agent_id)
        }
        WakeTrigger::All(triggers) => {
            triggers.iter().all(|t| is_trigger_satisfied(t, pending_events, idle_seconds, changed_files, completed_agents))
        }
        WakeTrigger::Any(triggers) => {
            triggers.iter().any(|t| is_trigger_satisfied(t, pending_events, idle_seconds, changed_files, completed_agents))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_active_hours_normal() {
        let sched = HeartbeatSchedule {
            cron: None,
            triggers: vec![],
            active_hours: Some(ActiveHours { start_hour: 8, end_hour: 22, timezone: None }),
            stagger_secs: 0,
        };
        assert!(sched.is_within_active_hours(12));
        assert!(!sched.is_within_active_hours(3));
        assert!(!sched.is_within_active_hours(23));
    }

    #[test]
    fn test_active_hours_midnight_wrap() {
        let sched = HeartbeatSchedule {
            cron: None,
            triggers: vec![],
            active_hours: Some(ActiveHours { start_hour: 22, end_hour: 6, timezone: None }),
            stagger_secs: 0,
        };
        assert!(sched.is_within_active_hours(23));
        assert!(sched.is_within_active_hours(0));
        assert!(sched.is_within_active_hours(3));
        assert!(!sched.is_within_active_hours(12));
    }

    #[test]
    fn test_event_trigger() {
        let trigger = WakeTrigger::OnEvent { pattern: "cron:daily".into() };
        assert!(is_trigger_satisfied(&trigger, &["cron:daily-brief completed".into()], 0, &[], &[]));
        assert!(!is_trigger_satisfied(&trigger, &["file changed".into()], 0, &[], &[]));
    }

    #[test]
    fn test_idle_trigger() {
        let trigger = WakeTrigger::OnIdle { idle_seconds: 300 };
        assert!(is_trigger_satisfied(&trigger, &[], 600, &[], &[]));
        assert!(!is_trigger_satisfied(&trigger, &[], 100, &[], &[]));
    }

    #[test]
    fn test_compound_all() {
        let trigger = WakeTrigger::All(vec![
            WakeTrigger::OnIdle { idle_seconds: 60 },
            WakeTrigger::OnEvent { pattern: "ready".into() },
        ]);
        // Both must be true
        assert!(is_trigger_satisfied(&trigger, &["ready".into()], 120, &[], &[]));
        assert!(!is_trigger_satisfied(&trigger, &["ready".into()], 30, &[], &[]));
        assert!(!is_trigger_satisfied(&trigger, &[], 120, &[], &[]));
    }
}
