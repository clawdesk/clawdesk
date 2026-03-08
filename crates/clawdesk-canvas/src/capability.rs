//! Capability tokens — time-limited auth for canvas URLs.
//!
//! Tokens are 18-byte random base64url strings with a configurable TTL.
//! Scoped to canvas paths: `/__clawdesk__/cap/{token}/...`
//!
//! ```text
//! mint() → CanvasCapability { token, expires_at }
//! validate(token) → bool
//! refresh(old_token) → CanvasCapability { new_token, ... }
//! ```

use base64::engine::{general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// Default capability TTL: 10 minutes.
const DEFAULT_TTL: Duration = Duration::from_secs(600);

/// Canvas capability token.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasCapability {
    pub token: String,
    pub expires_at_ms: i64,
    pub canvas_host_url: String,
}

/// Stored capability entry with metadata.
#[derive(Debug, Clone)]
struct CapEntry {
    expires_at: DateTime<Utc>,
    agent_id: String,
}

/// Thread-safe capability store.
#[derive(Debug, Clone)]
pub struct CapabilityStore {
    inner: Arc<DashMap<String, CapEntry>>,
    ttl: Duration,
    host_url: String,
}

impl CapabilityStore {
    /// Create a new store with default TTL.
    pub fn new(host_url: String) -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            ttl: DEFAULT_TTL,
            host_url,
        }
    }

    /// Create with custom TTL.
    pub fn with_ttl(host_url: String, ttl: Duration) -> Self {
        Self {
            inner: Arc::new(DashMap::new()),
            ttl,
            host_url,
        }
    }

    /// Mint a new capability token for an agent.
    pub fn mint(&self, agent_id: &str) -> CanvasCapability {
        let token = generate_token();
        let expires_at = Utc::now() + chrono::Duration::from_std(self.ttl).unwrap_or_default();

        self.inner.insert(
            token.clone(),
            CapEntry {
                expires_at,
                agent_id: agent_id.to_owned(),
            },
        );

        CanvasCapability {
            token: token.clone(),
            expires_at_ms: expires_at.timestamp_millis(),
            canvas_host_url: format!("{}/__clawdesk__/cap/{}", self.host_url, token),
        }
    }

    /// Validate a token. Returns the agent_id if valid.
    pub fn validate(&self, token: &str) -> Option<String> {
        let entry = self.inner.get(token)?;
        if Utc::now() > entry.expires_at {
            drop(entry);
            self.inner.remove(token);
            return None;
        }
        Some(entry.agent_id.clone())
    }

    /// Refresh: revoke old token, mint new one.
    pub fn refresh(&self, old_token: &str) -> Option<CanvasCapability> {
        let agent_id = {
            let entry = self.inner.get(old_token)?;
            entry.agent_id.clone()
        };
        self.inner.remove(old_token);
        Some(self.mint(&agent_id))
    }

    /// Revoke a token.
    pub fn revoke(&self, token: &str) -> bool {
        self.inner.remove(token).is_some()
    }

    /// Evict all expired tokens.
    pub fn evict_expired(&self) -> usize {
        let now = Utc::now();
        let before = self.inner.len();
        self.inner.retain(|_, entry| entry.expires_at > now);
        before - self.inner.len()
    }

    /// Number of active tokens.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the store is empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Get the host URL.
    pub fn host_url(&self) -> &str {
        &self.host_url
    }

    /// Extract capability token from a scoped URL path.
    ///
    /// Path format: `/__clawdesk__/cap/{token}/...`
    /// Returns `(token, remainder)` if matched.
    pub fn extract_from_path(path: &str) -> Option<(&str, &str)> {
        let prefix = "/__clawdesk__/cap/";
        if !path.starts_with(prefix) {
            return None;
        }
        let rest = &path[prefix.len()..];
        let (token, remainder) = match rest.find('/') {
            Some(pos) => (&rest[..pos], &rest[pos..]),
            None => (rest, "/"),
        };
        if token.is_empty() {
            return None;
        }
        Some((token, remainder))
    }
}

/// Generate a cryptographically random 18-byte base64url token.
fn generate_token() -> String {
    let mut bytes = [0u8; 18];
    rand::thread_rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

// ═══════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_and_validate() {
        let store = CapabilityStore::new("http://localhost:9000".into());
        let cap = store.mint("agent-1");
        assert!(!cap.token.is_empty());
        assert!(cap.canvas_host_url.contains(&cap.token));

        let agent = store.validate(&cap.token);
        assert_eq!(agent, Some("agent-1".into()));
    }

    #[test]
    fn expired_token_rejected() {
        let store = CapabilityStore::with_ttl(
            "http://localhost:9000".into(),
            Duration::from_millis(0),
        );
        let cap = store.mint("agent-1");
        // Token is already expired (0ms TTL)
        std::thread::sleep(Duration::from_millis(1));
        assert!(store.validate(&cap.token).is_none());
    }

    #[test]
    fn refresh_revokes_old() {
        let store = CapabilityStore::new("http://localhost:9000".into());
        let cap1 = store.mint("agent-1");
        let cap2 = store.refresh(&cap1.token).unwrap();

        assert!(store.validate(&cap1.token).is_none()); // old revoked
        assert_eq!(store.validate(&cap2.token), Some("agent-1".into())); // new valid
    }

    #[test]
    fn extract_from_path_works() {
        let (token, rest) =
            CapabilityStore::extract_from_path("/__clawdesk__/cap/abc123/index.html")
                .unwrap();
        assert_eq!(token, "abc123");
        assert_eq!(rest, "/index.html");

        let (token, rest) =
            CapabilityStore::extract_from_path("/__clawdesk__/cap/xyz").unwrap();
        assert_eq!(token, "xyz");
        assert_eq!(rest, "/");

        assert!(CapabilityStore::extract_from_path("/other/path").is_none());
        assert!(CapabilityStore::extract_from_path("/__clawdesk__/cap/").is_none());
    }

    #[test]
    fn evict_expired_cleans_up() {
        let store = CapabilityStore::with_ttl(
            "http://localhost:9000".into(),
            Duration::from_millis(0),
        );
        store.mint("agent-1");
        store.mint("agent-2");
        std::thread::sleep(Duration::from_millis(1));
        let evicted = store.evict_expired();
        assert_eq!(evicted, 2);
        assert!(store.is_empty());
    }
}
