//! Domain-specific audit facades for channel, filesystem, and tool policy events.
//!
//! These wrap the core `AuditLogger` with typed facades that ensure
//! consistent event structure for each security domain. All events
//! flow to the same hash-chained audit log.

use crate::audit::AuditLogger;
use clawdesk_types::security::{AuditActor, AuditCategory, AuditOutcome};
use serde_json::json;
use std::sync::Arc;

// ─────────────────────────────────────────────────────────────────────────────
// Channel Audit
// ─────────────────────────────────────────────────────────────────────────────

/// Audit facade for channel-level events.
pub struct ChannelAuditor {
    logger: Arc<AuditLogger>,
}

impl ChannelAuditor {
    pub fn new(logger: Arc<AuditLogger>) -> Self {
        Self { logger }
    }

    /// Log a message received from a channel.
    pub async fn log_message_received(
        &self,
        channel: &str,
        sender: &str,
        message_id: &str,
        allowed: bool,
    ) {
        self.logger
            .log(
                AuditCategory::MessageReceive,
                "channel.message_received",
                AuditActor::User {
                    sender_id: sender.to_string(),
                    channel: channel.to_string(),
                },
                Some(channel.to_string()),
                json!({
                    "message_id": message_id,
                    "channel": channel,
                    "allowed": allowed,
                }),
                if allowed {
                    AuditOutcome::Success
                } else {
                    AuditOutcome::Denied
                },
            )
            .await;
    }

    /// Log a DM policy decision.
    pub async fn log_dm_policy_decision(
        &self,
        channel: &str,
        user_id: &str,
        decision: &str,
        reason: &str,
    ) {
        self.logger
            .log(
                AuditCategory::AdminAction,
                "channel.dm_policy",
                AuditActor::System,
                Some(channel.to_string()),
                json!({
                    "user_id": user_id,
                    "decision": decision,
                    "reason": reason,
                }),
                if decision == "allow" {
                    AuditOutcome::Success
                } else {
                    AuditOutcome::Denied
                },
            )
            .await;
    }

    /// Log a message sent to a channel.
    pub async fn log_message_sent(
        &self,
        channel: &str,
        message_id: &str,
        success: bool,
        error: Option<&str>,
    ) {
        self.logger
            .log(
                AuditCategory::MessageSend,
                "channel.message_sent",
                AuditActor::Agent {
                    id: "default".to_string(),
                },
                Some(channel.to_string()),
                json!({
                    "message_id": message_id,
                    "success": success,
                    "error": error,
                }),
                if success {
                    AuditOutcome::Success
                } else {
                    AuditOutcome::Failed
                },
            )
            .await;
    }

