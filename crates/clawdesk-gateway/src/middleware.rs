//! Gateway middleware — request tracing, CORS, rate limiting, and auth.

use axum::{
    extract::Request,
    http::{header, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, warn};

// ── Request tracing ──────────────────────────────────────────

/// Middleware that logs every request with method, path, status, and latency.
pub async fn request_tracing(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    let start = Instant::now();

    let response = next.run(req).await;

    let elapsed = start.elapsed();
    let status = response.status();

    if status.is_server_error() {
        warn!(%method, %path, %status, elapsed_ms = elapsed.as_millis(), "request failed");
    } else {
        debug!(%method, %path, %status, elapsed_ms = elapsed.as_millis(), "request handled");
    }

    response
}

// ── CORS ─────────────────────────────────────────────────────

/// CORS configuration built from `GatewayConfig.cors_origins`.
#[derive(Debug, Clone)]
pub struct CorsConfig {
    pub allowed_origins: Vec<String>,
    pub max_age_secs: u64,
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allowed_origins: vec!["http://localhost:*".to_string()],
            max_age_secs: 86400,
        }
    }
}

/// CORS middleware. Handles preflight OPTIONS and sets appropriate headers.
pub async fn cors(
    cors_config: CorsConfig,
    req: Request,
    next: Next,
) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(String::from);

    // Handle preflight
    if req.method() == Method::OPTIONS {
        let mut response = StatusCode::NO_CONTENT.into_response();
        if let Some(ref origin) = origin {
            if origin_allowed(origin, &cors_config.allowed_origins) {
                let headers = response.headers_mut();
                headers.insert(
                    header::ACCESS_CONTROL_ALLOW_ORIGIN,
                    HeaderValue::from_str(origin).unwrap_or(HeaderValue::from_static("*")),
                );
                headers.insert(
                    header::ACCESS_CONTROL_ALLOW_METHODS,
                    HeaderValue::from_static("GET, POST, PUT, DELETE, OPTIONS"),
                );
                headers.insert(
                    header::ACCESS_CONTROL_ALLOW_HEADERS,
                    HeaderValue::from_static("Content-Type, Authorization, X-Request-Id"),
                );
                headers.insert(
                    header::ACCESS_CONTROL_MAX_AGE,
                    HeaderValue::from_str(&cors_config.max_age_secs.to_string())
                        .unwrap_or(HeaderValue::from_static("86400")),
                );
            }
        }
        return response;
    }

    let mut response = next.run(req).await;

    // Set CORS headers on actual response
    if let Some(ref origin) = origin {
        if origin_allowed(origin, &cors_config.allowed_origins) {
            response.headers_mut().insert(
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_str(origin).unwrap_or(HeaderValue::from_static("*")),
            );
        }
    }

    response
}

/// Check if an origin matches any pattern (supports `*` wildcard).
fn origin_allowed(origin: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|pattern| {
        if pattern == "*" {
            return true;
        }
        if pattern.contains('*') {
            // Simple glob: split on '*' and check prefix/suffix
            let parts: Vec<&str> = pattern.split('*').collect();
            match parts.len() {
                2 => origin.starts_with(parts[0]) && origin.ends_with(parts[1]),
                _ => pattern == origin,
            }
        } else {
            pattern == origin
        }
    })
}

// ── Lock-free rate limiter (delegates to ShardedRateLimiter) ─

use crate::rate_limiter::ShardedRateLimiter;

// ── ACL enforcement on the message hot-path (T-04) ──────────

use clawdesk_security::acl::{AccessDecision, AclManager, Action, Principal, Resource};

/// ACL enforcement result for middleware consumers.
#[derive(Debug, Clone)]
pub struct AclCheck {
    pub allowed: bool,
    pub reason: Option<String>,
}

/// Check ACL for a message send operation.
///
/// Maps the request context to (Principal, Resource, Action) and queries
/// the AclManager. This runs on the hot path, so it uses the O(1) indexed
/// HashMap lookup in `AclManager::check()`.
pub async fn check_message_acl(
    acl: &AclManager,
    sender_id: &str,
    channel_id: &str,
) -> AclCheck {
    let principal = if sender_id == "system" || sender_id == "internal" {
        Principal::System
    } else {
        Principal::User(sender_id.to_string())
    };
    let resource = Resource::Channel(channel_id.to_string());
    let action = Action::Write;

    match acl.check(&principal, &resource, action).await {
        AccessDecision::Allow => AclCheck {
            allowed: true,
            reason: None,
        },
        AccessDecision::ConditionalAllow { .. } => AclCheck {
            allowed: true,
            reason: Some("conditional allow — conditions not yet evaluated".into()),
        },
        AccessDecision::Deny { reason } => AclCheck {
            allowed: false,
            reason: Some(reason),
        },
    }
}

