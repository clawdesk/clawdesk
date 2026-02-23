//! IRC over TLS channel adapter.
//!
//! Connects to an IRC server using TLS, authenticates via SASL PLAIN
//! or NickServ, joins configured channels, and forwards PRIVMSG events
//! to the ClawDesk message bus.
//!
//! ## Protocol
//!
//! IRC is a line-based protocol (RFC 1459 / 2812):
//! - Lines are terminated by `\r\n`
//! - Maximum line length is 512 bytes (including `\r\n`)
//! - Server-prepended prefix `:nick!user@host` consumes ~64 bytes
//!
//! ## Authentication stack
//!
//! 1. **Server PASS** — sent before NICK/USER (IRCv3)
//! 2. **SASL PLAIN** — inline base64(\\0nick\\0password) during CAP negotiation
//! 3. **NickServ** — `PRIVMSG NickServ :IDENTIFY <password>` after RPL_WELCOME
//!
//! ## Message handling
//!
//! - Inbound PRIVMSGs are prefixed with [`IRC_STYLE_PREFIX`] to instruct
//!   the LLM to respond in plain text (no markdown, no tables).
//! - Channel messages (`#channel`) include `<sender>` prefix; DMs do not.
//! - Outbound messages are split at safe UTF-8 boundaries respecting the
//!   512-byte IRC line limit via [`split_message`].
//!
//! ## Architecture
//!
//! ```text
//! IrcChannel
//! ├── start(sink)   — TLS connect → CAP/SASL → NICK/USER → JOIN → PRIVMSG loop
//! ├── send(msg)     — split + PRIVMSG via shared writer
//! ├── stop()        — flip AtomicBool + QUIT
//! └── health_check  — TLS connect + QUIT
//! ```

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use chrono::Utc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{Mutex, Notify};
use tokio_rustls::rustls;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Read timeout — if no data arrives within this duration, the connection
/// is considered dead. IRC servers typically PING every 60-120s.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Monotonic counter for unique message IDs under burst traffic.
static MSG_SEQ: AtomicU64 = AtomicU64::new(0);

/// Style instruction prepended to every IRC message before it reaches the LLM.
/// IRC clients render plain text only — no markdown, no HTML, no XML.
const IRC_STYLE_PREFIX: &str = "\
[context: you are responding over IRC. \
Plain text only. No markdown, no tables, no XML/HTML tags. \
Never use triple backtick code fences. Use a single blank line to separate blocks instead. \
Be terse and concise. \
Use short lines. Avoid walls of text.]\n";

/// Reserved bytes for the server-prepended sender prefix (`:nick!user@host `).
const SENDER_PREFIX_RESERVE: usize = 64;

type WriteHalf = tokio::io::WriteHalf<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;

/// IRC over TLS channel.
pub struct IrcChannel {
    server: String,
    port: u16,
    nickname: String,
    username: String,
    channels: Vec<String>,
    allowed_users: Vec<String>,
    server_password: Option<String>,
    nickserv_password: Option<String>,
    sasl_password: Option<String>,
    verify_tls: bool,
    /// Shared write half of the TLS stream for sending messages.
    writer: Arc<Mutex<Option<WriteHalf>>>,
    /// Shutdown flag.
    running: AtomicBool,
    /// Shutdown notifier.
    shutdown: Notify,
}

/// Configuration for constructing an `IrcChannel`.
#[derive(Debug, Clone)]
pub struct IrcChannelConfig {
    pub server: String,
    pub port: u16,
    pub nickname: String,
    pub username: Option<String>,
    pub channels: Vec<String>,
    pub allowed_users: Vec<String>,
    pub server_password: Option<String>,
    pub nickserv_password: Option<String>,
    pub sasl_password: Option<String>,
    pub verify_tls: bool,
}

impl IrcChannel {
    pub fn new(cfg: IrcChannelConfig) -> Self {
        let username = cfg.username.unwrap_or_else(|| cfg.nickname.clone());
        Self {
            server: cfg.server,
            port: cfg.port,
            nickname: cfg.nickname,
            username,
            channels: cfg.channels,
            allowed_users: cfg.allowed_users,
            server_password: cfg.server_password,
            nickserv_password: cfg.nickserv_password,
            sasl_password: cfg.sasl_password,
            verify_tls: cfg.verify_tls,
            writer: Arc::new(Mutex::new(None)),
            running: AtomicBool::new(false),
            shutdown: Notify::new(),
        }
    }

