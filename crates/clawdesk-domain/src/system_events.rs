//! # System Events — ephemeral per-session context injection
//!
//! A lightweight in-memory queue of events that get injected into the next
//! agent prompt.
//! queue of events that get injected into the next agent prompt.
//!
//! Unlike the EventBus (which is for pub/sub dispatch), system events
//! are specifically for **prompt injection** — they accumulate between
//! turns and get flushed into the system prompt on the next LLM call.
//!
//! Use cases:
//! - Cron job completed → inject result summary into next prompt
//! - Shell command finished → inject exit code + output snippet
//! - File changed on disk → inject notification
//! - External agent finished → inject result
//!
//! Events are ephemeral (no persistence) and session-scoped.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};

const MAX_EVENTS_PER_SESSION: usize = 20;

/// A system event that will be injected into the next prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemEvent {
    pub text: String,
    pub timestamp: i64,
    /// Optional context key for deduplication (e.g., "cron:daily-brief").
    pub context_key: Option<String>,
    /// Source of the event.
    pub source: EventSource,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EventSource {
    Cron { job_id: String },
    Shell { exit_code: i32 },
    FileWatch { path: String },
    ExternalAgent { agent_id: String },
    System { subsystem: String },
}

/// Per-session event queue.
struct SessionQueue {
    events: VecDeque<SystemEvent>,
    /// Last event text (for dedup).
    last_text: Option<String>,
    /// Last context key (for dedup).
    last_context_key: Option<String>,
}

impl SessionQueue {
    fn new() -> Self {
        Self {
            events: VecDeque::new(),
            last_text: None,
            last_context_key: None,
        }
    }
}

/// Manages system events across all sessions.
pub struct SystemEventQueue {
    queues: HashMap<String, SessionQueue>,
}

impl SystemEventQueue {
    pub fn new() -> Self {
        Self {
            queues: HashMap::new(),
        }
    }

    /// Push an event to a session's queue.
    /// Deduplicates against the last event (same text or same context_key).
    pub fn push(&mut self, session_key: &str, event: SystemEvent) {
        let queue = self
            .queues
            .entry(session_key.to_string())
            .or_insert_with(SessionQueue::new);

        // Dedup: skip if identical to last event
        if let Some(ref last) = queue.last_text {
            if *last == event.text {
                return;
            }
        }
        if let (Some(ref key), Some(ref last_key)) = (&event.context_key, &queue.last_context_key) {
            if key == last_key {
                return;
            }
        }

        queue.last_text = Some(event.text.clone());
        queue.last_context_key = event.context_key.clone();
        queue.events.push_back(event);

        // Cap the queue
        while queue.events.len() > MAX_EVENTS_PER_SESSION {
            queue.events.pop_front();
        }
    }

    /// Drain all events for a session and format them as a prompt injection.
    /// This is called before each LLM turn to inject pending events.
    pub fn drain_as_prompt(&mut self, session_key: &str) -> Option<String> {
        let queue = self.queues.get_mut(session_key)?;
        if queue.events.is_empty() {
            return None;
        }

        let events: Vec<SystemEvent> = queue.events.drain(..).collect();
        let lines: Vec<String> = events
            .iter()
            .map(|e| {
                let source = match &e.source {
                    EventSource::Cron { job_id } => format!("[cron:{}]", job_id),
                    EventSource::Shell { exit_code } => format!("[shell:exit={}]", exit_code),
                    EventSource::FileWatch { path } => format!("[file:{}]", path),
                    EventSource::ExternalAgent { agent_id } => format!("[agent:{}]", agent_id),
                    EventSource::System { subsystem } => format!("[system:{}]", subsystem),
                };
                format!("{} {}", source, e.text)
            })
            .collect();

        Some(format!(
            "<system_events count=\"{}\">\n{}\n</system_events>",
            lines.len(),
            lines.join("\n")
        ))
    }

    /// Check if a session has pending events.
    pub fn has_pending(&self, session_key: &str) -> bool {
        self.queues
            .get(session_key)
            .map(|q| !q.events.is_empty())
            .unwrap_or(false)
    }

    /// Clear all events for a session (e.g., on session close).
    pub fn clear_session(&mut self, session_key: &str) {
        self.queues.remove(session_key);
    }

    /// Number of pending events across all sessions.
    pub fn total_pending(&self) -> usize {
        self.queues.values().map(|q| q.events.len()).sum()
    }
}

impl Default for SystemEventQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_push_and_drain() {
        let mut q = SystemEventQueue::new();
        q.push("s1", SystemEvent {
            text: "Cron job 'daily-brief' completed".into(),
            timestamp: 1234567890,
            context_key: Some("cron:daily-brief".into()),
            source: EventSource::Cron { job_id: "daily-brief".into() },
        });
        q.push("s1", SystemEvent {
            text: "File package.json changed".into(),
            timestamp: 1234567891,
            context_key: None,
            source: EventSource::FileWatch { path: "package.json".into() },
        });

        assert!(q.has_pending("s1"));
        let prompt = q.drain_as_prompt("s1").unwrap();
        assert!(prompt.contains("system_events"));
        assert!(prompt.contains("cron:daily-brief"));
        assert!(prompt.contains("package.json"));
        assert!(!q.has_pending("s1"));
    }

    #[test]
    fn test_dedup() {
        let mut q = SystemEventQueue::new();
        let event = SystemEvent {
            text: "same event".into(),
            timestamp: 1,
            context_key: None,
            source: EventSource::System { subsystem: "test".into() },
        };
        q.push("s1", event.clone());
        q.push("s1", event.clone()); // should be deduped
        assert_eq!(q.queues.get("s1").unwrap().events.len(), 1);
    }

    #[test]
    fn test_cap() {
        let mut q = SystemEventQueue::new();
        for i in 0..30 {
            q.push("s1", SystemEvent {
                text: format!("event {}", i),
                timestamp: i,
                context_key: None,
                source: EventSource::System { subsystem: "test".into() },
            });
        }
        assert_eq!(q.queues.get("s1").unwrap().events.len(), MAX_EVENTS_PER_SESSION);
    }
}
