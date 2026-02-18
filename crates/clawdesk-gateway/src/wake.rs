//! Local wake channel for child-process to gateway notifications.
//!
//! This exposes a local-only ingest endpoint (Unix socket) intended for
//! scripts and CLI harness sessions to deliver completion/progress signals.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::AsyncReadExt;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;

#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};

/// Wake message payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WakeMessage {
    pub text: String,
    pub priority: WakePriority,
    pub mode: WakeMode,
    pub channel: Option<String>,
    pub metadata: HashMap<String, String>,
    pub created_at: DateTime<Utc>,
}

impl WakeMessage {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            priority: WakePriority::Normal,
            mode: WakeMode::Now,
            channel: None,
            metadata: HashMap::new(),
            created_at: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WakePriority {
    Normal,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WakeMode {
    Now,
    Queue,
}

/// Wake listener config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WakeConfig {
    pub socket_path: PathBuf,
    pub max_payload_bytes: usize,
    pub bucket_capacity: f64,
    pub bucket_refill_per_sec: f64,
}

impl Default for WakeConfig {
    fn default() -> Self {
        Self {
            socket_path: default_wake_socket_path(),
            max_payload_bytes: 16 * 1024,
            bucket_capacity: 10.0,
            bucket_refill_per_sec: 1.0,
        }
    }
}

/// Wake server.
pub struct WakeServer {
    config: WakeConfig,
    bucket: Arc<Mutex<TokenBucket>>,
}

impl WakeServer {
    pub fn new(config: WakeConfig) -> Self {
        Self {
            bucket: Arc::new(Mutex::new(TokenBucket::new(
                config.bucket_capacity,
                config.bucket_refill_per_sec,
            ))),
            config,
        }
    }

    #[cfg(unix)]
    pub async fn run(
        &self,
        cancel: CancellationToken,
        tx: mpsc::Sender<WakeMessage>,
    ) -> Result<(), WakeError> {
        prepare_socket_path(&self.config.socket_path)?;
        let listener = UnixListener::bind(&self.config.socket_path)?;

        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                accepted = listener.accept() => {
                    let (stream, _) = accepted?;
                    let tx_clone = tx.clone();
                    let bucket = Arc::clone(&self.bucket);
                    let max_payload = self.config.max_payload_bytes;
                    tokio::spawn(async move {
                        let _ = handle_connection(stream, tx_clone, bucket, max_payload).await;
                    });
                }
            }
        }
        Ok(())
    }

    #[cfg(not(unix))]
    pub async fn run(
        &self,
        _cancel: CancellationToken,
        _tx: mpsc::Sender<WakeMessage>,
    ) -> Result<(), WakeError> {
        Err(WakeError::UnsupportedPlatform)
    }
}

#[cfg(unix)]
async fn handle_connection(
    mut stream: UnixStream,
    tx: mpsc::Sender<WakeMessage>,
    bucket: Arc<Mutex<TokenBucket>>,
    max_payload_bytes: usize,
) -> Result<(), WakeError> {
    let mut data = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];

    loop {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        if data.len() + n > max_payload_bytes {
            return Err(WakeError::PayloadTooLarge {
                max: max_payload_bytes,
            });
        }
        data.extend_from_slice(&chunk[..n]);
    }

    let raw = String::from_utf8_lossy(&data).trim().to_string();
    if raw.is_empty() {
        return Err(WakeError::Parse("empty payload".to_string()));
    }
    let msg = parse_wake_payload(&raw)?;

    {
        let mut guard = bucket.lock().await;
        if !guard.allow(1.0) {
            return Err(WakeError::RateLimited);
        }
    }

    tx.send(msg)
        .await
        .map_err(|e| WakeError::Channel(format!("wake receiver dropped: {e}")))?;
    Ok(())
}

fn parse_wake_payload(raw: &str) -> Result<WakeMessage, WakeError> {
    if raw.starts_with('{') {
        #[derive(Debug, Deserialize)]
        struct Wire {
            text: String,
            priority: Option<WakePriority>,
            mode: Option<WakeMode>,
            channel: Option<String>,
            metadata: Option<HashMap<String, String>>,
        }

        let wire: Wire = serde_json::from_str(raw)
            .map_err(|e| WakeError::Parse(format!("invalid json payload: {e}")))?;
        if wire.text.trim().is_empty() {
            return Err(WakeError::Parse("text cannot be empty".to_string()));
        }
        Ok(WakeMessage {
            text: wire.text,
            priority: wire.priority.unwrap_or(WakePriority::Normal),
            mode: wire.mode.unwrap_or(WakeMode::Now),
            channel: wire.channel,
            metadata: wire.metadata.unwrap_or_default(),
            created_at: Utc::now(),
        })
    } else {
        Ok(WakeMessage::new(raw))
    }
}

fn default_wake_socket_path() -> PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime_dir).join("clawdesk").join("wake.sock");
    }
    PathBuf::from("/tmp").join("clawdesk-wake.sock")
}

#[cfg(unix)]
fn prepare_socket_path(path: &Path) -> Result<(), WakeError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if path.exists() {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

#[cfg(not(unix))]
fn prepare_socket_path(_path: &Path) -> Result<(), WakeError> {
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum WakeError {
    #[error("unsupported platform")]
    UnsupportedPlatform,
    #[error("payload too large (max {max} bytes)")]
    PayloadTooLarge { max: usize },
    #[error("failed to parse payload: {0}")]
    Parse(String),
    #[error("rate limited")]
    RateLimited,
    #[error("channel error: {0}")]
    Channel(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug)]
struct TokenBucket {
    capacity: f64,
    refill_per_sec: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    fn new(capacity: f64, refill_per_sec: f64) -> Self {
        Self {
            capacity,
            refill_per_sec,
            tokens: capacity,
            last: Instant::now(),
        }
    }

    fn allow(&mut self, cost: f64) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        if self.tokens >= cost {
            self.tokens -= cost;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_text_payload() {
        let msg = parse_wake_payload("done!").unwrap();
        assert_eq!(msg.text, "done!");
        assert_eq!(msg.priority, WakePriority::Normal);
        assert_eq!(msg.mode, WakeMode::Now);
    }

    #[test]
    fn parse_json_payload() {
        let raw = r#"{"text":"refactor complete","priority":"high","mode":"queue","channel":"telegram"}"#;
        let msg = parse_wake_payload(raw).unwrap();
        assert_eq!(msg.text, "refactor complete");
        assert_eq!(msg.priority, WakePriority::High);
        assert_eq!(msg.mode, WakeMode::Queue);
        assert_eq!(msg.channel.as_deref(), Some("telegram"));
    }

    #[test]
    fn token_bucket_rate_limits() {
        let mut b = TokenBucket::new(2.0, 0.0);
        assert!(b.allow(1.0));
        assert!(b.allow(1.0));
        assert!(!b.allow(1.0));
    }
}

