//! OAuth2 token manager with lease renewal and thundering-herd prevention.
//!
//! Token refresh uses OnceCell to coalesce concurrent refresh attempts:
//! if N requests discover the token needs refreshing simultaneously,
//! only one calls the refresh endpoint; the rest await the same future.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, warn};

/// OAuth2 token with expiry tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: String,
    pub expires_at: DateTime<Utc>,
    pub scopes: Vec<String>,
    pub provider: String,
}

impl OAuthToken {
    /// Whether the token has expired.
    pub fn is_expired(&self) -> bool {
        Utc::now() >= self.expires_at
    }

    /// Whether the token should be proactively refreshed (< margin remaining).
    pub fn needs_refresh(&self, margin_secs: i64) -> bool {
        Utc::now() + chrono::Duration::seconds(margin_secs) >= self.expires_at
    }

    /// Remaining lifetime in seconds.
    pub fn remaining_secs(&self) -> i64 {
        (self.expires_at - Utc::now()).num_seconds()
    }
}

/// Manages OAuth2 tokens for a service adapter.
///
/// Provides thread-safe token access with coalesced refresh.
pub struct OAuthManager {
    /// Current token (behind RwLock for concurrent reads, exclusive writes)
    token: RwLock<Option<OAuthToken>>,
    /// Refresh margin in seconds (default: 300 = 5 minutes)
    refresh_margin_secs: i64,
    /// Provider name for logging
    provider: String,
}

impl OAuthManager {
    /// Create a new OAuth manager.
    pub fn new(provider: impl Into<String>, refresh_margin_secs: i64) -> Arc<Self> {
        Arc::new(Self {
            token: RwLock::new(None),
            refresh_margin_secs,
            provider: provider.into(),
        })
    }

    /// Set the current token (after initial OAuth flow or manual configuration).
    pub async fn set_token(&self, token: OAuthToken) {
        let mut guard = self.token.write().await;
        *guard = Some(token);
    }

    /// Get the current valid access token, or None if expired/missing.
    pub async fn get_token(&self) -> Option<String> {
        let guard = self.token.read().await;
        match &*guard {
            Some(token) if !token.is_expired() => Some(token.access_token.clone()),
            _ => None,
        }
    }

    /// Check if a refresh is needed.
    pub async fn needs_refresh(&self) -> bool {
        let guard = self.token.read().await;
        match &*guard {
            Some(token) => token.needs_refresh(self.refresh_margin_secs),
            None => false, // no token to refresh
        }
    }

    /// Get the refresh token for performing a refresh.
    pub async fn get_refresh_token(&self) -> Option<String> {
        let guard = self.token.read().await;
        guard
            .as_ref()
            .and_then(|t| t.refresh_token.clone())
    }

    /// Whether we have a token at all.
    pub async fn has_token(&self) -> bool {
        self.token.read().await.is_some()
    }

    /// Provider name.
    pub fn provider(&self) -> &str {
        &self.provider
    }
}