    /// Log channel connection/disconnection events.
    pub async fn log_channel_lifecycle(&self, channel: &str, event: &str) {
        self.logger
            .log(
                AuditCategory::SessionLifecycle,
                &format!("channel.{event}"),
                AuditActor::System,
                Some(channel.to_string()),
                json!({ "channel": channel, "event": event }),
                AuditOutcome::Success,
            )
            .await;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Filesystem Audit
// ─────────────────────────────────────────────────────────────────────────────

/// Audit facade for filesystem operations.
pub struct FilesystemAuditor {
    logger: Arc<AuditLogger>,
}

impl FilesystemAuditor {
    pub fn new(logger: Arc<AuditLogger>) -> Self {
        Self { logger }
    }

    /// Log a file read operation.
    pub async fn log_read(
        &self,
        path: &str,
        actor: AuditActor,
        bytes_read: u64,
        allowed: bool,
    ) {
        self.logger
            .log(
                AuditCategory::FileAccess,
                "fs.read",
                actor,
                Some(path.to_string()),
                json!({
                    "path": path,
                    "bytes_read": bytes_read,
                    "allowed": allowed,
                }),
                if allowed {
                    AuditOutcome::Success
                } else {
                    AuditOutcome::Denied
                },
            )
            .await;
    }

    /// Log a file write operation.
    pub async fn log_write(
        &self,
        path: &str,
        actor: AuditActor,
        bytes_written: u64,
        allowed: bool,
    ) {
        self.logger
            .log(
                AuditCategory::FileAccess,
                "fs.write",
                actor,
                Some(path.to_string()),
                json!({
                    "path": path,
                    "bytes_written": bytes_written,
                    "allowed": allowed,
                }),
                if allowed {
                    AuditOutcome::Success
                } else {
                    AuditOutcome::Denied
                },
            )
            .await;
    }

    /// Log a file delete operation.
    pub async fn log_delete(&self, path: &str, actor: AuditActor, allowed: bool) {
        self.logger
            .log(
                AuditCategory::FileAccess,
                "fs.delete",
                actor,
                Some(path.to_string()),
                json!({
                    "path": path,
                    "allowed": allowed,
                }),
                if allowed {
                    AuditOutcome::Success
                } else {
                    AuditOutcome::Denied
                },
            )
            .await;
    }

    /// Log a path traversal attempt (blocked by sandbox).
    pub async fn log_path_violation(&self, attempted_path: &str, actor: AuditActor) {
        self.logger
            .log(
                AuditCategory::SecurityAlert,
                "fs.path_violation",
                actor,
                Some(attempted_path.to_string()),
                json!({
                    "attempted_path": attempted_path,
                    "violation": "path_traversal",
                }),
                AuditOutcome::Denied,
            )
            .await;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tool Policy Audit
// ─────────────────────────────────────────────────────────────────────────────

/// Audit facade for tool invocation and policy decisions.
pub struct ToolPolicyAuditor {
    logger: Arc<AuditLogger>,
}

impl ToolPolicyAuditor {
    pub fn new(logger: Arc<AuditLogger>) -> Self {
        Self { logger }
    }

    /// Log a tool invocation decision.
    pub async fn log_tool_invocation(
        &self,
        tool_name: &str,
        agent: &str,
        approved: bool,
        reason: &str,
        required_approval: bool,
    ) {
        self.logger
            .log(
                AuditCategory::ToolExecution,
                "tool.invocation",
                AuditActor::Agent {
                    id: agent.to_string(),
                },
                Some(tool_name.to_string()),
                json!({
                    "tool": tool_name,
                    "approved": approved,
                    "reason": reason,
                    "required_approval": required_approval,
                }),
                if approved {
                    AuditOutcome::Success
                } else {
                    AuditOutcome::Denied
                },
            )
            .await;
    }

    /// Log a tool execution result.
    pub async fn log_tool_execution(
        &self,
        tool_name: &str,
        agent: &str,
        success: bool,
        duration_ms: u64,
        error: Option<&str>,
    ) {
        self.logger
            .log(
                AuditCategory::ToolExecution,
                "tool.execution",
                AuditActor::Agent {
                    id: agent.to_string(),
                },
                Some(tool_name.to_string()),
                json!({
                    "tool": tool_name,
                    "success": success,
                    "duration_ms": duration_ms,
                    "error": error,
                }),
                if success {
                    AuditOutcome::Success
                } else {
                    AuditOutcome::Failed
                },
            )
            .await;
    }

    /// Log a sandbox enforcement action.
    pub async fn log_sandbox_enforcement(
        &self,
        tool_name: &str,
        agent: &str,
        enforcement: &str,
        detail: &str,
    ) {
        self.logger
            .log(
                AuditCategory::SecurityAlert,
                "tool.sandbox_enforcement",
                AuditActor::Agent {
                    id: agent.to_string(),
                },
                Some(tool_name.to_string()),
                json!({
                    "tool": tool_name,
                    "enforcement": enforcement,
                    "detail": detail,
                }),
                AuditOutcome::Denied,
            )
            .await;
    }

    /// Log a policy override by admin.
    pub async fn log_policy_override(
        &self,
        tool_name: &str,
        admin_id: &str,
        old_policy: &str,
        new_policy: &str,
    ) {
        self.logger
            .log(
                AuditCategory::AdminAction,
                "tool.policy_override",
                AuditActor::User {
                    sender_id: admin_id.to_string(),
                    channel: "admin".to_string(),
                },
                Some(tool_name.to_string()),
                json!({
                    "tool": tool_name,
                    "old_policy": old_policy,
                    "new_policy": new_policy,
                }),
                AuditOutcome::Success,
            )
            .await;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::AuditLoggerConfig;

    fn test_logger() -> Arc<AuditLogger> {
        Arc::new(AuditLogger::new(AuditLoggerConfig::default()))
    }

    #[tokio::test]
    async fn channel_audit_message_received() {
        let logger = test_logger();
        let auditor = ChannelAuditor::new(logger.clone());

        auditor
            .log_message_received("slack-work", "user-123", "msg-456", true)
            .await;

        let recent = logger.recent(1).await;
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].action, "channel.message_received");
    }

    #[tokio::test]
    async fn fs_audit_path_violation() {
        let logger = test_logger();
        let auditor = FilesystemAuditor::new(logger.clone());

        auditor
            .log_path_violation(
                "/etc/passwd",
                AuditActor::Agent {
                    id: "coder".to_string(),
                },
            )
            .await;

        let recent = logger.recent(1).await;
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].action, "fs.path_violation");
    }

    #[tokio::test]
    async fn tool_audit_invocation() {
        let logger = test_logger();
        let auditor = ToolPolicyAuditor::new(logger.clone());

        auditor
            .log_tool_invocation("shell_exec", "coder", false, "high-risk command", true)
            .await;

        let recent = logger.recent(1).await;
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].action, "tool.invocation");
    }

    #[tokio::test]
    async fn combined_audit_trail() {
        let logger = test_logger();
        let ch = ChannelAuditor::new(logger.clone());
        let fs = FilesystemAuditor::new(logger.clone());
        let tool = ToolPolicyAuditor::new(logger.clone());

        ch.log_message_received("slack", "user1", "m1", true).await;
        fs.log_read(
            "/project/src/main.rs",
            AuditActor::Agent {
                id: "coder".to_string(),
            },
            1024,
            true,
        )
        .await;
        tool.log_tool_execution("file_read", "coder", true, 5, None)
            .await;

        let recent = logger.recent(3).await;
        assert_eq!(recent.len(), 3);
    }
}
