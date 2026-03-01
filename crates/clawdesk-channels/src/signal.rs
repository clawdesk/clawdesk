//! Signal Messenger channel adapter via signal-cli JSON-RPC.
//!
//! Communicates with Signal through the `signal-cli` daemon, which wraps
//! the Signal protocol (libsignal) and exposes a JSON-RPC interface.
//!
//! ## Architecture
//!
//! ```text
//! SignalChannel
//! ├── start(sink)  — connect to signal-cli JSON-RPC, subscribe to messages
//! ├── send(msg)    — call "send" RPC method
//! ├── stop()       — disconnect
//! └── Pairing via "link" RPC (generates QR code)
//! ```
//!
//! ## Protocol
//!
//! signal-cli exposes a Unix socket or TCP JSON-RPC interface:
//! - `receive`: poll for inbound messages
//! - `send`: send a message to a phone number or group
//! - `listGroups`: enumerate joined groups
//! - `link`: start device linking (returns QR code URI)

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, Pairing, PairingResult, PairingSession};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Configuration for Signal channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalConfig {
    /// Phone number registered with Signal (e.g., +1234567890).
    pub phone_number: String,
    /// signal-cli JSON-RPC endpoint (Unix socket or TCP).
    /// Default: "http://localhost:8080/api/v1/rpc"
    pub rpc_endpoint: String,
    /// Allowed phone numbers that can send messages to the bot.
    /// Empty = allow all.
    pub allowed_numbers: Vec<String>,
    /// Poll interval in seconds (default: 1).
    pub poll_interval_secs: u64,
}

impl Default for SignalConfig {
    fn default() -> Self {
        Self {
            phone_number: String::new(),
            rpc_endpoint: "http://localhost:8080/api/v1/rpc".into(),
            allowed_numbers: Vec::new(),
            poll_interval_secs: 1,
        }
    }
}

/// JSON-RPC request for signal-cli.
#[derive(Serialize)]
struct RpcRequest {
    jsonrpc: &'static str,
    method: String,
    params: serde_json::Value,
    id: u64,
}

/// JSON-RPC response.
#[derive(Deserialize)]
struct RpcResponse {
    result: Option<serde_json::Value>,
    error: Option<RpcError>,
}

#[derive(Deserialize)]
struct RpcError {
    message: String,
}

/// Inbound message from signal-cli.
#[derive(Deserialize)]
struct SignalMessage {
    #[serde(rename = "sourceNumber")]
    source_number: Option<String>,
    #[serde(rename = "sourceName")]
    source_name: Option<String>,
    #[serde(rename = "groupId")]
    group_id: Option<String>,
    message: Option<String>,
    timestamp: Option<i64>,
}

/// Signal Messenger channel.
pub struct SignalChannel {
    config: SignalConfig,
    client: reqwest::Client,
    sink: RwLock<Option<Arc<dyn MessageSink>>>,
    running: AtomicBool,
    rpc_id: std::sync::atomic::AtomicU64,
}

