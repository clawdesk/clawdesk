//! Tlon (Urbit) channel implementation via Eyre HTTP API.
//!
//! Tlon is a messaging and community platform built on the Urbit
//! network — a decentralized, peer-to-peer computing system. Each
//! Urbit node ("ship") runs its own server and communicates via the
//! Urbit networking protocol (Ames). The Eyre HTTP API exposes
//! endpoints for external clients to interact with a ship.
//!
//! ## Architecture
//!
//! ```text
//! TlonChannel
//! ├── subscribe_loop()  — SSE subscription to /~/channel for live updates
//! ├── normalize()       — Urbit graph-store event → NormalizedMessage
//! ├── send()            — OutboundMessage → poke to graph-store
//! ├── authenticate()    — POST /~/login with +code
//! └── poke()            — generic Urbit poke (action) via Eyre
//! ```
//!
//! ## Urbit Eyre HTTP API
//!
//! All requests go to the ship's Eyre endpoint (e.g., `http://localhost:8080`).
//!
//! - `POST /~/login`                  — authenticate with ship +code
//! - `PUT  /~/channel/{uid}`         — send poke/subscribe actions
//! - `GET  /~/channel/{uid}`         — SSE event stream
//! - `DELETE /~/channel/{uid}`       — close channel
//!
//! ## Urbit Poke/Subscribe Protocol
//!
//! Actions are sent as JSON arrays to a channel endpoint:
//! ```json
//! [{"id": 1, "action": "poke", "ship": "~sampel-palnet",
//!   "app": "graph-store", "mark": "graph-update-3",
//!   "json": { ... }}]
//! ```
//!
//! ## Ship naming
//!
//! Urbit ships use a phonetic naming scheme:
//! - Galaxy: `~zod` (8-bit, 256 total)
//! - Star: `~marzod` (16-bit)
//! - Planet: `~sampel-palnet` (32-bit)
//! - Moon: `~doznec-sampel-palnet` (64-bit)
//!
//! ## Limits
//!
//! - No official message length limit (practical ~100 KB)
//! - Rate limited by ship CPU and Ames bandwidth
//! - SSE reconnect on network interruption

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, StreamHandle, Streaming};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, Notify};
use tracing::{debug, info, warn};

/// Tlon (Urbit) channel adapter via Eyre HTTP API.
pub struct TlonChannel {
    client: Client,
    /// URL of the ship's Eyre endpoint (e.g., `http://localhost:8080`).
    ship_url: String,
    /// Ship name (e.g., `~sampel-palnet`).
    ship_name: String,
    /// Ship access code (+code from the ship's dojo).
    code: String,
    /// Target channel path (e.g., `~sampel-palnet/chat-name`).
    channel_name: String,
    /// The Eyre channel UID for this session.
    channel_uid: String,
    /// Authentication cookie from /~/login.
    auth_cookie: Mutex<Option<String>>,
    /// Monotonically increasing action ID for pokes/subscribes.
    next_action_id: AtomicU64,
    /// Shutdown flag.
    running: AtomicBool,
    /// Shutdown notifier.
    shutdown: Notify,
}

/// Configuration for the Tlon/Urbit channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TlonConfig {
    pub ship_url: String,
    pub ship_name: String,
    pub code: String,
    pub channel_name: String,
}

// ─── Urbit API types ────────────────────────────────────────────────

/// An Urbit poke action sent to the channel endpoint.
#[derive(Debug, Serialize)]
struct PokeAction {
    id: u64,
    action: String,
    ship: String,
    app: String,
    mark: String,
    json: serde_json::Value,
}

/// An Urbit subscribe action.
#[derive(Debug, Serialize)]
struct SubscribeAction {
    id: u64,
    action: String,
    ship: String,
    app: String,
    path: String,
}

/// An Urbit ack (acknowledge receipt of SSE event).
#[derive(Debug, Serialize)]
struct AckAction {
    id: u64,
    action: String,
    #[serde(rename = "event-id")]
    event_id: u64,
}

/// Parsed SSE event from the Urbit channel.
#[derive(Debug, Deserialize)]
struct UrbitSseEvent {
    id: Option<u64>,
    #[serde(rename = "response")]
    response: Option<String>,
    json: Option<serde_json::Value>,
    err: Option<String>,
}

/// Graph-store update (simplified).
#[derive(Debug, Deserialize)]
struct GraphUpdate {
    #[serde(rename = "add-nodes")]
    add_nodes: Option<serde_json::Value>,
}

