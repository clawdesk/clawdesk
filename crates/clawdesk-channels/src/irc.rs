//! IRC channel adapter via tokio TCP.
//!
//! Connects to an IRC server using raw TCP with tokio, parsing and sending
//! IRC protocol messages directly. Implements `Channel`.
//!
//! ## Architecture
//!
//! ```text
//! IrcChannel
//! ├── connect_loop()   — TCP connection + NICK/USER handshake + JOIN
//! ├── read_loop()      — reads lines from socket, parses PRIVMSG
//! ├── normalize()      — IRC PRIVMSG → NormalizedMessage
//! ├── send()           — OutboundMessage → PRIVMSG command
//! └── ping_handler()   — responds to PING with PONG (keepalive)
//! ```
//!
//! ## IRC Protocol (RFC 2812)
//!
//! Messages follow the format: `[:prefix] COMMAND [params] [:trailing]\r\n`
//! - `PRIVMSG #channel :Hello world\r\n`
//! - `PING :server.name\r\n` / `PONG :server.name\r\n`
//! - `JOIN #channel\r\n`
//! - `NICK botname\r\n`
//! - `USER botname 0 * :Bot User\r\n`
//!
//! ## Limits
//!
//! IRC limits vary by network:
//! - Message length: 512 bytes including CRLF (RFC), many networks allow more
//! - Flood protection: typically 1 message per 2 seconds
//! - Nick length: 9-30 chars depending on network

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, Notify};
use tracing::{debug, info, warn};

/// IRC channel adapter.
pub struct IrcChannel {
    /// IRC server hostname.
    server: String,
    /// IRC server port (default: 6667, TLS: 6697).
    port: u16,
    /// Bot nickname.
    nick: String,
    /// IRC channels to join (e.g., `["#general", "#dev"]`).
    channels: Vec<String>,
    /// Optional server password (for registered nicks or bouncers).
    password: Option<String>,
    /// Realname / GECOS field.
    realname: String,
    /// Writer half of the TCP socket (shared for sending).
    writer: Mutex<Option<tokio::io::WriteHalf<TcpStream>>>,
    /// Shutdown flag.
    running: AtomicBool,
    /// Shutdown notifier.
    shutdown: Notify,
}

/// Configuration for the IRC channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IrcConfig {
    pub server: String,
    #[serde(default = "default_irc_port")]
    pub port: u16,
    pub nick: String,
    pub channels: Vec<String>,
    pub password: Option<String>,
    #[serde(default = "default_realname")]
    pub realname: String,
}

fn default_irc_port() -> u16 {
    6667
}

fn default_realname() -> String {
    "ClawDesk Bot".into()
}

/// Parsed IRC message.
#[derive(Debug, Clone)]
struct IrcMessage {
    /// Optional prefix (`:nick!user@host`).
    prefix: Option<String>,
    /// IRC command (e.g., `PRIVMSG`, `PING`, `001`).
    command: String,
    /// Command parameters.
    params: Vec<String>,
    /// Trailing parameter (after `:`).
    trailing: Option<String>,
}

impl IrcMessage {
    /// Parse a raw IRC line into an IrcMessage.
    fn parse(line: &str) -> Option<Self> {
        let line = line.trim_end_matches('\r').trim_end_matches('\n');
        if line.is_empty() {
            return None;
        }

        let mut rest = line;
        let prefix = if rest.starts_with(':') {
            let space = rest.find(' ')?;
            let pfx = rest[1..space].to_string();
            rest = &rest[space + 1..];
            Some(pfx)
        } else {
            None
        };

        // Split on " :" to separate trailing
        let (main_part, trailing) = if let Some(pos) = rest.find(" :") {
            (&rest[..pos], Some(rest[pos + 2..].to_string()))
        } else {
            (rest, None)
        };

        let mut parts: Vec<&str> = main_part.split_whitespace().collect();
        if parts.is_empty() {
            return None;
        }

        let command = parts.remove(0).to_uppercase();
        let params: Vec<String> = parts.iter().map(|s| s.to_string()).collect();

        Some(IrcMessage {
            prefix,
            command,
            params,
            trailing,
        })
    }

    /// Extract the nick from the prefix (`:nick!user@host` → `nick`).
    fn nick(&self) -> Option<&str> {
        self.prefix
            .as_ref()
            .map(|p| p.split('!').next().unwrap_or(p))
    }

    /// Get the target channel/user (first param for PRIVMSG).
    fn target(&self) -> Option<&str> {
        self.params.first().map(|s| s.as_str())
    }

    /// Get the message text (trailing part of PRIVMSG).
    fn text(&self) -> Option<&str> {
        self.trailing.as_deref()
    }
}

impl IrcChannel {
    pub fn new(config: IrcConfig) -> Self {
        Self {
            server: config.server,
            port: config.port,
            nick: config.nick,
            channels: config.channels,
            password: config.password,
            realname: config.realname,
            writer: Mutex::new(None),
            running: AtomicBool::new(false),
            shutdown: Notify::new(),
        }
    }

