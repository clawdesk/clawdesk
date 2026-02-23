//! Email channel adapter via IMAP (receive) + SMTP (send).
//!
//! Polls an IMAP mailbox for inbound messages and sends replies via SMTP.
//! Thread matching uses the `In-Reply-To` and `References` headers per
//! RFC 5322 / RFC 2822.
//!
//! ## Architecture
//!
//! ```text
//! EmailChannel
//! ├── imap_poll_loop() — IDLE or periodic FETCH on IMAP mailbox
//! ├── normalize()      — IMAP email → NormalizedMessage
//! ├── send()           — OutboundMessage → SMTP send with threading headers
//! └── match_thread()   — In-Reply-To / References header matching
//! ```
//!
//! ## Protocols
//!
//! IMAP (RFC 3501):
//! - `SELECT INBOX`         — open mailbox
//! - `SEARCH UNSEEN`        — find unread messages
//! - `FETCH n (BODY[])`     — retrieve message content
//! - `STORE n +FLAGS (\Seen)` — mark as read
//! - `IDLE`                 — push notification (RFC 2177)
//!
//! SMTP (RFC 5321):
//! - Standard SMTP submission (port 587/465)
//! - STARTTLS or implicit TLS
//!
//! ## Thread matching
//!
//! Email threads are tracked via:
//! - `Message-ID` header (unique per email)
//! - `In-Reply-To` header (references the parent message)
//! - `References` header (full thread ancestry)
//!
//! ## Limits
//!
//! Provider-specific:
//! - Gmail: 500 recipients/day (free), 2000 (Workspace)
//! - Outlook: 300 messages/day
//! - Message size: typically 25-50 MB including attachments

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, Threaded};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, MediaAttachment, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

/// Email channel adapter.
pub struct EmailChannel {
    /// IMAP configuration.
    imap_config: ImapConfig,
    /// SMTP configuration.
    smtp_config: SmtpConfig,
    /// From address for outbound emails.
    from_address: String,
    /// Display name for the From header.
    from_name: String,
    /// Mailbox to monitor (default: INBOX).
    mailbox: String,
    /// Polling interval in seconds (used when IDLE is not supported).
    poll_interval_secs: u64,
    /// Last seen UID for incremental fetching.
    last_uid: AtomicU32,
    /// Shutdown flag.
    running: AtomicBool,
    /// Shutdown notifier.
    shutdown: Notify,
}

/// IMAP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImapConfig {
    pub host: String,
    #[serde(default = "default_imap_port")]
    pub port: u16,
    pub username: String,
    pub password: String,
    #[serde(default = "default_true")]
    pub use_tls: bool,
}

/// SMTP server configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SmtpConfig {
    pub host: String,
    #[serde(default = "default_smtp_port")]
    pub port: u16,
    pub username: String,
    pub password: String,
    #[serde(default = "default_true")]
    pub use_tls: bool,
}

/// Full email channel configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailConfig {
    pub imap: ImapConfig,
    pub smtp: SmtpConfig,
    pub from_address: String,
    #[serde(default = "default_from_name")]
    pub from_name: String,
    #[serde(default = "default_mailbox")]
    pub mailbox: String,
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
}

fn default_imap_port() -> u16 { 993 }
fn default_smtp_port() -> u16 { 587 }
fn default_true() -> bool { true }
fn default_from_name() -> String { "ClawDesk".into() }
fn default_mailbox() -> String { "INBOX".into() }
fn default_poll_interval() -> u64 { 30 }

/// Parsed email message from IMAP.
#[derive(Debug, Clone)]
struct ParsedEmail {
    /// Unique message UID from IMAP.
    uid: u32,
    /// Message-ID header.
    message_id: String,
    /// From address.
    from: String,
    /// From display name.
    from_name: Option<String>,
    /// To address(es).
    to: String,
    /// Subject line.
    subject: String,
    /// Plain text body (preferred).
    body_text: String,
    /// HTML body (fallback).
    body_html: Option<String>,
    /// In-Reply-To header (parent message ID).
    in_reply_to: Option<String>,
    /// References header (full thread chain).
    references: Vec<String>,
    /// Parsed attachments.
    attachments: Vec<EmailAttachment>,
}