    fn is_user_allowed(&self, nick: &str) -> bool {
        if self.allowed_users.iter().any(|u| u == "*") {
            return true;
        }
        self.allowed_users
            .iter()
            .any(|u| u.eq_ignore_ascii_case(nick))
    }

    /// Create a TLS connection to the IRC server.
    async fn connect(
        &self,
    ) -> Result<tokio_rustls::client::TlsStream<tokio::net::TcpStream>, String> {
        let addr = format!("{}:{}", self.server, self.port);
        let tcp = tokio::net::TcpStream::connect(&addr)
            .await
            .map_err(|e| format!("IRC TCP connect to {addr} failed: {e}"))?;

        let tls_config = if self.verify_tls {
            let root_store: rustls::RootCertStore =
                webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect();
            rustls::ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth()
        } else {
            rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(NoVerify))
                .with_no_client_auth()
        };

        let connector = tokio_rustls::TlsConnector::from(Arc::new(tls_config));
        let domain = rustls::pki_types::ServerName::try_from(self.server.clone())
            .map_err(|e| format!("IRC invalid server name: {e}"))?;
        let tls = connector
            .connect(domain, tcp)
            .await
            .map_err(|e| format!("IRC TLS handshake failed: {e}"))?;

        Ok(tls)
    }

    /// Send a raw IRC line (appends \r\n).
    async fn send_raw(writer: &mut WriteHalf, line: &str) -> Result<(), String> {
        let data = format!("{line}\r\n");
        writer
            .write_all(data.as_bytes())
            .await
            .map_err(|e| format!("IRC write error: {e}"))?;
        writer
            .flush()
            .await
            .map_err(|e| format!("IRC flush error: {e}"))?;
        Ok(())
    }
}

/// A parsed IRC message.
#[derive(Debug, Clone, PartialEq, Eq)]
struct IrcMessageParsed {
    prefix: Option<String>,
    command: String,
    params: Vec<String>,
}

impl IrcMessageParsed {
    /// Parse a raw IRC line.
    ///
    /// IRC format: `[:<prefix>] <command> [<params>] [:<trailing>]`
    fn parse(line: &str) -> Option<Self> {
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            return None;
        }

        let (prefix, rest) = if let Some(stripped) = line.strip_prefix(':') {
            let space = stripped.find(' ')?;
            (Some(stripped[..space].to_string()), &stripped[space + 1..])
        } else {
            (None, line)
        };

        let (params_part, trailing) = if let Some(colon_pos) = rest.find(" :") {
            (&rest[..colon_pos], Some(&rest[colon_pos + 2..]))
        } else {
            (rest, None)
        };

        let mut parts: Vec<&str> = params_part.split_whitespace().collect();
        if parts.is_empty() {
            return None;
        }

        let command = parts.remove(0).to_uppercase();
        let mut params: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
        if let Some(t) = trailing {
            params.push(t.to_string());
        }

        Some(IrcMessageParsed {
            prefix,
            command,
            params,
        })
    }

    /// Extract the nickname from the prefix (nick!user@host → nick).
    fn nick(&self) -> Option<&str> {
        self.prefix.as_ref().and_then(|p| {
            let end = p.find('!').unwrap_or(p.len());
            let nick = &p[..end];
            if nick.is_empty() {
                None
            } else {
                Some(nick)
            }
        })
    }
}