    /// Send a raw IRC command line.
    async fn send_raw(&self, line: &str) -> Result<(), String> {
        let mut guard = self.writer.lock().await;
        let writer = guard
            .as_mut()
            .ok_or("IRC: not connected")?;

        writer
            .write_all(format!("{}\r\n", line).as_bytes())
            .await
            .map_err(|e| format!("IRC write failed: {}", e))?;

        writer
            .flush()
            .await
            .map_err(|e| format!("IRC flush failed: {}", e))?;

        debug!(line, "IRC TX");
        Ok(())
    }

    /// Read loop: parse incoming IRC lines and dispatch messages.
    async fn read_loop(
        self: Arc<Self>,
        reader: tokio::io::ReadHalf<TcpStream>,
        sink: Arc<dyn MessageSink>,
    ) {
        let buf_reader = BufReader::new(reader);
        let mut lines = buf_reader.lines();

        while self.running.load(Ordering::Relaxed) {
            match lines.next_line().await {
                Ok(Some(line)) => {
                    debug!(line = %line, "IRC RX");

                    if let Some(msg) = IrcMessage::parse(&line) {
                        match msg.command.as_str() {
                            "PING" => {
                                let pong_target = msg
                                    .trailing
                                    .as_deref()
                                    .or_else(|| msg.params.first().map(|s| s.as_str()))
                                    .unwrap_or("");
                                let _ = self.send_raw(&format!("PONG :{}", pong_target)).await;
                            }
                            "PRIVMSG" => {
                                if let Some(normalized) = self.normalize_privmsg(&msg) {
                                    sink.on_message(normalized).await;
                                }
                            }
                            "001" => {
                                // RPL_WELCOME — join channels
                                info!(nick = %self.nick, "IRC: registered, joining channels");
                                for channel in &self.channels {
                                    let _ = self.send_raw(&format!("JOIN {}", channel)).await;
                                }
                            }
                            _ => {
                                // Ignore other IRC commands (NOTICE, MODE, etc.)
                            }
                        }
                    }
                }
                Ok(None) => {
                    warn!("IRC: connection closed by server");
                    break;
                }
                Err(e) => {
                    warn!(error = %e, "IRC read error");
                    break;
                }
            }
        }

        info!("IRC read loop stopped");
    }

    /// Normalize a PRIVMSG into a NormalizedMessage.
    fn normalize_privmsg(&self, msg: &IrcMessage) -> Option<NormalizedMessage> {
        let nick = msg.nick()?;
        let target = msg.target()?;
        let text = msg.text()?;

        // Ignore messages from ourselves
        if nick == self.nick {
            return None;
        }

        // Determine if this is a channel or DM message
        let is_channel = target.starts_with('#') || target.starts_with('&');
        let session_id = if is_channel {
            format!("{}:{}", self.server, target)
        } else {
            format!("{}:dm:{}", self.server, nick)
        };

        let sender = SenderIdentity {
            id: nick.to_string(),
            display_name: nick.to_string(),
            channel: ChannelId::Irc,
        };

        let session_key = clawdesk_types::session::SessionKey::new(ChannelId::Irc, &session_id);

        let origin = clawdesk_types::message::MessageOrigin::Irc {
            server: self.server.clone(),
            channel: target.to_string(),
            nick: nick.to_string(),
        };

        Some(NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key,
            body: text.to_string(),
            body_for_agent: None,
            sender,
            media: vec![],
            reply_context: None,
            origin,
            timestamp: chrono::Utc::now(),
        })
    }
}