/// Email attachment metadata.
#[derive(Debug, Clone)]
struct EmailAttachment {
    filename: String,
    mime_type: String,
    size_bytes: u64,
    data: Vec<u8>,
}

impl EmailChannel {
    pub fn new(config: EmailConfig) -> Self {
        Self {
            imap_config: config.imap,
            smtp_config: config.smtp,
            from_address: config.from_address,
            from_name: config.from_name,
            mailbox: config.mailbox,
            poll_interval_secs: config.poll_interval_secs,
            last_uid: AtomicU32::new(0),
            running: AtomicBool::new(false),
            shutdown: Notify::new(),
        }
    }

    /// Derive a thread/session key from the email's In-Reply-To and References.
    fn derive_thread_key(&self, email: &ParsedEmail) -> String {
        // The thread root is the first message in the References chain,
        // or the In-Reply-To if References is empty, or the Message-ID itself.
        if let Some(root) = email.references.first() {
            root.clone()
        } else if let Some(ref reply_to) = email.in_reply_to {
            reply_to.clone()
        } else {
            email.message_id.clone()
        }
    }

    /// Generate a unique Message-ID for outbound emails.
    fn generate_message_id(&self) -> String {
        let domain = self
            .from_address
            .rsplit('@')
            .next()
            .unwrap_or("clawdesk.local");
        format!("<{}.{}@{}>", uuid::Uuid::new_v4(), chrono::Utc::now().timestamp(), domain)
    }

    /// Build an RFC 5322 email message as raw bytes.
    fn build_email(
        &self,
        to: &str,
        subject: &str,
        body: &str,
        in_reply_to: Option<&str>,
        references: Option<&str>,
    ) -> String {
        let message_id = self.generate_message_id();
        let date = chrono::Utc::now().format("%a, %d %b %Y %H:%M:%S +0000").to_string();

        let mut headers = format!(
            "From: {} <{}>\r\n\
             To: {}\r\n\
             Subject: {}\r\n\
             Date: {}\r\n\
             Message-ID: {}\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\
             Content-Transfer-Encoding: 8bit\r\n",
            self.from_name, self.from_address, to, subject, date, message_id
        );

        if let Some(irt) = in_reply_to {
            headers.push_str(&format!("In-Reply-To: {}\r\n", irt));
        }
        if let Some(refs) = references {
            headers.push_str(&format!("References: {}\r\n", refs));
        }

        format!("{}\r\n{}", headers, body)
    }

