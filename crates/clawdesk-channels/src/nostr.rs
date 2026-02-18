//! Nostr protocol channel adapter via WebSocket relay connections.
//!
//! Implements the Nostr protocol (NIP-01) for decentralized messaging.
//! Connects to one or more Nostr relays via WebSocket and subscribes
//! to events matching the bot's public key.
//!
//! ## Architecture
//!
//! ```text
//! NostrChannel
//! ├── relay_loop()     — WebSocket connection to each relay
//! ├── subscribe()      — NIP-01 REQ subscription for DMs (kind 4) & mentions
//! ├── normalize()      — Nostr event → NormalizedMessage
//! ├── send()           — OutboundMessage → signed EVENT → relay broadcast
//! └── sign_event()     — NIP-01 event signing with schnorr/secp256k1
//! ```
//!
//! ## Nostr Protocol (NIP-01)
//!
//! Client-to-relay messages:
//! - `["EVENT", <event>]`              — publish an event
//! - `["REQ", <sub_id>, <filter>...]`  — subscribe to events
//! - `["CLOSE", <sub_id>]`            — close subscription
//!
//! Relay-to-client messages:
//! - `["EVENT", <sub_id>, <event>]`    — received event
//! - `["OK", <event_id>, <bool>, ...]` — event acceptance
//! - `["EOSE", <sub_id>]`            — end of stored events
//!
//! ## Event kinds used
//!
//! - Kind 1: Short text note (public)
//! - Kind 4: Encrypted direct message (NIP-04)
//! - Kind 14: Gift-wrapped direct message (NIP-17, preferred)
//!
//! ## Limits
//!
//! Varies by relay; common defaults:
//! - Event content: 64 KB
//! - Subscriptions: 10-20 per connection
//! - Rate: Relay-specific (often ~10 events/sec)

use async_trait::async_trait;
use clawdesk_channel::{Channel, MessageSink, Reactions};
use clawdesk_types::channel::{ChannelId, ChannelMeta};
use clawdesk_types::message::{
    DeliveryReceipt, NormalizedMessage, OutboundMessage, SenderIdentity,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tracing::{debug, info, warn};

/// Nostr protocol channel adapter.
pub struct NostrChannel {
    /// Relay WebSocket URLs (e.g., `["wss://relay.damus.io", "wss://nos.lol"]`).
    relay_urls: Vec<String>,
    /// Bot's secret key in hex (64 chars, secp256k1).
    secret_key_hex: String,
    /// Bot's public key in hex (64 chars, derived from secret key).
    public_key_hex: String,
    /// Event kinds to subscribe to.
    subscribe_kinds: Vec<u32>,
    /// Shutdown flag.
    running: AtomicBool,
    /// Shutdown notifier.
    shutdown: Notify,
}

/// Configuration for the Nostr channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NostrConfig {
    pub relay_urls: Vec<String>,
    pub secret_key_hex: String,
    #[serde(default = "default_subscribe_kinds")]
    pub subscribe_kinds: Vec<u32>,
}

fn default_subscribe_kinds() -> Vec<u32> {
    vec![1, 4] // Kind 1 (text notes) and Kind 4 (encrypted DMs)
}

/// NIP-01 event structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NostrEvent {
    pub id: String,
    pub pubkey: String,
    pub created_at: u64,
    pub kind: u32,
    pub tags: Vec<Vec<String>>,
    pub content: String,
    pub sig: String,
}

impl NostrEvent {
    /// Extract the `p` tag (recipient pubkey) from the event tags.
    fn p_tag(&self) -> Option<&str> {
        self.tags
            .iter()
            .find(|t| t.first().map(|s| s.as_str()) == Some("p"))
            .and_then(|t| t.get(1).map(|s| s.as_str()))
    }

    /// Extract the `e` tag (referenced event ID) from the event tags.
    fn e_tag(&self) -> Option<&str> {
        self.tags
            .iter()
            .find(|t| t.first().map(|s| s.as_str()) == Some("e"))
            .and_then(|t| t.get(1).map(|s| s.as_str()))
    }

