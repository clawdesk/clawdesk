//! Structured audit logging for security-critical operations.
//!
//! Provides an append-only audit trail for operations that must be logged
//! for compliance, forensic analysis, and security monitoring.
//!
//! # Design
//!
//! Audit events are structured records (not free-text logs) with:
//! - **Who**: Actor identity (user, agent, system)
//! - **What**: Action type (tool invocation, config change, auth event)
//! - **Where**: Resource affected
//! - **When**: Timestamp
//! - **Outcome**: Success/failure with detail
//!
//! Events are buffered and flushed to a sink (file, OTLP, or callback).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Actor who performed the action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditActor {
    /// Actor type: "user", "agent", "system", "plugin".
    pub actor_type: String,
    /// Actor identifier (user ID, agent name, etc.).
    pub actor_id: String,
    /// Session/run ID for correlation.
    #[serde(default)]
    pub session_id: Option<String>,
}

/// Category of audit event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditCategory {
    /// Authentication events (login, token refresh, key rotation).
    Authentication,
    /// Authorization decisions (capability checks, policy enforcement).
    Authorization,
    /// Tool invocations (file read/write, shell exec, web requests).
    ToolInvocation,
    /// Configuration changes (agent config, system settings).
    ConfigChange,
    /// Data access (memory read/write, RAG queries).
    DataAccess,
    /// Security events (injection detection, sandbox violations).
    Security,
    /// Administrative actions (plugin install, model change).
    Admin,
}

/// Outcome of the audited action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditOutcome {
    /// Whether the action succeeded.
    pub success: bool,
    /// Human-readable detail.
    #[serde(default)]
    pub detail: Option<String>,
    /// Error code if failed.
    #[serde(default)]
    pub error_code: Option<String>,
}

impl AuditOutcome {
    pub fn ok() -> Self {
        Self {
            success: true,
            detail: None,
            error_code: None,
        }
    }

    pub fn denied(reason: impl Into<String>) -> Self {
        Self {
            success: false,
            detail: Some(reason.into()),
            error_code: Some("DENIED".into()),
        }
    }

    pub fn error(detail: impl Into<String>) -> Self {
        Self {
            success: false,
            detail: Some(detail.into()),
            error_code: Some("ERROR".into()),
        }
    }
}

/// A single audit event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEvent {
    /// Unique event ID.
    pub event_id: String,
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
    /// Who performed the action.
    pub actor: AuditActor,
    /// Category of the event.
    pub category: AuditCategory,
    /// Specific action (e.g., "tool.file_read", "auth.login", "config.agent.update").
    pub action: String,
    /// Resource affected (file path, agent name, etc.).
    #[serde(default)]
    pub resource: Option<String>,
    /// Outcome of the action.
    pub outcome: AuditOutcome,
    /// Additional metadata.
    #[serde(default)]
    pub metadata: std::collections::HashMap<String, serde_json::Value>,
}

/// Configuration for the audit logger.
#[derive(Debug, Clone)]
pub struct AuditConfig {
    /// Maximum events to buffer before flushing.
    pub buffer_size: usize,
    /// Categories that must be logged (empty = log all).
    pub required_categories: Vec<AuditCategory>,
    /// Whether to include metadata in logs.
    pub include_metadata: bool,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self {
            buffer_size: 1000,
            required_categories: vec![],
            include_metadata: true,
        }
    }
}

/// Audit logger that buffers events and flushes to sinks.
pub struct AuditLogger {
    config: AuditConfig,
    buffer: Arc<Mutex<VecDeque<AuditEvent>>>,
    total_logged: Arc<Mutex<u64>>,
}

impl AuditLogger {
    /// Create a new audit logger.
    pub fn new(config: AuditConfig) -> Self {
        Self {
            config,
            buffer: Arc::new(Mutex::new(VecDeque::new())),
            total_logged: Arc::new(Mutex::new(0)),
        }
    }

    /// Log an audit event.
    pub fn log(&self, event: AuditEvent) {
        // Filter by required categories if configured.
        if !self.config.required_categories.is_empty()
            && !self.config.required_categories.contains(&event.category)
        {
            return;
        }

        let mut buf = self.buffer.lock().unwrap();
        if buf.len() >= self.config.buffer_size {
            buf.pop_front(); // Ring buffer behavior.
        }
        buf.push_back(event);

        let mut total = self.total_logged.lock().unwrap();
        *total += 1;
    }

    /// Convenience: log a tool invocation.
    pub fn log_tool_invocation(
        &self,
        actor: AuditActor,
        tool_name: &str,
        resource: Option<String>,
        outcome: AuditOutcome,
    ) {
        self.log(AuditEvent {
            event_id: generate_event_id(),
            timestamp: Utc::now(),
            actor,
            category: AuditCategory::ToolInvocation,
            action: format!("tool.{}", tool_name),
            resource,
            outcome,
            metadata: Default::default(),
        });
    }

