//! Email as a Proactive Tool — dedicated `email_send` builtin.
//!
//! Unlike the `MessageSendTool` (which sends to connected channels), this tool
//! lets the agent compose and send emails to arbitrary addresses. Wraps existing
//! SMTP infrastructure from `clawdesk-channels::email` but exposes it with proper
//! parameters: to, subject, body, cc, bcc, reply_to.
//!
//! Permission: classified as `dangerous_tools` (sending email is irreversible).
//! The approval prompt shows the full email preview before sending.

use crate::tools::{Tool, ToolCapability, ToolSchema};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Configuration for the SMTP backend used by EmailSendTool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailSmtpConfig {
    /// SMTP server hostname.
    pub host: String,
    /// SMTP server port (587 for STARTTLS, 465 for implicit TLS).
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    /// SMTP username.
    pub username: String,
    /// SMTP password.
    pub password: String,
    /// Whether to use TLS.
    #[serde(default = "default_true")]
    pub use_tls: bool,
    /// Sender email address.
    pub from_address: String,
    /// Sender display name.
    #[serde(default = "default_from_name")]
    pub from_name: String,
}

fn default_smtp_port() -> u16 { 587 }
fn default_true() -> bool { true }
fn default_from_name() -> String { "ClawDesk Agent".to_string() }

/// Parsed parameters for a single email send request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailSendParams {
    /// Primary recipient(s), comma-separated.
    pub to: String,
    /// Email subject line.
    pub subject: String,
    /// Email body (plain text or markdown).
    pub body: String,
    /// CC recipients (optional, comma-separated).
    #[serde(default)]
    pub cc: Option<String>,
    /// BCC recipients (optional, comma-separated).
    #[serde(default)]
    pub bcc: Option<String>,
    /// Message-ID to reply to (for email threading).
    #[serde(default)]
    pub reply_to: Option<String>,
}

/// Proactive email sending tool.
///
/// The tool uses a callback pattern matching `MessageSendTool` — the actual
/// SMTP send is wired by the gateway/CLI layer, keeping the tool independent
/// of transport details.
pub struct EmailSendTool {
    /// Async callback that performs the actual SMTP send.
    /// Parameters: (to, subject, body, cc, bcc, reply_to) → Ok(message_id) or Err(error).
    send_fn: Arc<
        dyn Fn(
                EmailSendParams,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
}

impl EmailSendTool {
    /// Create a new EmailSendTool with a send callback.
    pub fn new(
        send_fn: Arc<
            dyn Fn(
                    EmailSendParams,
                )
                    -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
                + Send
                + Sync,
        >,
    ) -> Self {
        Self { send_fn }
    }

    /// Create a tool that logs emails but doesn't actually send them (dry-run mode).
    pub fn dry_run() -> Self {
        Self {
            send_fn: Arc::new(|params: EmailSendParams| {
                Box::pin(async move {
                    info!(
                        to = %params.to,
                        subject = %params.subject,
                        body_len = params.body.len(),
                        "email_send (dry run) — would send email"
                    );
                    Ok(format!("<dry-run-{}>", uuid::Uuid::new_v4()))
                })
            }),
        }
    }
}

#[async_trait]
impl Tool for EmailSendTool {
    fn name(&self) -> &str {
        "email_send"
    }

    fn schema(&self) -> ToolSchema {
        ToolSchema {
            name: "email_send".to_string(),
            description: "Send an email to one or more recipients. Use this when the user \
                          asks you to send, compose, or email someone. The email is delivered \
                          immediately via SMTP."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "to": {
                        "type": "string",
                        "description": "Recipient email address(es), comma-separated for multiple."
                    },
                    "subject": {
                        "type": "string",
                        "description": "Email subject line."
                    },
                    "body": {
                        "type": "string",
                        "description": "Email body text. Can include markdown formatting."
                    },
                    "cc": {
                        "type": "string",
                        "description": "CC recipient(s), comma-separated. Optional."
                    },
                    "bcc": {
                        "type": "string",
                        "description": "BCC recipient(s), comma-separated. Optional."
                    },
                    "reply_to": {
                        "type": "string",
                        "description": "Message-ID to reply to (for email threading). Optional."
                    }
                },
                "required": ["to", "subject", "body"]
            }),
        }
    }

    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![ToolCapability::Messaging, ToolCapability::Network]
    }

    async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
        let params: EmailSendParams = serde_json::from_value(args.clone())
            .map_err(|e| format!("invalid email_send parameters: {e}"))?;

        // Validate email addresses (basic check)
        if params.to.is_empty() {
            return Err("'to' field is required".to_string());
        }
        if params.subject.is_empty() {
            return Err("'subject' field is required".to_string());
        }

        // Validate basic email format
        for addr in params.to.split(',').map(|s| s.trim()) {
            if !addr.contains('@') || !addr.contains('.') {
                return Err(format!("invalid email address: '{}'", addr));
            }
        }

        if let Some(ref cc) = params.cc {
            for addr in cc.split(',').map(|s| s.trim()) {
                if !addr.is_empty() && (!addr.contains('@') || !addr.contains('.')) {
                    return Err(format!("invalid CC email address: '{}'", addr));
                }
            }
        }

        debug!(
            to = %params.to,
            subject = %params.subject,
            body_len = params.body.len(),
            "email_send: sending"
        );

        let message_id = (self.send_fn)(params.clone()).await?;

        info!(
            to = %params.to,
            subject = %params.subject,
            message_id = %message_id,
            "email sent successfully"
        );

        Ok(serde_json::json!({
            "status": "sent",
            "message_id": message_id,
            "to": params.to,
            "subject": params.subject,
        })
        .to_string())
    }
}