/// Check ACL for tool execution by an agent.
pub async fn check_tool_acl(
    acl: &AclManager,
    agent_id: &str,
    tool_name: &str,
) -> AclCheck {
    let principal = Principal::Agent(agent_id.to_string());
    let resource = Resource::Tool(tool_name.to_string());
    let action = Action::Execute;

    match acl.check(&principal, &resource, action).await {
        AccessDecision::Allow => AclCheck {
            allowed: true,
            reason: None,
        },
        AccessDecision::ConditionalAllow { .. } => AclCheck {
            allowed: true,
            reason: Some("conditional allow".into()),
        },
        AccessDecision::Deny { reason } => AclCheck {
            allowed: false,
            reason: Some(reason),
        },
    }
}

/// Check ACL for plugin message dispatch.
pub async fn check_plugin_acl(
    acl: &AclManager,
    plugin_name: &str,
    action: Action,
) -> AclCheck {
    let principal = Principal::Plugin(plugin_name.to_string());
    let resource = Resource::Endpoint("plugin-dispatch".to_string());

    match acl.check(&principal, &resource, action).await {
        AccessDecision::Allow => AclCheck {
            allowed: true,
            reason: None,
        },
        AccessDecision::ConditionalAllow { .. } => AclCheck {
            allowed: true,
            reason: Some("conditional allow".into()),
        },
        AccessDecision::Deny { reason } => AclCheck {
            allowed: false,
            reason: Some(reason),
        },
    }
}

/// Rate limiter configuration.
#[derive(Debug, Clone)]
pub struct RateLimitConfig {
    /// Maximum burst size.
    pub capacity: u32,
    /// Tokens refilled per second.
    pub refill_per_sec: f64,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        Self {
            capacity: 60,
            refill_per_sec: 10.0,
        }
    }
}

/// Per-IP rate limiter backed by the lock-free `ShardedRateLimiter`.
///
/// All operations are lock-free on the hot path — zero mutexes, zero
/// allocations, fixed 256KB memory regardless of client count.
#[derive(Clone)]
pub struct RateLimiter {
    inner: Arc<ShardedRateLimiter>,
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            inner: Arc::new(ShardedRateLimiter::new(config.capacity, config.refill_per_sec)),
        }
    }

    /// Try to consume a token for the given key. Returns `true` if allowed.
    /// Lock-free — no mutex, no allocation.
    pub fn check(&self, key: &str) -> bool {
        self.inner.check(key)
    }
}

/// Rate-limiting middleware.
pub async fn rate_limit(
    limiter: RateLimiter,
    req: Request,
    next: Next,
) -> Response {
    // Extract client IP (from X-Forwarded-For or socket)
    let client_ip = req
        .headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .unwrap_or("unknown")
        .trim()
        .to_string();

    if !limiter.check(&client_ip) {
        warn!(%client_ip, "rate limit exceeded");
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "Rate limit exceeded. Please slow down.",
        )
            .into_response();
    }

    next.run(req).await
}

// ── Bearer-token auth guard ──────────────────────────────────

/// SHA-256 hash of an auth token. We never store the plaintext token
/// in memory — only its hash. This prevents memory-dump attacks from
/// recovering the token.
#[derive(Clone)]
pub struct HashedToken {
    hash: [u8; 32],
}

impl HashedToken {
    /// Create from a plaintext token. The plaintext is hashed immediately
    /// and never stored.
    pub fn from_plaintext(token: &str) -> Self {
        Self {
            hash: clawdesk_security::crypto::sha256(token.as_bytes()),
        }
    }

    /// Create an empty sentinel (no auth configured).
    pub fn empty() -> Self {
        Self { hash: [0u8; 32] }
    }

    pub fn is_empty(&self) -> bool {
        self.hash == [0u8; 32]
    }
}

