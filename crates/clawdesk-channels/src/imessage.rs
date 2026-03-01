//! iMessage channel adapter (macOS only).
//!
//! Sends messages via the macOS AppleScript bridge (`osascript`) and
//! receives inbound messages by polling the Messages.app SQLite database
//! at `~/Library/Messages/chat.db`.
//!
//! ## Security
//!
//! - **CWE-78 mitigation**: All strings interpolated into AppleScript are
//!   escaped via [`escape_applescript`] (backslashes, quotes, newlines).
//! - **Target validation**: [`is_valid_imessage_target`] rejects targets
//!   that don't look like phone numbers or email addresses before they
//!   reach the AppleScript layer.
//! - **Contact allowlist**: Only messages from `allowed_contacts` are
//!   forwarded to the agent. `"*"` matches everyone.
//!
//! ## Architecture
//!
//! ```text
//! IMessageChannel
//! ├── start(sink)       — poll loop: read chat.db → sink.on_message()
//! ├── send(msg)         — escape + osascript → Messages.app
//! ├── stop()            — flip AtomicBool
//! └── health check      — verify macOS + chat.db exists
//! ```
//!
//! ## Limitations
//!
//! - macOS only (AppleScript + Messages.app)
//! - Requires Full Disk Access for `~/Library/Messages/chat.db`
//! - Text-only (no media attachments from the DB query)
//! - Polling interval defaults to 3 seconds

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use chrono::Utc;
use directories::UserDirs;
use rusqlite::{Connection, OpenFlags};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Notify;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// iMessage channel — AppleScript bridge + Messages.app SQLite polling.
pub struct IMessageChannel {
    /// Contacts allowed to interact. `"*"` = everyone.
    allowed_contacts: Vec<String>,
    /// Polling interval in seconds (default: 3).
    poll_interval_secs: u64,
    /// Shutdown flag.
    running: AtomicBool,
    /// Shutdown notifier.
    shutdown: Notify,
}

impl IMessageChannel {
    pub fn new(allowed_contacts: Vec<String>, poll_interval_secs: u64) -> Self {
        Self {
            allowed_contacts,
            poll_interval_secs: if poll_interval_secs == 0 {
                3
            } else {
                poll_interval_secs
            },
            running: AtomicBool::new(false),
            shutdown: Notify::new(),
        }
    }

    /// Check if a sender is in the allowlist.
    fn is_contact_allowed(&self, sender: &str) -> bool {
        if self.allowed_contacts.iter().any(|u| u == "*") {
            return true;
        }
        self.allowed_contacts
            .iter()
            .any(|u| u.eq_ignore_ascii_case(sender))
    }
}