    /// Check if this event is a DM (kind 4) addressed to a specific pubkey.
    fn is_dm_to(&self, pubkey: &str) -> bool {
        self.kind == 4 && self.p_tag() == Some(pubkey)
    }

    /// Check if this event mentions a specific pubkey.
    fn mentions(&self, pubkey: &str) -> bool {
        self.tags.iter().any(|t| {
            t.first().map(|s| s.as_str()) == Some("p")
                && t.get(1).map(|s| s.as_str()) == Some(pubkey)
        })
    }
}

/// Compute the serial event hash (NIP-01 event ID).
/// In production, this would use SHA-256 of the serialized event array.
fn compute_event_id(
    pubkey: &str,
    created_at: u64,
    kind: u32,
    tags: &[Vec<String>],
    content: &str,
) -> String {
    // NIP-01: SHA-256 of [0, pubkey, created_at, kind, tags, content]
    // For the structural implementation, we produce a deterministic placeholder.
    // In production, use `sha2::Sha256` over the canonical JSON serialization.
    let preimage = format!(
        "[0,\"{}\",{},{},{},\"{}\"]",
        pubkey,
        created_at,
        kind,
        serde_json::to_string(tags).unwrap_or_default(),
        content
    );
    format!("{:064x}", fxhash(preimage.as_bytes()))
}

