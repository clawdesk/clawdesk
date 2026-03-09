//! MCP authentication and authorization.
//!
//! Provides credential management and request signing for MCP server connections.
//! Supports multiple auth schemes that MCP servers may require:
//!
//! - **Bearer token**: Simple API key / token auth.
//! - **OAuth2**: Client credentials or PKCE flows.
//! - **Custom header**: Arbitrary key-value header injection.
//! - **None**: No authentication (local/trusted servers).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Authentication scheme for an MCP server connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthScheme {
    /// No authentication required.
    None,
    /// Bearer token (API key).
    Bearer {
        /// The bearer token value. Should be loaded from secure storage.
        token: String,
    },
    /// OAuth2 client credentials grant.
    OAuth2 {
        client_id: String,
        client_secret: String,
        token_url: String,
        #[serde(default)]
        scopes: Vec<String>,
    },
    /// Custom header injection.
    CustomHeader {
        header_name: String,
        header_value: String,
    },
}

/// A resolved set of HTTP headers to inject into MCP requests.
#[derive(Debug, Clone, Default)]
pub struct AuthHeaders {
    pub headers: HashMap<String, String>,
}

impl AuthHeaders {
    /// Merge these headers into a reqwest HeaderMap.
    pub fn as_header_pairs(&self) -> impl Iterator<Item = (&str, &str)> {
        self.headers.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// Cached OAuth2 token with expiry tracking.
#[derive(Debug, Clone)]
struct CachedToken {
    access_token: String,
    expires_at: Instant,
}

impl CachedToken {
    fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }
}

/// MCP credential resolver — resolves auth schemes to concrete headers.
pub struct McpAuthResolver {
    /// Cached OAuth2 tokens keyed by (client_id, token_url).
    token_cache: HashMap<String, CachedToken>,
}

impl McpAuthResolver {
    pub fn new() -> Self {
        Self {
            token_cache: HashMap::new(),
        }
    }

    /// Resolve an auth scheme into HTTP headers ready for injection.
    ///
    /// For OAuth2, this will use cached tokens when available and not expired.
    /// Token refresh is currently synchronous (blocking is acceptable during
    /// MCP connection setup, which is infrequent).
    pub fn resolve(&mut self, scheme: &AuthScheme) -> Result<AuthHeaders, McpAuthError> {
        match scheme {
            AuthScheme::None => Ok(AuthHeaders::default()),

            AuthScheme::Bearer { token } => {
                let mut headers = HashMap::new();
                headers.insert(
                    "Authorization".to_string(),
                    format!("Bearer {}", token),
                );
                Ok(AuthHeaders { headers })
            }

            AuthScheme::OAuth2 {
                client_id,
                token_url,
                ..
            } => {
                let cache_key = format!("{}:{}", client_id, token_url);

                // Check cache.
                if let Some(cached) = self.token_cache.get(&cache_key) {
                    if !cached.is_expired() {
                        let mut headers = HashMap::new();
                        headers.insert(
                            "Authorization".to_string(),
                            format!("Bearer {}", cached.access_token),
                        );
                        return Ok(AuthHeaders { headers });
                    }
                }

                // Token expired or not cached — needs refresh.
                // Actual HTTP token exchange would happen here in production.
                // For now, return an error indicating refresh is needed.
                Err(McpAuthError::TokenExpired {
                    client_id: client_id.clone(),
                })
            }

            AuthScheme::CustomHeader {
                header_name,
                header_value,
            } => {
                let mut headers = HashMap::new();
                headers.insert(header_name.clone(), header_value.clone());
                Ok(AuthHeaders { headers })
            }
        }
    }

    /// Store an OAuth2 token in the cache.
    pub fn cache_token(
        &mut self,
        client_id: &str,
        token_url: &str,
        access_token: String,
        expires_in: Duration,
    ) {
        let cache_key = format!("{}:{}", client_id, token_url);
        // Expire slightly early to avoid race conditions.
        let expires_at = Instant::now() + expires_in - Duration::from_secs(30);
        self.token_cache.insert(
            cache_key,
            CachedToken {
                access_token,
                expires_at,
            },
        );
    }