/// Escape a string for safe interpolation into AppleScript.
///
/// Prevents injection attacks by escaping:
/// - Backslashes (`\` → `\\`)
/// - Double quotes (`"` → `\"`)
/// - Newlines (`\n` → `\\n`, `\r` → `\\r`) to prevent code injection via line breaks
pub fn escape_applescript(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// Validate that a target looks like a valid phone number or email address.
///
/// Defense-in-depth measure to reject obviously malicious targets before
/// they reach AppleScript interpolation.
///
/// Valid patterns:
/// - Phone: starts with `+` followed by 7-15 digits (with optional spaces/dashes)
/// - Email: contains `@` with alphanumeric chars on both sides
pub fn is_valid_imessage_target(target: &str) -> bool {
    let target = target.trim();
    if target.is_empty() {
        return false;
    }

    // Phone number: +1234567890 or +1 234-567-8900
    if target.starts_with('+') {
        let digits_only: String = target.chars().filter(char::is_ascii_digit).collect();
        return digits_only.len() >= 7 && digits_only.len() <= 15;
    }

    // Email: simple validation
    if let Some(at_pos) = target.find('@') {
        let local = &target[..at_pos];
        let domain = &target[at_pos + 1..];

        let local_valid = !local.is_empty()
            && local
                .chars()
                .all(|c| c.is_alphanumeric() || "._+-".contains(c));

        let domain_valid = !domain.is_empty()
            && domain.contains('.')
            && domain
                .chars()
                .all(|c| c.is_alphanumeric() || ".-".contains(c));

        return local_valid && domain_valid;
    }

    false
}

#[async_trait]
impl Channel for IMessageChannel {
    fn id(&self) -> ChannelId {
        ChannelId::IMessage
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta::basic("iMessage")
    }

    async fn start(&self, sink: Arc<dyn MessageSink>) -> Result<(), String> {
        self.running.store(true, Ordering::SeqCst);
        info!("iMessage channel starting (AppleScript bridge, poll={}s)...", self.poll_interval_secs);

        // Locate the Messages database
        let db_path = match UserDirs::new() {
            Some(dirs) => dirs.home_dir().join("Library/Messages/chat.db"),
            None => {
                warn!("iMessage: cannot determine home directory");
                return Err("Cannot determine home directory".to_string());
            }
        };

        if !db_path.exists() {
            let msg = format!(
                "iMessage: Messages database not found at {}. \
                 Ensure Messages.app is set up and Full Disk Access is granted.",
                db_path.display()
            );
            warn!("{msg}");
            return Err(msg);
        }

        // Open a persistent read-only connection
        let path = db_path.to_path_buf();
        let conn = match tokio::task::spawn_blocking(move || {
            Connection::open_with_flags(
                &path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
        })
        .await
        {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                warn!("iMessage: failed to open chat.db: {e}");
                return Err(format!("Failed to open chat.db: {e}"));
            }
            Err(e) => {
                warn!("iMessage: spawn_blocking join error: {e}");
                return Err(format!("spawn_blocking join error: {e}"));
            }
        };

        // Get the initial max ROWID
        let (mut conn, initial_rowid) = match tokio::task::spawn_blocking(move || {
            let rowid: i64 = conn
                .query_row(
                    "SELECT COALESCE(MAX(ROWID), 0) FROM message WHERE is_from_me = 0",
                    [],
                    |row| row.get(0),
                )
                .unwrap_or(0);
            (conn, rowid)
        })
        .await
        {
            Ok(pair) => pair,
            Err(e) => {
                warn!("iMessage: failed to get initial rowid: {e}");
                return Err(format!("Failed to get initial rowid: {e}"));
            }
        };

        let mut last_rowid = initial_rowid;
        debug!("iMessage: starting poll from ROWID {last_rowid}");

        // Poll loop
        while self.running.load(Ordering::SeqCst) {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(self.poll_interval_secs)) => {},
                _ = self.shutdown.notified() => break,
            }

            let since = last_rowid;
            let allowed = self.allowed_contacts.clone();
            let (returned_conn, poll_result) = match tokio::task::spawn_blocking(move || {
                let result: Result<Vec<(i64, String, String)>, rusqlite::Error> = (|| {
                    let mut stmt = conn.prepare(
                        "SELECT m.ROWID, h.id, m.text \
                         FROM message m \
                         JOIN handle h ON m.handle_id = h.ROWID \
                         WHERE m.ROWID > ?1 \
                         AND m.is_from_me = 0 \
                         AND m.text IS NOT NULL \
                         ORDER BY m.ROWID ASC \
                         LIMIT 20",
                    )?;
                    let rows = stmt.query_map([since], |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    })?;
                    rows.collect::<Result<Vec<_>, _>>()
                })();
                (conn, result)
            })
            .await
            {
                Ok(pair) => pair,
                Err(e) => {
                    warn!("iMessage: poll worker join error: {e}");
                    break;
                }
            };
            conn = returned_conn;

            match poll_result {
                Ok(messages) => {
                    for (rowid, sender, text) in messages {
                        if rowid > last_rowid {
                            last_rowid = rowid;
                        }

                        // Allowlist filter
                        let is_allowed = if allowed.iter().any(|u| u == "*") {
                            true
                        } else {
                            allowed.iter().any(|u| u.eq_ignore_ascii_case(&sender))
                        };
                        if !is_allowed {
                            continue;
                        }

                        if text.trim().is_empty() {
                            continue;
                        }

                        let normalized = NormalizedMessage {
                            id: Uuid::new_v4(),
                            session_key: clawdesk_types::session::SessionKey::new(
                                ChannelId::IMessage,
                                &sender,
                            ),
                            body: text,
                            body_for_agent: None,
                            sender: SenderIdentity {
                                id: sender.clone(),
                                display_name: sender.clone(),
                                channel: ChannelId::IMessage,
                            },
                            media: vec![],
                            artifact_refs: vec![],
                            reply_context: None,
                            origin: clawdesk_types::message::MessageOrigin::IMessage {
                                rowid,
                                sender,
                            },
                            timestamp: Utc::now(),
                        };

                        sink.on_message(normalized).await;
                    }
                }
                Err(e) => {
                    warn!("iMessage: poll error: {e}");
                }
            }
        }

        info!("iMessage channel stopped");
        Ok(())
    }

    async fn send(&self, message: OutboundMessage) -> Result<DeliveryReceipt, String> {
        // Extract recipient from origin
        let recipient = match &message.origin {
            clawdesk_types::message::MessageOrigin::IMessage { sender, .. } => sender.clone(),
            _ => return Err("iMessage send: invalid origin (not IMessage)".to_string()),
        };

        // Defense-in-depth: validate target format
        if !is_valid_imessage_target(&recipient) {
            return Err(format!(
                "Invalid iMessage target: must be a phone number (+1234567890) or email (user@example.com), got: {recipient}"
            ));
        }

        // Escape both message AND target to prevent AppleScript injection
        let escaped_msg = escape_applescript(&message.body);
        let escaped_target = escape_applescript(&recipient);

        let script = format!(
            r#"tell application "Messages"
    set targetService to 1st account whose service type = iMessage
    set targetBuddy to participant "{escaped_target}" of targetService
    send "{escaped_msg}" to targetBuddy
end tell"#
        );

        let output = tokio::process::Command::new("osascript")
            .arg("-e")
            .arg(&script)
            .output()
            .await
            .map_err(|e| format!("iMessage: failed to run osascript: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("iMessage send failed: {stderr}"));
        }

        Ok(DeliveryReceipt {
            channel: ChannelId::IMessage,
            message_id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        info!("iMessage channel stopping...");
        self.running.store(false, Ordering::SeqCst);
        self.shutdown.notify_waiters();
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ═══════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── AppleScript escaping ──────────────────────────────────

    #[test]
    fn escape_plain_text() {
        assert_eq!(escape_applescript("hello world"), "hello world");
    }

    #[test]
    fn escape_double_quotes() {
        assert_eq!(escape_applescript(r#"say "hi""#), r#"say \"hi\""#);
    }

    #[test]
    fn escape_backslashes() {
        assert_eq!(escape_applescript(r"path\to\file"), r"path\\to\\file");
    }

    #[test]
    fn escape_newlines() {
        assert_eq!(escape_applescript("line1\nline2"), "line1\\nline2");
        assert_eq!(escape_applescript("line1\rline2"), "line1\\rline2");
    }

    #[test]
    fn escape_combined_injection() {
        // Attempt: end tell" & do shell script "rm -rf /"
        let malicious = r#"end tell" & do shell script "rm -rf /""#;
        let escaped = escape_applescript(malicious);
        assert!(!escaped.contains("\"\n"));
        assert!(escaped.contains("\\\""));
    }

    #[test]
    fn escape_empty_string() {
        assert_eq!(escape_applescript(""), "");
    }

    // ── Target validation ──────────────────────────────────

    #[test]
    fn valid_phone_numbers() {
        assert!(is_valid_imessage_target("+1234567890"));
        assert!(is_valid_imessage_target("+1 234-567-8900"));
        assert!(is_valid_imessage_target("+44 7911 123456"));
    }

    #[test]
    fn valid_emails() {
        assert!(is_valid_imessage_target("user@example.com"));
        assert!(is_valid_imessage_target("test.user+tag@mail.co.uk"));
    }

    #[test]
    fn invalid_targets() {
        assert!(!is_valid_imessage_target(""));
        assert!(!is_valid_imessage_target("   "));
        assert!(!is_valid_imessage_target("random string"));
        assert!(!is_valid_imessage_target("1234567890")); // no +
        assert!(!is_valid_imessage_target("@example.com")); // no local part
        assert!(!is_valid_imessage_target("user@")); // no domain
        assert!(!is_valid_imessage_target("user@nodot")); // no dot in domain
    }

    #[test]
    fn short_phone_rejected() {
        assert!(!is_valid_imessage_target("+123")); // too short (< 7 digits)
    }

    #[test]
    fn long_phone_rejected() {
        assert!(!is_valid_imessage_target("+1234567890123456")); // > 15 digits
    }

    // ── Contact allowlist ──────────────────────────────────

    #[test]
    fn wildcard_allows_all() {
        let ch = IMessageChannel::new(vec!["*".to_string()], 3);
        assert!(ch.is_contact_allowed("+1234567890"));
        assert!(ch.is_contact_allowed("anyone@example.com"));
    }

    #[test]
    fn specific_contacts_only() {
        let ch = IMessageChannel::new(vec!["+1234567890".to_string()], 3);
        assert!(ch.is_contact_allowed("+1234567890"));
        assert!(!ch.is_contact_allowed("+9876543210"));
    }

    #[test]
    fn case_insensitive_contacts() {
        let ch = IMessageChannel::new(vec!["User@Example.COM".to_string()], 3);
        assert!(ch.is_contact_allowed("user@example.com"));
        assert!(ch.is_contact_allowed("USER@EXAMPLE.COM"));
    }

    #[test]
    fn empty_contacts_blocks_all() {
        let ch = IMessageChannel::new(vec![], 3);
        assert!(!ch.is_contact_allowed("+1234567890"));
    }

    // ── Channel trait ──────────────────────────────────

    #[test]
    fn channel_id() {
        let ch = IMessageChannel::new(vec!["*".to_string()], 3);
        assert_eq!(ch.id(), ChannelId::IMessage);
    }

    #[test]
    fn channel_meta() {
        let ch = IMessageChannel::new(vec!["*".to_string()], 3);
        let meta = ch.meta();
        assert_eq!(meta.display_name, "iMessage");
        assert!(!meta.supports_threading);
        assert!(!meta.supports_streaming);
    }

    #[test]
    fn default_poll_interval() {
        let ch = IMessageChannel::new(vec![], 0);
        assert_eq!(ch.poll_interval_secs, 3);
    }
}