/// Encode SASL PLAIN credentials: base64(\0nick\0password).
fn encode_sasl_plain(nick: &str, password: &str) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let input = format!("\0{nick}\0{password}");
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);

    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(CHARS[(triple >> 18 & 0x3F) as usize] as char);
        out.push(CHARS[(triple >> 12 & 0x3F) as usize] as char);

        if chunk.len() > 1 {
            out.push(CHARS[(triple >> 6 & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }

        if chunk.len() > 2 {
            out.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }

    out
}

/// Split a message into lines safe for IRC transmission.
///
/// IRC is line-based — `\r\n` terminates each command, so any newline
/// inside a PRIVMSG payload would truncate the message. This function:
/// 1. Splits on `\n` so each logical line becomes its own PRIVMSG.
/// 2. Splits any line exceeding `max_bytes` at a safe UTF-8 boundary.
/// 3. Skips empty lines to avoid blank PRIVMSGs.
pub fn split_message(message: &str, max_bytes: usize) -> Vec<String> {
    let mut chunks = Vec::new();

    if max_bytes == 0 {
        let mut full = String::new();
        for l in message
            .lines()
            .map(|l| l.trim_end_matches('\r'))
            .filter(|l| !l.is_empty())
        {
            if !full.is_empty() {
                full.push(' ');
            }
            full.push_str(l);
        }
        if full.is_empty() {
            chunks.push(String::new());
        } else {
            chunks.push(full);
        }
        return chunks;
    }

    for line in message.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }

        if line.len() <= max_bytes {
            chunks.push(line.to_string());
            continue;
        }

        // Line exceeds max_bytes — split at safe UTF-8 boundaries
        let mut remaining = line;
        while !remaining.is_empty() {
            if remaining.len() <= max_bytes {
                chunks.push(remaining.to_string());
                break;
            }

            let mut split_at = max_bytes;
            while split_at > 0 && !remaining.is_char_boundary(split_at) {
                split_at -= 1;
            }
            if split_at == 0 {
                split_at = max_bytes;
                while split_at < remaining.len() && !remaining.is_char_boundary(split_at) {
                    split_at += 1;
                }
            }

            chunks.push(remaining[..split_at].to_string());
            remaining = &remaining[split_at..];
        }
    }

    if chunks.is_empty() {
        chunks.push(String::new());
    }

    chunks
}

/// Certificate verifier that accepts any certificate (for `verify_tls=false`).
#[derive(Debug)]
struct NoVerify;

impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