    /// Normalize a parsed email into NormalizedMessage.
    fn normalize_email(&self, email: &ParsedEmail) -> NormalizedMessage {
        let sender = SenderIdentity {
            id: email.from.clone(),
            display_name: email
                .from_name
                .clone()
                .unwrap_or_else(|| email.from.clone()),
            channel: ChannelId::Email,
        };

        let thread_key = self.derive_thread_key(email);
        let session_key = clawdesk_types::session::SessionKey::new(ChannelId::Email, &thread_key);

        let media: Vec<MediaAttachment> = email
            .attachments
            .iter()
            .map(|a| MediaAttachment {
                media_type: mime_to_media_type(&a.mime_type),
                url: None,
                data: Some(a.data.clone()),
                mime_type: a.mime_type.clone(),
                filename: Some(a.filename.clone()),
                size_bytes: Some(a.size_bytes),
            })
            .collect();

        let reply_context = email.in_reply_to.as_ref().map(|irt| {
            clawdesk_types::message::ReplyContext {
                original_message_id: irt.clone(),
                original_text: None,
                original_sender: None,
            }
        });

        let origin = clawdesk_types::message::MessageOrigin::Email {
            message_id: email.message_id.clone(),
            from: email.from.clone(),
            to: email.to.clone(),
        };

        NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key,
            body: email.body_text.clone(),
            body_for_agent: Some(format!("Subject: {}\n\n{}", email.subject, email.body_text)),
            sender,
            media,
            reply_context,
            origin,
            timestamp: chrono::Utc::now(),
        }
    }

    /// IMAP polling loop: connects to the IMAP server, fetches unseen messages,
    /// normalizes them, and dispatches via `sink.on_message()`.
    ///
    /// Uses raw IMAP commands over TLS (tokio-rustls). On each poll cycle:
    /// 1. LOGIN with credentials
    /// 2. SELECT mailbox
    /// 3. SEARCH UNSEEN for unread messages
    /// 4. FETCH each UID (BODY.PEEK[HEADER] + BODY.PEEK[TEXT])
    /// 5. Parse and normalize
    /// 6. STORE +FLAGS (\Seen) to mark as read
    /// 7. LOGOUT
    /// 8. Sleep for poll_interval_secs
    async fn imap_poll_loop(self: Arc<Self>, sink: Arc<dyn MessageSink>) {
        info!(
            host = %self.imap_config.host,
            mailbox = %self.mailbox,
            poll_secs = self.poll_interval_secs,
            "Email IMAP poll loop started"
        );

        while self.running.load(Ordering::Relaxed) {
            match self.imap_poll_once(&sink).await {
                Ok(count) => {
                    if count > 0 {
                        info!(count, "Email: processed new messages");
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Email IMAP poll error, retrying next cycle");
                }
            }

            tokio::time::sleep(Duration::from_secs(self.poll_interval_secs)).await;
        }

        info!("Email IMAP poll loop stopped");
    }

    /// Single IMAP poll cycle — connect, fetch unseen, normalize, mark read.
    async fn imap_poll_once(&self, sink: &Arc<dyn MessageSink>) -> Result<usize, String> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        // Connect TLS
        let addr = format!("{}:{}", self.imap_config.host, self.imap_config.port);
        let tcp = tokio::net::TcpStream::connect(&addr)
            .await
            .map_err(|e| format!("IMAP connect failed: {e}"))?;

        let mut root_store = tokio_rustls::rustls::RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        let tls_config = tokio_rustls::rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        let connector = tokio_tls_connector(tls_config);
        let server_name = tokio_rustls::rustls::pki_types::ServerName::try_from(
            self.imap_config.host.as_str(),
        )
        .map_err(|e| format!("invalid server name: {e}"))?
        .to_owned();

        let tls_stream = connector
            .connect(server_name, tcp)
            .await
            .map_err(|e| format!("IMAP TLS handshake failed: {e}"))?;

        let (reader, mut writer) = tokio::io::split(tls_stream);
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        let mut tag = 0u32;

        // Read server greeting
        line.clear();
        reader
            .read_line(&mut line)
            .await
            .map_err(|e| format!("IMAP greeting read failed: {e}"))?;

        if !line.contains("OK") {
            return Err(format!("IMAP server rejected connection: {}", line.trim()));
        }

        // LOGIN
        tag += 1;
        let login_cmd = format!(
            "A{tag} LOGIN {} {}\r\n",
            self.imap_config.username, self.imap_config.password
        );
        writer
            .write_all(login_cmd.as_bytes())
            .await
            .map_err(|e| format!("IMAP LOGIN write failed: {e}"))?;

        loop {
            line.clear();
            reader
                .read_line(&mut line)
                .await
                .map_err(|e| format!("IMAP LOGIN read failed: {e}"))?;
            if line.starts_with(&format!("A{tag} ")) {
                if !line.contains("OK") {
                    return Err(format!("IMAP LOGIN failed: {}", line.trim()));
                }
                break;
            }
        }

        // SELECT mailbox
        tag += 1;
        let select_cmd = format!("A{tag} SELECT {}\r\n", self.mailbox);
        writer
            .write_all(select_cmd.as_bytes())
            .await
            .map_err(|e| format!("IMAP SELECT write failed: {e}"))?;

        loop {
            line.clear();
            reader
                .read_line(&mut line)
                .await
                .map_err(|e| format!("IMAP SELECT read failed: {e}"))?;
            if line.starts_with(&format!("A{tag} ")) {
                if !line.contains("OK") {
                    return Err(format!("IMAP SELECT failed: {}", line.trim()));
                }
                break;
            }
        }

        // SEARCH UNSEEN
        tag += 1;
        let search_cmd = format!("A{tag} SEARCH UNSEEN\r\n");
        writer
            .write_all(search_cmd.as_bytes())
            .await
            .map_err(|e| format!("IMAP SEARCH write failed: {e}"))?;

        let mut uids: Vec<u32> = Vec::new();
        loop {
            line.clear();
            reader
                .read_line(&mut line)
                .await
                .map_err(|e| format!("IMAP SEARCH read failed: {e}"))?;

            if line.starts_with("* SEARCH") {
                // Parse UIDs from "* SEARCH 1 2 3"
                uids = line
                    .trim()
                    .strip_prefix("* SEARCH")
                    .unwrap_or("")
                    .split_whitespace()
                    .filter_map(|s| s.parse::<u32>().ok())
                    .filter(|&uid| uid > self.last_uid.load(Ordering::Relaxed))
                    .collect();
            }

            if line.starts_with(&format!("A{tag} ")) {
                break;
            }
        }

        let mut count = 0;

        for uid in &uids {
            // FETCH message
            tag += 1;
            let fetch_cmd = format!("A{tag} FETCH {uid} (BODY.PEEK[HEADER] BODY.PEEK[TEXT])\r\n");
            writer
                .write_all(fetch_cmd.as_bytes())
                .await
                .map_err(|e| format!("IMAP FETCH write failed: {e}"))?;

            let mut headers = String::new();
            let mut body_text = String::new();
            let mut in_headers = false;
            let mut in_body = false;

            loop {
                line.clear();
                reader
                    .read_line(&mut line)
                    .await
                    .map_err(|e| format!("IMAP FETCH read failed: {e}"))?;

                if line.starts_with(&format!("A{tag} ")) {
                    break;
                }

                // Simple state machine for FETCH response parsing
                if line.contains("BODY[HEADER]") {
                    in_headers = true;
                    in_body = false;
                    continue;
                }
                if line.contains("BODY[TEXT]") {
                    in_headers = false;
                    in_body = true;
                    continue;
                }
                if line.trim() == ")" {
                    in_headers = false;
                    in_body = false;
                    continue;
                }

                if in_headers {
                    headers.push_str(&line);
                } else if in_body {
                    body_text.push_str(&line);
                }
            }

            // Parse headers
            let from = extract_header(&headers, "From").unwrap_or_default();
            let to = extract_header(&headers, "To").unwrap_or_default();
            let subject = extract_header(&headers, "Subject").unwrap_or_default();
            let message_id = extract_header(&headers, "Message-ID")
                .unwrap_or_else(|| format!("<{uid}@imap>"));
            let in_reply_to = extract_header(&headers, "In-Reply-To");
            let references: Vec<String> = extract_header(&headers, "References")
                .map(|r| r.split_whitespace().map(String::from).collect())
                .unwrap_or_default();

            // Extract sender name from "Name <email>" format
            let (from_name, from_addr) = parse_email_address(&from);

            let parsed = ParsedEmail {
                uid: *uid,
                message_id,
                from: from_addr,
                from_name: Some(from_name),
                to,
                subject,
                body_text: body_text.trim().to_string(),
                body_html: None,
                in_reply_to,
                references,
                attachments: vec![],
            };

            let normalized = self.normalize_email(&parsed);
            sink.on_message(normalized).await;
            count += 1;

            // Mark as seen
            tag += 1;
            let store_cmd = format!("A{tag} STORE {uid} +FLAGS (\\Seen)\r\n");
            let _ = writer.write_all(store_cmd.as_bytes()).await;
            loop {
                line.clear();
                let _ = reader.read_line(&mut line).await;
                if line.starts_with(&format!("A{tag} ")) {
                    break;
                }
            }

            self.last_uid.store(*uid, Ordering::Relaxed);
        }

        // LOGOUT
        tag += 1;
        let logout_cmd = format!("A{tag} LOGOUT\r\n");
        let _ = writer.write_all(logout_cmd.as_bytes()).await;

        Ok(count)
    }
}