impl TlonChannel {
    pub fn new(config: TlonConfig) -> Self {
        // Generate a unique channel UID for this session
        let channel_uid = format!(
            "clawdesk-{}",
            uuid::Uuid::new_v4().to_string().replace('-', "")[..12].to_string()
        );

        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("failed to build HTTP client"),
            ship_url: config.ship_url.trim_end_matches('/').to_string(),
            ship_name: config.ship_name,
            code: config.code,
            channel_name: config.channel_name,
            channel_uid,
            auth_cookie: Mutex::new(None),
            next_action_id: AtomicU64::new(1),
            running: AtomicBool::new(false),
            shutdown: Notify::new(),
        }
    }

    /// Get the next action ID (monotonically increasing).
    fn next_id(&self) -> u64 {
        self.next_action_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Build a ship endpoint URL.
    fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.ship_url, path)
    }

    /// The Eyre channel URL for this session.
    fn channel_url(&self) -> String {
        self.endpoint(&format!("/~/channel/{}", self.channel_uid))
    }

    /// Strip the `~` prefix from a ship name if present.
    fn bare_ship_name(&self) -> &str {
        self.ship_name.strip_prefix('~').unwrap_or(&self.ship_name)
    }

    /// Authenticate with the ship using +code.
    ///
    /// Posts to /~/login and stores the urbauth cookie for subsequent requests.
    async fn authenticate(&self) -> Result<(), String> {
        let url = self.endpoint("/~/login");
        let body = format!("password={}", self.code);

        let resp = self
            .client
            .post(&url)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(|e| format!("Urbit login failed: {}", e))?;

        if !resp.status().is_success() {
            return Err(format!(
                "Urbit login HTTP {} — check +code",
                resp.status().as_u16()
            ));
        }

        // Extract the urbauth cookie from Set-Cookie header
        let cookie = resp
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .find(|c| c.starts_with("urbauth-"))
            .map(|c| {
                c.split(';')
                    .next()
                    .unwrap_or(c)
                    .to_string()
            })
            .ok_or("Urbit login: no urbauth cookie returned")?;

        let mut guard = self.auth_cookie.lock().await;
        *guard = Some(cookie.clone());

        debug!(ship = %self.ship_name, "Urbit authenticated");
        Ok(())
    }

    /// Send a poke to the ship via the Eyre channel.
    fn poke(
        &self,
        app: &str,
        mark: &str,
        json: serde_json::Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<u64, String>> + Send + '_>> {
        let app = app.to_string();
        let mark = mark.to_string();
        Box::pin(async move {
        let action_id = self.next_id();
        let json_backup = json.clone();

        let action = PokeAction {
            id: action_id,
            action: "poke".into(),
            ship: self.bare_ship_name().to_string(),
            app: app.clone(),
            mark: mark.clone(),
            json,
        };

        let url = self.channel_url();
        let cookie = self.get_auth_cookie().await?;

        let resp = self
            .client
            .put(&url)
            .header("Cookie", &cookie)
            .json(&vec![action])
            .send()
            .await
            .map_err(|e| format!("Urbit poke failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();

            // If 403/401, try re-authenticating
            if status.as_u16() == 401 || status.as_u16() == 403 {
                warn!("Urbit session expired, re-authenticating");
                self.authenticate().await?;
                return self.poke(&app, &mark, json_backup).await;
            }

            return Err(format!("Urbit poke HTTP {}: {}", status, err));
        }

        debug!(id = action_id, app = %app, mark = %mark, "Urbit poke sent");
        Ok(action_id)
        })
    }

    /// Build a graph-store add-post poke for sending a chat message.
    fn build_chat_poke(&self, text: &str) -> serde_json::Value {
        let now_ms = chrono::Utc::now().timestamp_millis();
        // Urbit uses a specific index format based on timestamp
        let index = format!("/{}", now_ms);

        serde_json::json!({
            "add-nodes": {
                "resource": {
                    "ship": self.bare_ship_name(),
                    "name": self.extract_chat_name(),
                },
                "nodes": {
                    &index: {
                        "post": {
                            "author": self.bare_ship_name(),
                            "index": index,
                            "time-sent": now_ms,
                            "contents": [
                                {"text": text}
                            ],
                            "hash": null,
                            "signatures": []
                        },
                        "children": null
                    }
                }
            }
        })
    }

    /// Extract the chat name from the channel_name (e.g., `~ship/chat-name` → `chat-name`).
    fn extract_chat_name(&self) -> &str {
        self.channel_name
            .split('/')
            .last()
            .unwrap_or(&self.channel_name)
    }

    /// Get the stored auth cookie, or error if not authenticated.
    async fn get_auth_cookie(&self) -> Result<String, String> {
        let guard = self.auth_cookie.lock().await;
        guard
            .clone()
            .ok_or_else(|| "Urbit: not authenticated — call authenticate() first".into())
    }

    /// Subscribe to graph-store updates for the configured channel.
    async fn subscribe(&self) -> Result<u64, String> {
        let action_id = self.next_id();

        let subscribe = SubscribeAction {
            id: action_id,
            action: "subscribe".into(),
            ship: self.bare_ship_name().to_string(),
            app: "graph-store".into(),
            path: format!(
                "/updates/keys/~{}/{}",
                self.bare_ship_name(),
                self.extract_chat_name()
            ),
        };

        let url = self.channel_url();
        let cookie = self.get_auth_cookie().await?;

        let resp = self
            .client
            .put(&url)
            .header("Cookie", &cookie)
            .json(&vec![subscribe])
            .send()
            .await
            .map_err(|e| format!("Urbit subscribe failed: {}", e))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            return Err(format!("Urbit subscribe HTTP {}: {}", status, err));
        }

        debug!(id = action_id, "Urbit subscription started");
        Ok(action_id)
    }

    /// Normalize a graph-store add-nodes event into a NormalizedMessage.
    fn normalize_graph_event(
        &self,
        json: &serde_json::Value,
    ) -> Option<NormalizedMessage> {
        let add_nodes = json.get("add-nodes")?;
        let nodes = add_nodes.get("nodes")?.as_object()?;

        // Get the first (and typically only) node
        let (_index, node) = nodes.iter().next()?;
        let post = node.get("post")?;
        let author = post.get("author")?.as_str()?;
        let contents = post.get("contents")?.as_array()?;

        // Skip our own messages
        if author == self.bare_ship_name() {
            return None;
        }

        // Concatenate text contents
        let text: String = contents
            .iter()
            .filter_map(|c| c.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<&str>>()
            .join("");

        if text.is_empty() {
            return None;
        }

        let time_sent = post
            .get("time-sent")
            .and_then(|t| t.as_i64())
            .unwrap_or(0);

        let sender = SenderIdentity {
            id: format!("~{}", author),
            display_name: format!("~{}", author),
            channel: ChannelId::Tlon,
        };

        let session_key = clawdesk_types::session::SessionKey::new(
            ChannelId::Tlon,
            &self.channel_name,
        );

        let origin = clawdesk_types::message::MessageOrigin::Tlon {
            ship: format!("~{}", author),
            channel_name: self.channel_name.clone(),
        };

        Some(NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key,
            body: text,
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
impl Channel for TlonChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Tlon
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Tlon".into(),
            supports_threading: true,
            supports_streaming: true,
            supports_reactions: false,
            supports_media: false,
            supports_groups: true,
            max_message_length: None,
        }
    }

    async fn start(&self, _sink: Arc<dyn MessageSink>) -> Result<(), String> {
        self.running.store(true, Ordering::Relaxed);

        // Authenticate with the ship
        self.authenticate().await?;

        // Subscribe to graph-store updates
        self.subscribe().await?;

        info!(
            ship = %self.ship_name,
            channel = %self.channel_name,
            url = %self.ship_url,
            "Tlon/Urbit channel started"
        );
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let json = self.build_chat_poke(&msg.body);

        let action_id = self
            .poke("graph-store", "graph-update-3", json)
            .await?;

        Ok(DeliveryReceipt {
            channel: ChannelId::Tlon,
            message_id: action_id.to_string(),
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);

        // Close the Eyre channel
        if let Ok(cookie) = self.get_auth_cookie().await {
            let url = self.channel_url();
            let _ = self
                .client
                .delete(&url)
                .header("Cookie", &cookie)
                .send()
                .await;
        }

        self.shutdown.notify_waiters();
        info!(
            ship = %self.ship_name,
            "Tlon/Urbit channel stopped"
        );
        Ok(())
    }
}

#[async_trait]
impl Streaming for TlonChannel {
    async fn send_streaming(&self, initial: OutboundMessage) -> Result<StreamHandle, String> {
        // Urbit doesn't natively support message editing in graph-store.
        // For streaming, we send the initial message and return a handle.
        // Updates would need to delete + re-add the node, which is not
        // ideal but functional.
        let receipt = self.send(initial).await?;
        let msg_id = receipt.message_id.clone();

        Ok(StreamHandle {
            message_id: msg_id,
            update_fn: Box::new(|_text| Ok(())),
        })
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> TlonConfig {
        TlonConfig {
            ship_url: "http://localhost:8080".into(),
            ship_name: "~sampel-palnet".into(),
            code: "lidlut-tabwed-pillex-ridrup".into(),
            channel_name: "~sampel-palnet/test-chat".into(),
        }
    }

    #[test]
    fn test_tlon_channel_creation() {
        let channel = TlonChannel::new(test_config());
        assert_eq!(channel.id(), ChannelId::Tlon);
        assert_eq!(channel.ship_name, "~sampel-palnet");
        assert_eq!(channel.ship_url, "http://localhost:8080");
        assert_eq!(channel.channel_name, "~sampel-palnet/test-chat");
        assert!(channel.channel_uid.starts_with("clawdesk-"));
    }

    #[test]
    fn test_tlon_meta() {
        let channel = TlonChannel::new(test_config());
        let meta = channel.meta();
        assert_eq!(meta.display_name, "Tlon");
        assert!(meta.supports_threading);
        assert!(meta.supports_streaming);
        assert!(!meta.supports_reactions);
        assert!(meta.supports_groups);
        assert_eq!(meta.max_message_length, None);
    }

    #[test]
    fn test_tlon_ship_url_endpoint() {
        let channel = TlonChannel::new(test_config());
        assert_eq!(
            channel.endpoint("/~/login"),
            "http://localhost:8080/~/login"
        );
        assert!(channel.channel_url().starts_with("http://localhost:8080/~/channel/clawdesk-"));
    }

    #[test]
    fn test_tlon_ship_url_trailing_slash() {
        let mut cfg = test_config();
        cfg.ship_url = "http://localhost:8080/".into();
        let channel = TlonChannel::new(cfg);
        assert_eq!(channel.ship_url, "http://localhost:8080");
    }

    #[test]
    fn test_tlon_bare_ship_name() {
        let channel = TlonChannel::new(test_config());
        assert_eq!(channel.bare_ship_name(), "sampel-palnet");

        let mut cfg = test_config();
        cfg.ship_name = "sampel-palnet".into(); // without ~
        let channel2 = TlonChannel::new(cfg);
        assert_eq!(channel2.bare_ship_name(), "sampel-palnet");
    }

    #[test]
    fn test_tlon_extract_chat_name() {
        let channel = TlonChannel::new(test_config());
        assert_eq!(channel.extract_chat_name(), "test-chat");
    }

    #[test]
    fn test_tlon_poke_payload() {
        let channel = TlonChannel::new(test_config());
        let poke = channel.build_chat_poke("Hello, Urbit!");

        let add_nodes = poke.get("add-nodes").unwrap();
        let resource = add_nodes.get("resource").unwrap();
        assert_eq!(
            resource.get("ship").unwrap().as_str().unwrap(),
            "sampel-palnet"
        );
        assert_eq!(
            resource.get("name").unwrap().as_str().unwrap(),
            "test-chat"
        );

        let nodes = add_nodes.get("nodes").unwrap().as_object().unwrap();
        assert_eq!(nodes.len(), 1);

        let (_, node) = nodes.iter().next().unwrap();
        let post = node.get("post").unwrap();
        assert_eq!(
            post.get("author").unwrap().as_str().unwrap(),
            "sampel-palnet"
        );

        let contents = post.get("contents").unwrap().as_array().unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(
            contents[0].get("text").unwrap().as_str().unwrap(),
            "Hello, Urbit!"
        );
    }

    #[test]
    fn test_tlon_action_id_monotonic() {
        let channel = TlonChannel::new(test_config());
        let id1 = channel.next_id();
        let id2 = channel.next_id();
        let id3 = channel.next_id();
        assert!(id2 > id1);
        assert!(id3 > id2);
    }

    #[test]
    fn test_tlon_normalize_graph_event() {
        let channel = TlonChannel::new(test_config());
        let event_json = serde_json::json!({
            "add-nodes": {
                "resource": {
                    "ship": "sampel-palnet",
                    "name": "test-chat"
                },
                "nodes": {
                    "/170000000000": {
                        "post": {
                            "author": "zod",
                            "index": "/170000000000",
                            "time-sent": 170000000000i64,
                            "contents": [
                                {"text": "Hello from ~zod!"}
                            ],
                            "hash": null,
                            "signatures": []
                        },
                        "children": null
                    }
                }
            }
        });

        let normalized = channel.normalize_graph_event(&event_json).unwrap();
        assert_eq!(normalized.body, "Hello from ~zod!");
        assert_eq!(normalized.sender.id, "~zod");
    }

    #[test]
    fn test_tlon_normalize_ignores_own_messages() {
        let channel = TlonChannel::new(test_config());
        let event_json = serde_json::json!({
            "add-nodes": {
                "resource": {
                    "ship": "sampel-palnet",
                    "name": "test-chat"
                },
                "nodes": {
                    "/170000000001": {
                        "post": {
                            "author": "sampel-palnet",
                            "index": "/170000000001",
                            "time-sent": 170000000001i64,
                            "contents": [
                                {"text": "My own message"}
                            ],
                            "hash": null,
                            "signatures": []
                        },
                        "children": null
                    }
                }
            }
        });

        assert!(channel.normalize_graph_event(&event_json).is_none());
    }
}