#[async_trait]
#[allow(clippy::too_many_lines)]
impl Channel for IrcChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Irc
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "IRC".to_string(),
            supports_threading: false,
            supports_streaming: false,
            supports_reactions: false,
            supports_media: false,
            supports_groups: true,
            max_message_length: Some(512 - SENDER_PREFIX_RESERVE - 20),
        }
    }

    async fn start(&self, sink: Arc<dyn MessageSink>) -> Result<(), String> {
        self.running.store(true, Ordering::SeqCst);
        let mut current_nick = self.nickname.clone();
        info!(
            "IRC channel connecting to {}:{} as {}...",
            self.server, self.port, current_nick
        );

        let tls = match self.connect().await {
            Ok(tls) => tls,
            Err(e) => {
                warn!("IRC: connection failed: {e}");
                return Err(format!("IRC connection failed: {e}"));
            }
        };

        let (reader, mut writer) = tokio::io::split(tls);

        // --- SASL negotiation ---
        if self.sasl_password.is_some() {
            if let Err(e) = Self::send_raw(&mut writer, "CAP REQ :sasl").await {
                warn!("IRC: failed to request SASL: {e}");
            }
        }

        // --- Server password ---
        if let Some(ref pass) = self.server_password {
            if let Err(e) = Self::send_raw(&mut writer, &format!("PASS {pass}")).await {
                warn!("IRC: failed to send PASS: {e}");
            }
        }

        // --- Nick/User registration ---
        if let Err(e) = Self::send_raw(&mut writer, &format!("NICK {current_nick}")).await {
            warn!("IRC: failed to send NICK: {e}");
            return Err(format!("Failed to send NICK: {e}"));
        }
        if let Err(e) = Self::send_raw(
            &mut writer,
            &format!("USER {} 0 * :ClawDesk", self.username),
        )
        .await
        {
            warn!("IRC: failed to send USER: {e}");
            return Err(format!("Failed to send USER: {e}"));
        }

        // Store writer for send()
        {
            let mut guard = self.writer.lock().await;
            *guard = Some(writer);
        }

        let mut buf_reader = BufReader::new(reader);
        let mut line = String::new();
        let mut registered = false;
        let mut sasl_pending = self.sasl_password.is_some();

        while self.running.load(Ordering::SeqCst) {
            line.clear();
            let read_result = tokio::select! {
                r = tokio::time::timeout(READ_TIMEOUT, buf_reader.read_line(&mut line)) => r,
                _ = self.shutdown.notified() => break,
            };

            let n = match read_result {
                Ok(Ok(n)) => n,
                Ok(Err(e)) => {
                    warn!("IRC: read error: {e}");
                    break;
                }
                Err(_) => {
                    warn!("IRC: read timed out (no data for {READ_TIMEOUT:?})");
                    break;
                }
            };

            if n == 0 {
                warn!("IRC: connection closed by server");
                break;
            }

            let Some(msg) = IrcMessageParsed::parse(&line) else {
                continue;
            };

            match msg.command.as_str() {
                "PING" => {
                    let token = msg.params.first().map_or("", String::as_str);
                    let mut guard = self.writer.lock().await;
                    if let Some(ref mut w) = *guard {
                        let _ = Self::send_raw(w, &format!("PONG :{token}")).await;
                    }
                }

                // CAP responses for SASL
                "CAP" => {
                    if sasl_pending && msg.params.iter().any(|p| p.contains("sasl")) {
                        if msg.params.iter().any(|p| p.contains("ACK")) {
                            let mut guard = self.writer.lock().await;
                            if let Some(ref mut w) = *guard {
                                let _ = Self::send_raw(w, "AUTHENTICATE PLAIN").await;
                            }
                        } else if msg.params.iter().any(|p| p.contains("NAK")) {
                            warn!("IRC: server does not support SASL, continuing without it");
                            sasl_pending = false;
                            let mut guard = self.writer.lock().await;
                            if let Some(ref mut w) = *guard {
                                let _ = Self::send_raw(w, "CAP END").await;
                            }
                        }
                    }
                }

                "AUTHENTICATE" => {
                    if sasl_pending && msg.params.first().is_some_and(|p| p == "+") {
                        if let Some(password) = self.sasl_password.as_deref() {
                            let encoded = encode_sasl_plain(&current_nick, password);
                            let mut guard = self.writer.lock().await;
                            if let Some(ref mut w) = *guard {
                                let _ =
                                    Self::send_raw(w, &format!("AUTHENTICATE {encoded}")).await;
                            }
                        } else {
                            warn!("IRC: SASL requested but no password configured; aborting SASL");
                            sasl_pending = false;
                            let mut guard = self.writer.lock().await;
                            if let Some(ref mut w) = *guard {
                                let _ = Self::send_raw(w, "CAP END").await;
                            }
                        }
                    }
                }

                // RPL_SASLSUCCESS (903)
                "903" => {
                    sasl_pending = false;
                    let mut guard = self.writer.lock().await;
                    if let Some(ref mut w) = *guard {
                        let _ = Self::send_raw(w, "CAP END").await;
                    }
                }

                // SASL failure
                "904" | "905" | "906" | "907" => {
                    warn!("IRC: SASL authentication failed ({})", msg.command);
                    sasl_pending = false;
                    let mut guard = self.writer.lock().await;
                    if let Some(ref mut w) = *guard {
                        let _ = Self::send_raw(w, "CAP END").await;
                    }
                }

                // RPL_WELCOME — registration complete
                "001" => {
                    registered = true;
                    info!("IRC: registered as {current_nick}");

                    // NickServ authentication
                    if let Some(ref pass) = self.nickserv_password {
                        let mut guard = self.writer.lock().await;
                        if let Some(ref mut w) = *guard {
                            let _ = Self::send_raw(
                                w,
                                &format!("PRIVMSG NickServ :IDENTIFY {pass}"),
                            )
                            .await;
                        }
                    }

                    // Join channels
                    for chan in &self.channels {
                        let mut guard = self.writer.lock().await;
                        if let Some(ref mut w) = *guard {
                            let _ = Self::send_raw(w, &format!("JOIN {chan}")).await;
                        }
                    }
                }

                // ERR_NICKNAMEINUSE (433)
                "433" => {
                    let alt = format!("{current_nick}_");
                    warn!("IRC: nickname {current_nick} is in use, trying {alt}");
                    let mut guard = self.writer.lock().await;
                    if let Some(ref mut w) = *guard {
                        let _ = Self::send_raw(w, &format!("NICK {alt}")).await;
                    }
                    current_nick = alt;
                }

                "PRIVMSG" => {
                    if !registered {
                        continue;
                    }

                    let target = msg.params.first().map_or("", String::as_str);
                    let text = msg.params.get(1).map_or("", String::as_str);
                    let sender_nick = msg.nick().unwrap_or("unknown");

                    // Skip service messages
                    if sender_nick.eq_ignore_ascii_case("NickServ")
                        || sender_nick.eq_ignore_ascii_case("ChanServ")
                    {
                        continue;
                    }

                    if !self.is_user_allowed(sender_nick) {
                        continue;
                    }

                    // Determine reply target
                    let is_channel = target.starts_with('#') || target.starts_with('&');
                    let reply_target = if is_channel {
                        target.to_string()
                    } else {
                        sender_nick.to_string()
                    };

                    let content = if is_channel {
                        format!("{IRC_STYLE_PREFIX}<{sender_nick}> {text}")
                    } else {
                        format!("{IRC_STYLE_PREFIX}{text}")
                    };

                    let seq = MSG_SEQ.fetch_add(1, Ordering::Relaxed);
                    let msg_id =
                        format!("irc_{}_{seq}", chrono::Utc::now().timestamp_millis());

                    let normalized = NormalizedMessage {
                        id: Uuid::new_v4(),
                        session_key: clawdesk_types::session::SessionKey::new(
                            ChannelId::Irc,
                            &reply_target,
                        ),
                        body: content,
                        body_for_agent: None,
                        sender: SenderIdentity {
                            id: sender_nick.to_string(),
                            display_name: sender_nick.to_string(),
                            channel: ChannelId::Irc,
                        },
                        media: vec![],
                        reply_context: None,
                        origin: clawdesk_types::message::MessageOrigin::Irc {
                            target: reply_target,
                            sender_nick: sender_nick.to_string(),
                            is_channel,
                        },
                        timestamp: Utc::now(),
                    };

                    sink.on_message(normalized).await;
                }

                // ERR_PASSWDMISMATCH (464)
                "464" => {
                    warn!("IRC: password mismatch");
                    break;
                }

                _ => {}
            }
        }

        // Cleanup: send QUIT and clear writer
        {
            let mut guard = self.writer.lock().await;
            if let Some(ref mut w) = *guard {
                let _ = Self::send_raw(w, "QUIT :ClawDesk shutting down").await;
            }
            *guard = None;
        }

        info!("IRC channel stopped");
        Ok(())
    }

    async fn send(&self, message: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let mut guard = self.writer.lock().await;
        let writer = guard
            .as_mut()
            .ok_or_else(|| "IRC not connected".to_string())?;

        // Extract reply target from origin
        let recipient = match &message.origin {
            clawdesk_types::message::MessageOrigin::Irc { target, .. } => target.clone(),
            _ => return Err("IRC send: invalid origin (not IRC)".to_string()),
        };

        // Calculate safe payload size:
        // 512 - sender prefix (~64) - "PRIVMSG " - target - " :" - "\r\n"
        let overhead = SENDER_PREFIX_RESERVE + 10 + recipient.len() + 2;
        let max_payload = 512_usize.saturating_sub(overhead);
        let chunks = split_message(&message.body, max_payload);

        for chunk in chunks {
            Self::send_raw(writer, &format!("PRIVMSG {recipient} :{chunk}")).await?;
        }

        Ok(DeliveryReceipt {
            channel: ChannelId::Irc,
            message_id: Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        info!("IRC channel stopping...");
        self.running.store(false, Ordering::SeqCst);
        self.shutdown.notify_waiters();
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    // ── IRC message parsing ──────────────────────────────────

    #[test]
    fn parse_privmsg_with_prefix() {
        let msg = IrcMessageParsed::parse(":nick!user@host PRIVMSG #channel :Hello world").unwrap();
        assert_eq!(msg.prefix.as_deref(), Some("nick!user@host"));
        assert_eq!(msg.command, "PRIVMSG");
        assert_eq!(msg.params, vec!["#channel", "Hello world"]);
    }

    #[test]
    fn parse_privmsg_dm() {
        let msg = IrcMessageParsed::parse(":alice!a@host PRIVMSG botname :hi there").unwrap();
        assert_eq!(msg.command, "PRIVMSG");
        assert_eq!(msg.params, vec!["botname", "hi there"]);
        assert_eq!(msg.nick(), Some("alice"));
    }

    #[test]
    fn parse_ping() {
        let msg = IrcMessageParsed::parse("PING :server.example.com").unwrap();
        assert!(msg.prefix.is_none());
        assert_eq!(msg.command, "PING");
        assert_eq!(msg.params, vec!["server.example.com"]);
    }

    #[test]
    fn parse_numeric_reply() {
        let msg =
            IrcMessageParsed::parse(":server 001 botname :Welcome to the IRC network").unwrap();
        assert_eq!(msg.prefix.as_deref(), Some("server"));
        assert_eq!(msg.command, "001");
        assert_eq!(msg.params, vec!["botname", "Welcome to the IRC network"]);
    }

    #[test]
    fn parse_no_trailing() {
        let msg = IrcMessageParsed::parse(":server 433 * botname").unwrap();
        assert_eq!(msg.command, "433");
        assert_eq!(msg.params, vec!["*", "botname"]);
    }

    #[test]
    fn parse_cap_ack() {
        let msg = IrcMessageParsed::parse(":server CAP * ACK :sasl").unwrap();
        assert_eq!(msg.command, "CAP");
        assert_eq!(msg.params, vec!["*", "ACK", "sasl"]);
    }

    #[test]
    fn parse_empty_line_returns_none() {
        assert!(IrcMessageParsed::parse("").is_none());
        assert!(IrcMessageParsed::parse("\r\n").is_none());
    }

    #[test]
    fn parse_strips_crlf() {
        let msg = IrcMessageParsed::parse("PING :test\r\n").unwrap();
        assert_eq!(msg.params, vec!["test"]);
    }

    #[test]
    fn parse_command_uppercase() {
        let msg = IrcMessageParsed::parse("ping :test").unwrap();
        assert_eq!(msg.command, "PING");
    }

    #[test]
    fn nick_extraction_full_prefix() {
        let msg = IrcMessageParsed::parse(":nick!user@host PRIVMSG #ch :msg").unwrap();
        assert_eq!(msg.nick(), Some("nick"));
    }

    #[test]
    fn nick_extraction_nick_only() {
        let msg = IrcMessageParsed::parse(":server 001 bot :Welcome").unwrap();
        assert_eq!(msg.nick(), Some("server"));
    }

    #[test]
    fn nick_extraction_no_prefix() {
        let msg = IrcMessageParsed::parse("PING :token").unwrap();
        assert_eq!(msg.nick(), None);
    }

    // ── SASL encoding ──────────────────────────────────

    #[test]
    fn sasl_plain_encoding() {
        let encoded = encode_sasl_plain("nick", "password");
        // \0nick\0password → base64
        assert!(!encoded.is_empty());
        assert!(encoded.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='));
    }

    #[test]
    fn sasl_roundtrip() {
        let encoded = encode_sasl_plain("bot", "secret");
        // Manual verification: \0bot\0secret = [0, 98, 111, 116, 0, 115, 101, 99, 114, 101, 116]
        assert_eq!(encoded, "AGJvdABzZWNyZXQ=");
    }

    // ── Message splitting ──────────────────────────────────

    #[test]
    fn split_short_message() {
        let chunks = split_message("hello", 100);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn split_multiline() {
        let chunks = split_message("line1\nline2\nline3", 100);
        assert_eq!(chunks, vec!["line1", "line2", "line3"]);
    }

    #[test]
    fn split_skips_empty_lines() {
        let chunks = split_message("line1\n\nline2", 100);
        assert_eq!(chunks, vec!["line1", "line2"]);
    }

    #[test]
    fn split_long_line() {
        let long = "a".repeat(600);
        let chunks = split_message(&long, 400);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 400);
        assert_eq!(chunks[1].len(), 200);
    }

    #[test]
    fn split_unicode_boundary() {
        // 4-byte emoji
        let msg = "🦀".repeat(50); // 200 bytes
        let chunks = split_message(&msg, 100);
        for chunk in &chunks {
            // Each chunk must be valid UTF-8
            assert!(chunk.is_char_boundary(0));
            assert!(chunk.len() <= 100 || chunk.len() == 4); // may exceed by one char
        }
    }

    #[test]
    fn split_empty_message() {
        let chunks = split_message("", 100);
        assert_eq!(chunks, vec![""]);
    }

    #[test]
    fn split_zero_max_bytes() {
        let chunks = split_message("line1\nline2", 0);
        assert_eq!(chunks, vec!["line1 line2"]);
    }

    // ── User allowlist ──────────────────────────────────

    #[test]
    fn wildcard_allows_all_users() {
        let ch = IrcChannel::new(IrcChannelConfig {
            server: "irc.test".into(),
            port: 6697,
            nickname: "bot".into(),
            username: None,
            channels: vec![],
            allowed_users: vec!["*".into()],
            server_password: None,
            nickserv_password: None,
            sasl_password: None,
            verify_tls: true,
        });
        assert!(ch.is_user_allowed("anyone"));
        assert!(ch.is_user_allowed("AnyOne"));
    }

    #[test]
    fn specific_users_only() {
        let ch = IrcChannel::new(IrcChannelConfig {
            server: "irc.test".into(),
            port: 6697,
            nickname: "bot".into(),
            username: None,
            channels: vec![],
            allowed_users: vec!["alice".into(), "bob".into()],
            server_password: None,
            nickserv_password: None,
            sasl_password: None,
            verify_tls: true,
        });
        assert!(ch.is_user_allowed("alice"));
        assert!(ch.is_user_allowed("Alice")); // case insensitive
        assert!(ch.is_user_allowed("bob"));
        assert!(!ch.is_user_allowed("charlie"));
    }

    #[test]
    fn empty_allowlist_blocks_all() {
        let ch = IrcChannel::new(IrcChannelConfig {
            server: "irc.test".into(),
            port: 6697,
            nickname: "bot".into(),
            username: None,
            channels: vec![],
            allowed_users: vec![],
            server_password: None,
            nickserv_password: None,
            sasl_password: None,
            verify_tls: true,
        });
        assert!(!ch.is_user_allowed("anyone"));
    }

    // ── Channel trait ──────────────────────────────────

    #[test]
    fn channel_id() {
        let ch = IrcChannel::new(IrcChannelConfig {
            server: "irc.test".into(),
            port: 6697,
            nickname: "bot".into(),
            username: None,
            channels: vec![],
            allowed_users: vec![],
            server_password: None,
            nickserv_password: None,
            sasl_password: None,
            verify_tls: true,
        });
        assert_eq!(ch.id(), ChannelId::Irc);
    }

    #[test]
    fn channel_meta() {
        let ch = IrcChannel::new(IrcChannelConfig {
            server: "irc.test".into(),
            port: 6697,
            nickname: "bot".into(),
            username: None,
            channels: vec![],
            allowed_users: vec![],
            server_password: None,
            nickserv_password: None,
            sasl_password: None,
            verify_tls: true,
        });
        let meta = ch.meta();
        assert_eq!(meta.display_name, "IRC");
        assert!(meta.supports_groups);
        assert!(!meta.supports_streaming);
        assert!(meta.max_message_length.is_some());
    }

    #[test]
    fn username_defaults_to_nickname() {
        let ch = IrcChannel::new(IrcChannelConfig {
            server: "irc.test".into(),
            port: 6697,
            nickname: "mybot".into(),
            username: None,
            channels: vec![],
            allowed_users: vec![],
            server_password: None,
            nickserv_password: None,
            sasl_password: None,
            verify_tls: true,
        });
        assert_eq!(ch.username, "mybot");
    }

    #[test]
    fn custom_username() {
        let ch = IrcChannel::new(IrcChannelConfig {
            server: "irc.test".into(),
            port: 6697,
            nickname: "mybot".into(),
            username: Some("custom_user".into()),
            channels: vec![],
            allowed_users: vec![],
            server_password: None,
            nickserv_password: None,
            sasl_password: None,
            verify_tls: true,
        });
        assert_eq!(ch.username, "custom_user");
    }
}