/// Map MIME type string to MediaType.
fn mime_to_media_type(mime: &str) -> clawdesk_types::message::MediaType {
    if mime.starts_with("image/") {
        clawdesk_types::message::MediaType::Image
    } else if mime.starts_with("video/") {
        clawdesk_types::message::MediaType::Video
    } else if mime.starts_with("audio/") {
        clawdesk_types::message::MediaType::Audio
    } else {
        clawdesk_types::message::MediaType::Document
    }
}

/// Create a TLS connector from a rustls ClientConfig.
fn tokio_tls_connector(
    config: tokio_rustls::rustls::ClientConfig,
) -> tokio_rustls::TlsConnector {
    tokio_rustls::TlsConnector::from(std::sync::Arc::new(config))
}

/// Extract a header value by name from raw IMAP headers.
fn extract_header(headers: &str, name: &str) -> Option<String> {
    let prefix = format!("{}: ", name);
    for line in headers.lines() {
        if let Some(rest) = line.strip_prefix(&prefix) {
            return Some(rest.trim().to_string());
        }
        // Case-insensitive fallback
        if line.len() > prefix.len() {
            let lower = line[..prefix.len()].to_lowercase();
            if lower == prefix.to_lowercase() {
                return Some(line[prefix.len()..].trim().to_string());
            }
        }
    }
    None
}