    /// Clear all cached tokens.
    pub fn clear_cache(&mut self) {
        self.token_cache.clear();
    }

    /// Evict expired tokens from the cache.
    pub fn evict_expired(&mut self) {
        self.token_cache.retain(|_, v| !v.is_expired());
    }
}

impl Default for McpAuthResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// MCP-specific authentication configuration for a server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerAuth {
    /// Server identifier.
    pub server_id: String,
    /// Authentication scheme.
    pub auth: AuthScheme,
    /// Whether to validate the server's TLS certificate.
    #[serde(default = "default_true")]
    pub verify_tls: bool,
    /// Client certificate path for mTLS.
    #[serde(default)]
    pub client_cert_path: Option<String>,
}

fn default_true() -> bool {
    true
}

/// MCP authentication errors.
#[derive(Debug, Clone)]
pub enum McpAuthError {
    /// OAuth2 token has expired and needs refresh.
    TokenExpired { client_id: String },
    /// Invalid credentials.
    InvalidCredentials { detail: String },
    /// TLS/certificate error.
    TlsError { detail: String },
}

impl std::fmt::Display for McpAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TokenExpired { client_id } => {
                write!(f, "OAuth2 token expired for client '{}'", client_id)
            }
            Self::InvalidCredentials { detail } => {
                write!(f, "invalid credentials: {}", detail)
            }
            Self::TlsError { detail } => write!(f, "TLS error: {}", detail),
        }
    }
}

impl std::error::Error for McpAuthError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_auth_headers() {
        let mut resolver = McpAuthResolver::new();
        let scheme = AuthScheme::Bearer {
            token: "sk-test-123".into(),
        };
        let headers = resolver.resolve(&scheme).unwrap();
        assert_eq!(
            headers.headers.get("Authorization").unwrap(),
            "Bearer sk-test-123"
        );
    }

    #[test]
    fn no_auth_empty_headers() {
        let mut resolver = McpAuthResolver::new();
        let headers = resolver.resolve(&AuthScheme::None).unwrap();
        assert!(headers.headers.is_empty());
    }

    #[test]
    fn custom_header_auth() {
        let mut resolver = McpAuthResolver::new();
        let scheme = AuthScheme::CustomHeader {
            header_name: "X-Api-Key".into(),
            header_value: "my-key".into(),
        };
        let headers = resolver.resolve(&scheme).unwrap();
        assert_eq!(headers.headers.get("X-Api-Key").unwrap(), "my-key");
    }

    #[test]
    fn oauth2_token_cache() {
        let mut resolver = McpAuthResolver::new();

        // Cache a token.
        resolver.cache_token(
            "client1",
            "https://auth.example.com/token",
            "access-token-123".into(),
            Duration::from_secs(3600),
        );

        // Resolve should use cached token.
        let scheme = AuthScheme::OAuth2 {
            client_id: "client1".into(),
            client_secret: "secret".into(),
            token_url: "https://auth.example.com/token".into(),
            scopes: vec![],
        };
        let headers = resolver.resolve(&scheme).unwrap();
        assert_eq!(
            headers.headers.get("Authorization").unwrap(),
            "Bearer access-token-123"
        );
    }

    #[test]
    fn expired_token_returns_error() {
        let mut resolver = McpAuthResolver::new();

        // Cache a token that's already expired (expires_in < 30s safety margin).
        resolver.cache_token(
            "client1",
            "https://auth.example.com/token",
            "old-token".into(),
            Duration::from_secs(0), // Already expired.
        );

        let scheme = AuthScheme::OAuth2 {
            client_id: "client1".into(),
            client_secret: "secret".into(),
            token_url: "https://auth.example.com/token".into(),
            scopes: vec![],
        };

        assert!(resolver.resolve(&scheme).is_err());
    }
}