/// Register the email_send tool in the tool registry.
///
/// Called by the gateway/CLI layer with the appropriate send callback wired
/// to the SMTP backend.
pub fn register_email_tool(
    registry: &mut crate::tools::ToolRegistry,
    send_fn: Arc<
        dyn Fn(
                EmailSendParams,
            )
                -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send>>
            + Send
            + Sync,
    >,
) {
    registry.register(Arc::new(EmailSendTool::new(send_fn)));
}

/// Register a dry-run email tool (logs but doesn't send).
pub fn register_email_tool_dry_run(registry: &mut crate::tools::ToolRegistry) {
    registry.register(Arc::new(EmailSendTool::dry_run()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn email_send_basic() {
        let call_count = Arc::new(AtomicUsize::new(0));
        let count = Arc::clone(&call_count);

        let tool = EmailSendTool::new(Arc::new(move |params: EmailSendParams| {
            let count = Arc::clone(&count);
            Box::pin(async move {
                count.fetch_add(1, Ordering::Relaxed);
                assert_eq!(params.to, "john@example.com");
                assert_eq!(params.subject, "Meeting update");
                Ok("<msg-123>".to_string())
            })
        }));

        let result = tool
            .execute(serde_json::json!({
                "to": "john@example.com",
                "subject": "Meeting update",
                "body": "Hello John, the meeting is at 3pm."
            }))
            .await
            .unwrap();

        assert!(result.contains("sent"));
        assert!(result.contains("msg-123"));
        assert_eq!(call_count.load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn email_send_invalid_address() {
        let tool = EmailSendTool::dry_run();
        let result = tool
            .execute(serde_json::json!({
                "to": "not-an-email",
                "subject": "Test",
                "body": "Body"
            }))
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid email address"));
    }

    #[tokio::test]
    async fn email_send_empty_to() {
        let tool = EmailSendTool::dry_run();
        let result = tool
            .execute(serde_json::json!({
                "to": "",
                "subject": "Test",
                "body": "Body"
            }))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn email_send_with_cc_bcc() {
        let tool = EmailSendTool::new(Arc::new(|params: EmailSendParams| {
            Box::pin(async move {
                assert!(params.cc.is_some());
                assert!(params.bcc.is_some());
                Ok("<msg-456>".to_string())
            })
        }));

        let result = tool
            .execute(serde_json::json!({
                "to": "alice@example.com",
                "subject": "Team update",
                "body": "Hi team.",
                "cc": "bob@example.com",
                "bcc": "manager@example.com"
            }))
            .await
            .unwrap();
        assert!(result.contains("sent"));
    }

    #[tokio::test]
    async fn email_send_dry_run() {
        let tool = EmailSendTool::dry_run();
        let result = tool
            .execute(serde_json::json!({
                "to": "test@example.com",
                "subject": "Dry run",
                "body": "This won't actually send."
            }))
            .await
            .unwrap();
        assert!(result.contains("sent"));
        assert!(result.contains("dry-run"));
    }

    #[test]
    fn email_tool_schema() {
        let tool = EmailSendTool::dry_run();
        assert_eq!(tool.name(), "email_send");
        let schema = tool.schema();
        assert!(schema.description.contains("email"));
        let required = schema.parameters["required"].as_array().unwrap();
        assert_eq!(required.len(), 3);
    }

    #[test]
    fn email_tool_capabilities() {
        let tool = EmailSendTool::dry_run();
        let caps = tool.required_capabilities();
        assert!(caps.contains(&ToolCapability::Messaging));
        assert!(caps.contains(&ToolCapability::Network));
    }
}