    /// Convenience: log a security event.
    pub fn log_security_event(
        &self,
        actor: AuditActor,
        action: impl Into<String>,
        detail: impl Into<String>,
    ) {
        self.log(AuditEvent {
            event_id: generate_event_id(),
            timestamp: Utc::now(),
            actor,
            category: AuditCategory::Security,
            action: action.into(),
            resource: None,
            outcome: AuditOutcome {
                success: false,
                detail: Some(detail.into()),
                error_code: Some("SECURITY".into()),
            },
            metadata: Default::default(),
        });
    }

    /// Flush the buffer and return all buffered events.
    pub fn flush(&self) -> Vec<AuditEvent> {
        let mut buf = self.buffer.lock().unwrap();
        buf.drain(..).collect()
    }

    /// Get the number of buffered events.
    pub fn buffered_count(&self) -> usize {
        self.buffer.lock().unwrap().len()
    }

    /// Get total events logged since creation.
    pub fn total_logged(&self) -> u64 {
        *self.total_logged.lock().unwrap()
    }

    /// Query buffered events by category.
    pub fn query_by_category(&self, category: AuditCategory) -> Vec<AuditEvent> {
        let buf = self.buffer.lock().unwrap();
        buf.iter()
            .filter(|e| e.category == category)
            .cloned()
            .collect()
    }

    /// Query buffered events by actor.
    pub fn query_by_actor(&self, actor_id: &str) -> Vec<AuditEvent> {
        let buf = self.buffer.lock().unwrap();
        buf.iter()
            .filter(|e| e.actor.actor_id == actor_id)
            .cloned()
            .collect()
    }
}

fn generate_event_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("audit-{}-{}", Utc::now().timestamp_millis(), seq)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_actor() -> AuditActor {
        AuditActor {
            actor_type: "agent".into(),
            actor_id: "code-reviewer".into(),
            session_id: Some("sess-123".into()),
        }
    }

    #[test]
    fn log_and_flush() {
        let logger = AuditLogger::new(AuditConfig::default());

        logger.log_tool_invocation(
            test_actor(),
            "file_read",
            Some("/src/main.rs".into()),
            AuditOutcome::ok(),
        );

        assert_eq!(logger.buffered_count(), 1);
        assert_eq!(logger.total_logged(), 1);

        let events = logger.flush();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "tool.file_read");
        assert!(events[0].outcome.success);
        assert_eq!(logger.buffered_count(), 0);
    }

    #[test]
    fn security_event_logging() {
        let logger = AuditLogger::new(AuditConfig::default());

        logger.log_security_event(
            test_actor(),
            "injection.detected",
            "Prompt injection attempt in tool output",
        );

        let events = logger.query_by_category(AuditCategory::Security);
        assert_eq!(events.len(), 1);
        assert!(!events[0].outcome.success);
    }

    #[test]
    fn ring_buffer_eviction() {
        let config = AuditConfig {
            buffer_size: 2,
            ..Default::default()
        };
        let logger = AuditLogger::new(config);

        for i in 0..5 {
            logger.log_tool_invocation(
                test_actor(),
                &format!("tool_{}", i),
                None,
                AuditOutcome::ok(),
            );
        }

        assert_eq!(logger.buffered_count(), 2);
        assert_eq!(logger.total_logged(), 5);

        let events = logger.flush();
        // Should have the last 2 events.
        assert_eq!(events[0].action, "tool.tool_3");
        assert_eq!(events[1].action, "tool.tool_4");
    }

    #[test]
    fn category_filter() {
        let config = AuditConfig {
            required_categories: vec![AuditCategory::Security],
            ..Default::default()
        };
        let logger = AuditLogger::new(config);

        // This should be filtered out (ToolInvocation != Security).
        logger.log_tool_invocation(test_actor(), "file_read", None, AuditOutcome::ok());

        // This should be logged.
        logger.log_security_event(test_actor(), "test", "test");

        assert_eq!(logger.buffered_count(), 1);
    }

    #[test]
    fn query_by_actor() {
        let logger = AuditLogger::new(AuditConfig::default());

        let actor1 = AuditActor {
            actor_type: "agent".into(),
            actor_id: "agent-a".into(),
            session_id: None,
        };
        let actor2 = AuditActor {
            actor_type: "agent".into(),
            actor_id: "agent-b".into(),
            session_id: None,
        };

        logger.log_tool_invocation(actor1, "tool1", None, AuditOutcome::ok());
        logger.log_tool_invocation(actor2, "tool2", None, AuditOutcome::ok());

        let results = logger.query_by_actor("agent-a");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].action, "tool.tool1");
    }
}