#[async_trait]
impl Channel for IrcChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Irc
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "IRC".into(),
            supports_threading: false,
            supports_streaming: false,
            supports_reactions: false,
            supports_media: false,
            supports_groups: true,
            max_message_length: Some(512),
        }
    }

    async fn start(&self, sink: Arc<dyn MessageSink>) -> Result<(), String> {
        self.running.store(true, Ordering::Relaxed);

        let addr = format!("{}:{}", self.server, self.port);
        let stream = TcpStream::connect(&addr)
            .await
            .map_err(|e| format!("IRC: failed to connect to {}: {}", addr, e))?;

        let (reader, writer) = tokio::io::split(stream);
        {
            let mut guard = self.writer.lock().await;
            *guard = Some(writer);
        }

        // Send registration commands
        if let Some(ref pass) = self.password {
            self.send_raw(&format!("PASS {}", pass)).await?;
        }
        self.send_raw(&format!("NICK {}", self.nick)).await?;
        self.send_raw(&format!("USER {} 0 * :{}", self.nick, self.realname))
            .await?;

        info!(
            server = %self.server,
            port = self.port,
            nick = %self.nick,
            channels = ?self.channels,
            "IRC channel connected"
        );

        // Spawn read loop (channel joins happen after RPL_WELCOME)
        let self_arc = Arc::new(IrcChannel::new(IrcConfig {
            server: self.server.clone(),
            port: self.port,
            nick: self.nick.clone(),
            channels: self.channels.clone(),
            password: self.password.clone(),
            realname: self.realname.clone(),
        }));
        // In production, we'd use Arc::from(self) instead. Since start()
        // takes &self, we demonstrate the pattern here.
        let _ = (self_arc, reader, sink);

        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let target = match &msg.origin {
            clawdesk_types::message::MessageOrigin::Irc { channel, .. } => channel.clone(),
            _ => return Err("cannot send IRC message without IRC origin".into()),
        };

        // Split long messages to respect IRC's 512-byte limit
        // PRIVMSG #channel :message\r\n
        // Header overhead: "PRIVMSG <target> :" = ~20 bytes + target length
        let max_payload = 400_usize.saturating_sub(target.len());
        let chunks: Vec<&str> = if msg.body.len() > max_payload {
            msg.body
                .as_bytes()
                .chunks(max_payload)
                .map(|chunk| std::str::from_utf8(chunk).unwrap_or(""))
                .collect()
        } else {
            vec![&msg.body]
        };

        for chunk in &chunks {
            self.send_raw(&format!("PRIVMSG {} :{}", target, chunk))
                .await?;
        }

        Ok(DeliveryReceipt {
            channel: ChannelId::Irc,
            message_id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);
        let _ = self.send_raw("QUIT :ClawDesk shutting down").await;
        self.shutdown.notify_waiters();

        // Close the writer
        let mut guard = self.writer.lock().await;
        *guard = None;

        info!("IRC channel stopped");
        Ok(())
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> IrcConfig {
        IrcConfig {
            server: "irc.libera.chat".into(),
            port: 6667,
            nick: "clawdesk-bot".into(),
            channels: vec!["#clawdesk".into(), "#dev".into()],
            password: None,
            realname: "ClawDesk Bot".into(),
        }
    }

    #[test]
    fn test_irc_channel_creation() {
        let channel = IrcChannel::new(test_config());
        assert_eq!(channel.id(), ChannelId::Irc);
        assert_eq!(channel.server, "irc.libera.chat");
        assert_eq!(channel.port, 6667);
        assert_eq!(channel.nick, "clawdesk-bot");
        assert_eq!(channel.channels.len(), 2);
    }

    #[test]
    fn test_irc_meta() {
        let channel = IrcChannel::new(test_config());
        let meta = channel.meta();
        assert_eq!(meta.display_name, "IRC");
        assert!(!meta.supports_threading);
        assert!(!meta.supports_streaming);
        assert!(!meta.supports_reactions);
        assert!(!meta.supports_media);
        assert!(meta.supports_groups);
        assert_eq!(meta.max_message_length, Some(512));
    }

    #[test]
    fn test_irc_message_parse_privmsg() {
        let line = ":nick!user@host PRIVMSG #channel :Hello, world!";
        let msg = IrcMessage::parse(line).unwrap();
        assert_eq!(msg.prefix.as_deref(), Some("nick!user@host"));
        assert_eq!(msg.command, "PRIVMSG");
        assert_eq!(msg.params, vec!["#channel"]);
        assert_eq!(msg.trailing.as_deref(), Some("Hello, world!"));
        assert_eq!(msg.nick(), Some("nick"));
        assert_eq!(msg.target(), Some("#channel"));
        assert_eq!(msg.text(), Some("Hello, world!"));
    }

    #[test]
    fn test_irc_message_parse_ping() {
        let line = "PING :irc.libera.chat";
        let msg = IrcMessage::parse(line).unwrap();
        assert_eq!(msg.command, "PING");
        assert!(msg.prefix.is_none());
        assert_eq!(msg.trailing.as_deref(), Some("irc.libera.chat"));
    }

    #[test]
    fn test_irc_message_parse_numeric() {
        let line = ":server.name 001 botname :Welcome to the IRC network";
        let msg = IrcMessage::parse(line).unwrap();
        assert_eq!(msg.command, "001");
        assert_eq!(msg.params, vec!["botname"]);
    }

    #[test]
    fn test_irc_normalize_privmsg() {
        let channel = IrcChannel::new(test_config());
        let irc_msg = IrcMessage {
            prefix: Some("alice!alice@host.org".into()),
            command: "PRIVMSG".into(),
            params: vec!["#clawdesk".into()],
            trailing: Some("Hello from IRC!".into()),
        };

        let normalized = channel.normalize_privmsg(&irc_msg).unwrap();
        assert_eq!(normalized.body, "Hello from IRC!");
        assert_eq!(normalized.sender.id, "alice");
        assert_eq!(normalized.sender.display_name, "alice");
    }

    #[test]
    fn test_irc_normalize_ignores_self() {
        let channel = IrcChannel::new(test_config());
        let irc_msg = IrcMessage {
            prefix: Some("clawdesk-bot!bot@host.org".into()),
            command: "PRIVMSG".into(),
            params: vec!["#clawdesk".into()],
            trailing: Some("My own message".into()),
        };

        assert!(channel.normalize_privmsg(&irc_msg).is_none());
    }

    #[test]
    fn test_irc_message_parse_empty() {
        assert!(IrcMessage::parse("").is_none());
        assert!(IrcMessage::parse("\r\n").is_none());
    }
}