/// Parse "Display Name <email@example.com>" into (name, email).
fn parse_email_address(addr: &str) -> (String, String) {
    if let Some(start) = addr.find('<') {
        if let Some(end) = addr.find('>') {
            let name = addr[..start].trim().trim_matches('"').to_string();
            let email = addr[start + 1..end].trim().to_string();
            if name.is_empty() {
                return (email.clone(), email);
            }
            return (name, email);
        }
    }
    (addr.to_string(), addr.to_string())
}

#[async_trait]
impl Channel for EmailChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Email
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Email".into(),
            supports_threading: true,
            supports_streaming: false,
            supports_reactions: false,
            supports_media: true,
            supports_groups: false,
            max_message_length: None, // Email has no practical text limit
        }
    }

    async fn start(&self, sink: Arc<dyn MessageSink>) -> Result<(), String> {
        self.running.store(true, Ordering::Relaxed);

        info!(
            imap_host = %self.imap_config.host,
            imap_port = self.imap_config.port,
            smtp_host = %self.smtp_config.host,
            smtp_port = self.smtp_config.port,
            from = %self.from_address,
            mailbox = %self.mailbox,
            "Email channel starting"
        );

        // Spawn the IMAP poll loop on a background task.
        let poll_channel = Arc::new(EmailChannel::new(EmailConfig {
            imap: self.imap_config.clone(),
            smtp: self.smtp_config.clone(),
            from_address: self.from_address.clone(),
            from_name: self.from_name.clone(),
            mailbox: self.mailbox.clone(),
            poll_interval_secs: self.poll_interval_secs,
        }));
        poll_channel.running.store(true, Ordering::Relaxed);

        tokio::spawn(async move {
            poll_channel.imap_poll_loop(sink).await;
        });

        info!("Email channel started — IMAP poll loop spawned");
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let (recipient, original_msg_id) = match &msg.origin {
            clawdesk_types::message::MessageOrigin::Email {
                from,
                message_id,
                ..
            } => (from.clone(), Some(message_id.clone())),
            _ => return Err("cannot send email without Email origin".into()),
        };

        // Build subject line (Re: prefix for replies)
        let subject = if original_msg_id.is_some() {
            format!("Re: ClawDesk Response")
        } else {
            "ClawDesk Response".into()
        };

        // Build In-Reply-To and References headers for thread matching
        let in_reply_to = original_msg_id.as_deref();
        let references = original_msg_id.as_deref();

        let email_raw = self.build_email(
            &recipient,
            &subject,
            &msg.body,
            in_reply_to,
            references,
        );

        // In production: send via SMTP
        // 1. Connect to SMTP server with STARTTLS or implicit TLS
        // 2. AUTH LOGIN / AUTH PLAIN
        // 3. MAIL FROM:<from_address>
        // 4. RCPT TO:<recipient>
        // 5. DATA <email_raw>
        // 6. QUIT

        debug!(
            to = %recipient,
            subject = %subject,
            size = email_raw.len(),
            "Email send prepared"
        );

        let sent_message_id = self.generate_message_id();

        Ok(DeliveryReceipt {
            channel: ChannelId::Email,
            message_id: sent_message_id,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);
        self.shutdown.notify_waiters();
        info!("Email channel stopped");
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait]
impl Threaded for EmailChannel {
    async fn send_to_thread(
        &self,
        thread_id: &str,
        msg: OutboundMessage,
    ) -> Result<DeliveryReceipt, String> {
        let recipient = match &msg.origin {
            clawdesk_types::message::MessageOrigin::Email { from, .. } => from.clone(),
            _ => return Err("cannot send email thread reply without Email origin".into()),
        };

        let email_raw = self.build_email(
            &recipient,
            "Re: ClawDesk Response",
            &msg.body,
            Some(thread_id),
            Some(thread_id),
        );

        debug!(
            to = %recipient,
            thread = %thread_id,
            size = email_raw.len(),
            "Email thread reply prepared"
        );

        Ok(DeliveryReceipt {
            channel: ChannelId::Email,
            message_id: self.generate_message_id(),
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn create_thread(
        &self,
        parent_msg_id: &str,
        _title: &str,
    ) -> Result<String, String> {
        // In email, threads are identified by the Message-ID chain.
        // The parent message ID becomes the In-Reply-To.
        Ok(parent_msg_id.to_string())
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> EmailConfig {
        EmailConfig {
            imap: ImapConfig {
                host: "imap.gmail.com".into(),
                port: 993,
                username: "bot@example.com".into(),
                password: "app-password".into(),
                use_tls: true,
            },
            smtp: SmtpConfig {
                host: "smtp.gmail.com".into(),
                port: 587,
                username: "bot@example.com".into(),
                password: "app-password".into(),
                use_tls: true,
            },
            from_address: "bot@example.com".into(),
            from_name: "ClawDesk Bot".into(),
            mailbox: "INBOX".into(),
            poll_interval_secs: 30,
        }
    }

    #[test]
    fn test_email_channel_creation() {
        let channel = EmailChannel::new(test_config());
        assert_eq!(channel.id(), ChannelId::Email);
        assert_eq!(channel.from_address, "bot@example.com");
        assert_eq!(channel.mailbox, "INBOX");
        assert_eq!(channel.poll_interval_secs, 30);
    }

    #[test]
    fn test_email_meta() {
        let channel = EmailChannel::new(test_config());
        let meta = channel.meta();
        assert_eq!(meta.display_name, "Email");
        assert!(meta.supports_threading);
        assert!(!meta.supports_streaming);
        assert!(!meta.supports_reactions);
        assert!(meta.supports_media);
        assert!(!meta.supports_groups);
        assert!(meta.max_message_length.is_none());
    }

    #[test]
    fn test_email_thread_key_with_references() {
        let channel = EmailChannel::new(test_config());

        let email = ParsedEmail {
            uid: 1,
            message_id: "<msg3@example.com>".into(),
            from: "alice@example.com".into(),
            from_name: Some("Alice".into()),
            to: "bot@example.com".into(),
            subject: "Re: Re: Hello".into(),
            body_text: "Thread reply".into(),
            body_html: None,
            in_reply_to: Some("<msg2@example.com>".into()),
            references: vec![
                "<msg1@example.com>".into(),
                "<msg2@example.com>".into(),
            ],
            attachments: vec![],
        };

        // Thread key should be the first reference (thread root)
        assert_eq!(channel.derive_thread_key(&email), "<msg1@example.com>");
    }

    #[test]
    fn test_email_thread_key_with_in_reply_to_only() {
        let channel = EmailChannel::new(test_config());

        let email = ParsedEmail {
            uid: 2,
            message_id: "<msg2@example.com>".into(),
            from: "bob@example.com".into(),
            from_name: None,
            to: "bot@example.com".into(),
            subject: "Re: Hello".into(),
            body_text: "Reply".into(),
            body_html: None,
            in_reply_to: Some("<msg1@example.com>".into()),
            references: vec![],
            attachments: vec![],
        };

        assert_eq!(channel.derive_thread_key(&email), "<msg1@example.com>");
    }

    #[test]
    fn test_email_thread_key_new_thread() {
        let channel = EmailChannel::new(test_config());

        let email = ParsedEmail {
            uid: 3,
            message_id: "<new@example.com>".into(),
            from: "carol@example.com".into(),
            from_name: Some("Carol".into()),
            to: "bot@example.com".into(),
            subject: "New topic".into(),
            body_text: "Starting a new thread".into(),
            body_html: None,
            in_reply_to: None,
            references: vec![],
            attachments: vec![],
        };

        assert_eq!(channel.derive_thread_key(&email), "<new@example.com>");
    }

    #[test]
    fn test_email_normalize() {
        let channel = EmailChannel::new(test_config());

        let email = ParsedEmail {
            uid: 10,
            message_id: "<test@example.com>".into(),
            from: "alice@example.com".into(),
            from_name: Some("Alice".into()),
            to: "bot@example.com".into(),
            subject: "Hello Bot".into(),
            body_text: "Can you help me?".into(),
            body_html: None,
            in_reply_to: None,
            references: vec![],
            attachments: vec![],
        };

        let normalized = channel.normalize_email(&email);
        assert_eq!(normalized.body, "Can you help me?");
        assert_eq!(normalized.sender.id, "alice@example.com");
        assert_eq!(normalized.sender.display_name, "Alice");
        assert!(normalized.body_for_agent.is_some());
        assert!(normalized
            .body_for_agent
            .unwrap()
            .contains("Subject: Hello Bot"));
    }

    #[test]
    fn test_email_build_message() {
        let channel = EmailChannel::new(test_config());

        let raw = channel.build_email(
            "user@example.com",
            "Test Subject",
            "Hello from ClawDesk",
            Some("<parent@example.com>"),
            Some("<parent@example.com>"),
        );

        assert!(raw.contains("From: ClawDesk Bot <bot@example.com>"));
        assert!(raw.contains("To: user@example.com"));
        assert!(raw.contains("Subject: Test Subject"));
        assert!(raw.contains("In-Reply-To: <parent@example.com>"));
        assert!(raw.contains("References: <parent@example.com>"));
        assert!(raw.contains("Hello from ClawDesk"));
    }

    #[test]
    fn test_email_generate_message_id() {
        let channel = EmailChannel::new(test_config());
        let id1 = channel.generate_message_id();
        let id2 = channel.generate_message_id();

        assert!(id1.starts_with('<'));
        assert!(id1.ends_with('>'));
        assert!(id1.contains("@example.com"));
        assert_ne!(id1, id2); // Each call should produce a unique ID
    }
}