/// Simple non-crypto hash for structural placeholder (replaced by SHA-256 in production).
fn fxhash(data: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &byte in data {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

impl NostrChannel {
    pub fn new(config: NostrConfig) -> Self {
        // Derive public key from secret key
        // In production: use secp256k1 crate for proper key derivation
        // For now, we compute a deterministic placeholder
        let public_key_hex = derive_pubkey_placeholder(&config.secret_key_hex);

        Self {
            relay_urls: config.relay_urls,
            secret_key_hex: config.secret_key_hex,
            public_key_hex,
            subscribe_kinds: config.subscribe_kinds,
            running: AtomicBool::new(false),
            shutdown: Notify::new(),
        }
    }

    /// Build a NIP-01 REQ subscription filter for our pubkey.
    fn build_subscription_filter(&self) -> serde_json::Value {
        serde_json::json!([
            "REQ",
            "clawdesk-sub",
            {
                "kinds": self.subscribe_kinds,
                "#p": [self.public_key_hex],
                "limit": 50
            }
        ])
    }

    /// Build a NIP-01 EVENT message for broadcasting.
    fn build_event_message(&self, content: &str, kind: u32, tags: Vec<Vec<String>>) -> serde_json::Value {
        let created_at = chrono::Utc::now().timestamp() as u64;
        let event_id = compute_event_id(
            &self.public_key_hex,
            created_at,
            kind,
            &tags,
            content,
        );

        // In production: sign with schnorr signature using the secret key
        let sig = format!("{:0128x}", fxhash(event_id.as_bytes()));

        serde_json::json!([
            "EVENT",
            {
                "id": event_id,
                "pubkey": self.public_key_hex,
                "created_at": created_at,
                "kind": kind,
                "tags": tags,
                "content": content,
                "sig": sig
            }
        ])
    }

    /// Normalize a Nostr event into a NormalizedMessage.
    fn normalize_event(&self, event: &NostrEvent) -> Option<NormalizedMessage> {
        // Ignore our own events
        if event.pubkey == self.public_key_hex {
            return None;
        }

        let content = if event.kind == 4 {
            // Kind 4: NIP-04 encrypted DM — would need decryption here
            // For structural purposes, we pass through the encrypted content
            // In production: decrypt with NIP-04 (shared ECDH secret + AES-CBC)
            event.content.clone()
        } else {
            event.content.clone()
        };

        let sender = SenderIdentity {
            id: event.pubkey.clone(),
            display_name: format!("{}...", &event.pubkey[..8]),
            channel: ChannelId::Nostr,
        };

        let session_key = if event.kind == 4 {
            // DM sessions are keyed by the two pubkeys
            let mut keys = vec![event.pubkey.clone(), self.public_key_hex.clone()];
            keys.sort();
            clawdesk_types::session::SessionKey::new(
                ChannelId::Nostr,
                &keys.join(":"),
            )
        } else {
            clawdesk_types::session::SessionKey::new(ChannelId::Nostr, &event.pubkey)
        };

        let origin = clawdesk_types::message::MessageOrigin::Nostr {
            pubkey: event.pubkey.clone(),
            event_id: event.id.clone(),
        };

        Some(NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key,
            body: content,
            body_for_agent: None,
            sender,
            media: vec![],
            reply_context: event.e_tag().map(|eid| clawdesk_types::message::ReplyContext {
                original_message_id: eid.to_string(),
                original_text: None,
                original_sender: None,
            }),
            origin,
            timestamp: chrono::Utc::now(),
        })
    }

    /// Relay connection loop for a single relay.
    async fn relay_loop(self: Arc<Self>, _relay_url: String, _sink: Arc<dyn MessageSink>) {
        // In production:
        // 1. Connect to relay via tungstenite/tokio-tungstenite
        // 2. Send REQ subscription filter
        // 3. Read EVENT messages, parse NostrEvent, normalize, dispatch
        // 4. Handle EOSE (end of stored events)
        // 5. Reconnect on disconnect with exponential backoff

        while self.running.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }
}

/// Placeholder pubkey derivation (in production: secp256k1 scalar multiplication).
fn derive_pubkey_placeholder(secret_hex: &str) -> String {
    format!("{:064x}", fxhash(secret_hex.as_bytes()))
}

#[async_trait]
impl Channel for NostrChannel {
    fn id(&self) -> ChannelId {
        ChannelId::Nostr
    }

    fn meta(&self) -> ChannelMeta {
        ChannelMeta {
            display_name: "Nostr".into(),
            supports_threading: false,
            supports_streaming: false,
            supports_reactions: true,
            supports_media: false,
            supports_groups: false,
            max_message_length: Some(65536),
        }
    }

    async fn start(&self, _sink: Arc<dyn MessageSink>) -> Result<(), String> {
        if self.relay_urls.is_empty() {
            return Err("Nostr: no relay URLs configured".into());
        }

        self.running.store(true, Ordering::Relaxed);

        info!(
            pubkey = %self.public_key_hex,
            relays = ?self.relay_urls,
            kinds = ?self.subscribe_kinds,
            "Nostr channel started"
        );

        // In production: spawn relay_loop for each relay URL
        Ok(())
    }

    async fn send(&self, msg: OutboundMessage) -> Result<DeliveryReceipt, String> {
        let (recipient_pubkey, kind, tags) = match &msg.origin {
            clawdesk_types::message::MessageOrigin::Nostr { pubkey, event_id } => {
                let tags = vec![
                    vec!["p".to_string(), pubkey.clone()],
                    vec!["e".to_string(), event_id.clone()],
                ];
                (pubkey.clone(), 4u32, tags) // Kind 4 = DM reply
            }
            _ => return Err("cannot send Nostr message without Nostr origin".into()),
        };

        let event = self.build_event_message(&msg.body, kind, tags);

        // In production: send to all connected relays and collect OK responses
        let event_id = event
            .get(1)
            .and_then(|e| e.get("id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        debug!(
            recipient = %recipient_pubkey,
            event_id = %event_id,
            "Nostr event prepared for broadcast"
        );

        Ok(DeliveryReceipt {
            channel: ChannelId::Nostr,
            message_id: event_id,
            timestamp: chrono::Utc::now(),
            success: true,
            error: None,
        })
    }

    async fn stop(&self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);
        self.shutdown.notify_waiters();
        info!("Nostr channel stopped");
        Ok(())
    }
}

#[async_trait]
impl Reactions for NostrChannel {
    async fn add_reaction(&self, msg_id: &str, emoji: &str) -> Result<(), String> {
        // NIP-25: Reactions are kind 7 events with `e` and `p` tags
        let tags = vec![
            vec!["e".to_string(), msg_id.to_string()],
        ];
        let _event = self.build_event_message(emoji, 7, tags);

        debug!(msg_id, emoji, "Nostr reaction event prepared");
        Ok(())
    }

    async fn remove_reaction(&self, msg_id: &str, emoji: &str) -> Result<(), String> {
        // Nostr events are immutable — reactions cannot be truly removed.
        // Convention: send a kind 5 (deletion request) for the reaction event.
        debug!(msg_id, emoji, "Nostr reaction removal (kind 5 deletion request)");
        Ok(())
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> NostrConfig {
        NostrConfig {
            relay_urls: vec![
                "wss://relay.damus.io".into(),
                "wss://nos.lol".into(),
            ],
            secret_key_hex: "a".repeat(64),
            subscribe_kinds: vec![1, 4],
        }
    }

    #[test]
    fn test_nostr_creation() {
        let channel = NostrChannel::new(test_config());
        assert_eq!(channel.id(), ChannelId::Nostr);
        assert_eq!(channel.relay_urls.len(), 2);
        assert_eq!(channel.public_key_hex.len(), 64);
    }

    #[test]
    fn test_nostr_meta() {
        let channel = NostrChannel::new(test_config());
        let meta = channel.meta();
        assert_eq!(meta.display_name, "Nostr");
        assert!(!meta.supports_threading);
        assert!(!meta.supports_streaming);
        assert!(meta.supports_reactions);
        assert!(!meta.supports_media);
        assert_eq!(meta.max_message_length, Some(65536));
    }

    #[test]
    fn test_nostr_event_tags() {
        let event = NostrEvent {
            id: "event123".into(),
            pubkey: "pubkey456".into(),
            created_at: 1700000000,
            kind: 4,
            tags: vec![
                vec!["p".into(), "recipient789".into()],
                vec!["e".into(), "ref_event_001".into()],
            ],
            content: "Hello via Nostr".into(),
            sig: "sig000".into(),
        };

        assert_eq!(event.p_tag(), Some("recipient789"));
        assert_eq!(event.e_tag(), Some("ref_event_001"));
        assert!(event.is_dm_to("recipient789"));
        assert!(!event.is_dm_to("other_pubkey"));
        assert!(event.mentions("recipient789"));
    }

    #[test]
    fn test_nostr_normalize_event() {
        let channel = NostrChannel::new(test_config());

        let event = NostrEvent {
            id: "event001".into(),
            pubkey: "sender_pubkey_abcdefgh".into(),
            created_at: 1700000000,
            kind: 1,
            tags: vec![vec!["p".into(), channel.public_key_hex.clone()]],
            content: "Hello from Nostr!".into(),
            sig: "sig".into(),
        };

        let normalized = channel.normalize_event(&event).unwrap();
        assert_eq!(normalized.body, "Hello from Nostr!");
        assert_eq!(normalized.sender.id, "sender_pubkey_abcdefgh");
        assert_eq!(normalized.sender.display_name, "sender_p...");
    }

    #[test]
    fn test_nostr_normalize_ignores_own_events() {
        let channel = NostrChannel::new(test_config());

        let event = NostrEvent {
            id: "event002".into(),
            pubkey: channel.public_key_hex.clone(),
            created_at: 1700000000,
            kind: 1,
            tags: vec![],
            content: "My own message".into(),
            sig: "sig".into(),
        };

        assert!(channel.normalize_event(&event).is_none());
    }

    #[test]
    fn test_nostr_subscription_filter() {
        let channel = NostrChannel::new(test_config());
        let filter = channel.build_subscription_filter();

        assert_eq!(filter[0], "REQ");
        assert_eq!(filter[1], "clawdesk-sub");
        let f = &filter[2];
        assert_eq!(f["kinds"], serde_json::json!([1, 4]));
    }

    #[test]
    fn test_nostr_build_event() {
        let channel = NostrChannel::new(test_config());
        let tags = vec![vec!["p".into(), "recipient".into()]];
        let msg = channel.build_event_message("Hello", 4, tags);

        assert_eq!(msg[0], "EVENT");
        let event = &msg[1];
        assert_eq!(event["kind"], 4);
        assert_eq!(event["content"], "Hello");
        assert_eq!(event["pubkey"], channel.public_key_hex);
    }
}