/// Secure bearer-token auth middleware for admin routes.
///
/// Security properties:
/// - Token is hashed (SHA-256) before comparison — plaintext never stored
/// - Comparison is constant-time — no timing side-channel
/// - Token must be in Authorization header — never in URL query params
pub async fn require_auth(
    expected: HashedToken,
    req: Request,
    next: Next,
) -> Response {
    if expected.is_empty() {
        // No auth configured — allow all
        return next.run(req).await;
    }

    let auth = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    match auth {
        Some(val) if val.starts_with("Bearer ") => {
            let token = &val[7..];
            let token_hash = clawdesk_security::crypto::sha256(token.as_bytes());

            // Constant-time comparison: always examines all 32 bytes
            // regardless of where they differ. ~50ns.
            if clawdesk_security::crypto::constant_time_eq(&token_hash, &expected.hash) {
                next.run(req).await
            } else {
                (StatusCode::UNAUTHORIZED, "Invalid token").into_response()
            }
        }
        _ => (StatusCode::UNAUTHORIZED, "Missing Authorization header").into_response(),
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origin_wildcard_match() {
        let patterns = vec!["http://localhost:*".to_string()];
        assert!(origin_allowed("http://localhost:3000", &patterns));
        assert!(origin_allowed("http://localhost:18789", &patterns));
        assert!(!origin_allowed("http://example.com", &patterns));
    }

    #[test]
    fn origin_exact_match() {
        let patterns = vec!["https://app.clawdesk.dev".to_string()];
        assert!(origin_allowed("https://app.clawdesk.dev", &patterns));
        assert!(!origin_allowed("https://evil.com", &patterns));
    }

    #[test]
    fn origin_star_matches_all() {
        let patterns = vec!["*".to_string()];
        assert!(origin_allowed("http://anything.com", &patterns));
    }

    #[test]
    fn rate_limiter_allows_within_capacity() {
        let limiter = RateLimiter::new(RateLimitConfig {
            capacity: 3,
            refill_per_sec: 0.0, // no refill
        });
        assert!(limiter.check("ip1"));
        assert!(limiter.check("ip1"));
        assert!(limiter.check("ip1"));
        assert!(!limiter.check("ip1")); // exhausted
    }

    #[test]
    fn rate_limiter_separate_keys() {
        let limiter = RateLimiter::new(RateLimitConfig {
            capacity: 1,
            refill_per_sec: 0.0,
        });
        assert!(limiter.check("ip1"));
        assert!(limiter.check("ip2")); // different key
        assert!(!limiter.check("ip1"));
    }

    // ── Constant-time auth tests ─────────────────────────────

    // Use the shared crypto module for tests
    use clawdesk_security::crypto::{sha256, constant_time_eq, hmac_sha256};

    #[test]
    fn constant_time_eq_same_values() {
        let a = sha256(b"test-token-123");
        let b = sha256(b"test-token-123");
        assert!(constant_time_eq(&a, &b));
    }

    #[test]
    fn constant_time_eq_different_values() {
        let a = sha256(b"token-a");
        let b = sha256(b"token-b");
        assert!(!constant_time_eq(&a, &b));
    }

    #[test]
    fn constant_time_eq_single_bit_diff() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        b[31] = 1; // differ by one bit in the last byte
        assert!(!constant_time_eq(&a, &b));
        a[31] = 1;
        assert!(constant_time_eq(&a, &b));
    }

    #[test]
    fn hashed_token_from_plaintext() {
        let ht = HashedToken::from_plaintext("my-secret-token");
        assert!(!ht.is_empty());
        // Same plaintext should produce same hash
        let ht2 = HashedToken::from_plaintext("my-secret-token");
        assert!(constant_time_eq(&ht.hash, &ht2.hash));
        // Different plaintext should produce different hash
        let ht3 = HashedToken::from_plaintext("different-token");
        assert!(!constant_time_eq(&ht.hash, &ht3.hash));
    }

    #[test]
    fn hashed_token_empty() {
        let ht = HashedToken::empty();
        assert!(ht.is_empty());
    }

    #[test]
    fn sha256_known_vector() {
        // SHA-256("") = e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855
        let hash = sha256(b"");
        assert_eq!(
            hash,
            [
                0xe3, 0xb0, 0xc4, 0x42, 0x98, 0xfc, 0x1c, 0x14,
                0x9a, 0xfb, 0xf4, 0xc8, 0x99, 0x6f, 0xb9, 0x24,
                0x27, 0xae, 0x41, 0xe4, 0x64, 0x9b, 0x93, 0x4c,
                0xa4, 0x95, 0x99, 0x1b, 0x78, 0x52, 0xb8, 0x55,
            ]
        );
    }

    #[test]
    fn sha256_abc_vector() {
        // SHA-256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        let hash = sha256(b"abc");
        assert_eq!(
            hash,
            [
                0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea,
                0x41, 0x41, 0x40, 0xde, 0x5d, 0xae, 0x22, 0x23,
                0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c,
                0xb4, 0x10, 0xff, 0x61, 0xf2, 0x00, 0x15, 0xad,
            ]
        );
    }

    #[test]
    fn hmac_sha256_known_vector() {
        // HMAC-SHA256 with key=0x0b repeated 20 times, data="Hi There"
        // Expected: b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7
        let mut key = [0u8; 32];
        for i in 0..20 {
            key[i] = 0x0b;
        }
        let mac = hmac_sha256(&key, b"Hi There");
        assert_eq!(
            mac,
            [
                0xb0, 0x34, 0x4c, 0x61, 0xd8, 0xdb, 0x38, 0x53,
                0x5c, 0xa8, 0xaf, 0xce, 0xaf, 0x0b, 0xf1, 0x2b,
                0x88, 0x1d, 0xc2, 0x00, 0xc9, 0x83, 0x3d, 0xa7,
                0x26, 0xe9, 0x37, 0x6c, 0x2e, 0x32, 0xcf, 0xf7,
            ]
        );
    }
}