impl SignalChannel {
    pub fn new(config: SignalConfig) -> Self {
        Self {
            config,
            client: reqwest::Client::new(),
            sink: RwLock::new(None),
            running: AtomicBool::new(false),
            rpc_id: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Make a JSON-RPC call to signal-cli.
    async fn rpc_call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = self.rpc_id.fetch_add(1, Ordering::Relaxed);

        let request = RpcRequest {
            jsonrpc: "2.0",
            method: method.to_string(),
            params,
            id,
        };

        let resp = self
            .client
            .post(&self.config.rpc_endpoint)
            .json(&request)
            .send()
            .await
            .map_err(|e| format!("Signal RPC failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!("Signal RPC error: HTTP {}", resp.status()));
        }

        let rpc_resp: RpcResponse = resp
            .json()
            .await
            .map_err(|e| format!("Signal RPC parse error: {}", e))?;

        if let Some(err) = rpc_resp.error {
            return Err(format!("Signal RPC error: {}", err.message));
        }

        Ok(rpc_resp.result.unwrap_or(serde_json::Value::Null))
    }

    /// Process inbound messages from signal-cli.
    pub async fn process_messages(&self, messages: Vec<SignalMessage>) {
        let sink = self.sink.read().await;
        let sink = match *sink {
            Some(ref s) => s,
            None => return,
        };

        for signal_msg in messages {
            let text = match signal_msg.message {
                Some(t) if !t.is_empty() => t,
                _ => continue,
            };

            let source = signal_msg.source_number.unwrap_or_default();

            // Filter by allowed numbers
            if !self.config.allowed_numbers.is_empty()
                && !self.config.allowed_numbers.contains(&source)
            {
                debug!(from = %source, "ignoring message from non-allowlisted number");
                continue;
            }

            let session_target = signal_msg.group_id.clone().unwrap_or_else(|| source.clone());
            let msg = NormalizedMessage {
                id: Uuid::new_v4(),
                session_key: clawdesk_types::session::SessionKey::new(
                    ChannelId::Signal,
                    &session_target,
                ),
                body: text,
                body_for_agent: None,
                sender: SenderIdentity {
                    id: source.clone(),
                    display_name: signal_msg
                        .source_name
                        .unwrap_or_else(|| source.clone()),
                    channel: ChannelId::Signal,
                },
                media: vec![],
                artifact_refs: vec![],
                reply_context: None,
                origin: clawdesk_types::message::MessageOrigin::Signal {
                    phone_number: source,
                },
                timestamp: signal_msg
                    .timestamp
                    .map(|ts| {
                        chrono::DateTime::from_timestamp(ts / 1000, 0)
                            .unwrap_or_else(Utc::now)
                    })
                    .unwrap_or_else(Utc::now),
            };

            sink.on_message(msg).await;
        }
    }
}

#[async_trait]
impl Channel for SignalChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Signal
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Signal".into(),
            supports_threading: false,
            supports_streaming: false,
            supports_reactions: true,
            supports_media: true,
            supports_groups: true,
            max_message_length: None, // Signal has no hard text limit
        }
    }

    async fn start(&self, sink: Arc<dyn MessageSink>) -> Result<(), String> {
        *self.sink.write().await = Some(sink);
        self.running.store(true, Ordering::Release);
        info!(phone = %self.config.phone_number, "Signal channel started");
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let params = if let Some(ref group_id) = msg.thread_id {
            serde_json::json!({
                "account": self.config.phone_number,
                "groupId": group_id,
                "message": msg.body,
            })
        } else {
            // Direct message — need a recipient
            let recipient = msg
                .reply_to
                .as_deref()
                .ok_or("Signal send requires a recipient phone number")?;

            serde_json::json!({
                "account": self.config.phone_number,
                "recipient": [recipient],
                "message": msg.body,
            })
        };

        let result = self.rpc_call("send", params).await?;
        let timestamp = result["timestamp"]
            .as_i64()
            .unwrap_or(0)
            .to_string();

        Ok(DeliveryReceipt {
            channel: ChannelId::Signal,
            message_id: timestamp,
            timestamp: Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Release);
        *self.sink.write().await = None;
        info!("Signal channel stopped");
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[async_trait]
impl Pairing for SignalChannel {
    async fn start_pairing(&self) -> Result<PairingSession, String> {
        let result = self
            .rpc_call("link", serde_json::json!({"name": "ClawDesk"}))
            .await?;

        let qr_uri = result["uri"].as_str().map(|s| s.to_string());

        Ok(PairingSession {
            session_id: uuid::Uuid::new_v4().to_string(),
            qr_code: qr_uri,
            instructions: "Scan the QR code in Signal Desktop or Mobile → Linked Devices".into(),
        })
    }

    async fn complete_pairing(&self, code: &str) -> Result<PairingResult, String> {
        let result = self
            .rpc_call(
                "finishLink",
                serde_json::json!({
                    "deviceName": "ClawDesk",
                    "verificationCode": code,
                }),
            )
            .await;

        match result {
            Ok(_) => Ok(PairingResult::Success {
                device_id: "linked".to_string(),
            }),
            Err(e) => Ok(PairingResult::Failed { reason: e }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_meta() {
        let ch = SignalChannel::new(SignalConfig::default());
        assert_eq!(ch.id(), ChannelId::Signal);
        let meta = ch.meta();
        assert!(!meta.supports_threading);
        assert!(meta.supports_reactions);
        assert_eq!(meta.display_name, "Signal");
    }

    #[test]
    fn signal_config_default() {
        let cfg = SignalConfig::default();
        assert_eq!(cfg.poll_interval_secs, 1);
        assert!(cfg.rpc_endpoint.contains("8080"));
    }
}
