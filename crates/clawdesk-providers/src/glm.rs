//! GLM / ZhipuAI provider — Chinese LLM with JWT HMAC-SHA256 authentication.
//!
//! GLM (General Language Model) by ZhipuAI (`api.z.ai`) uses API keys in
//! `id.secret` format. Authentication requires generating short-lived JWTs
//! signed with HMAC-SHA256, where the key secret is used as the signing key.
//!
//! ## Authentication
//!
//! 1. API key format: `{key_id}.{key_secret}`
//! 2. Generate JWT: header `{"alg":"HS256","sign_type":"SIGN"}` +
//!    payload `{"api_key":"{key_id}","exp":{now+210},"timestamp":{now}}`
//! 3. Sign with HMAC-SHA256 using `key_secret` as the key
//! 4. Cache token for 3 minutes (token expiry is 3.5 minutes)
//!
//! This implementation uses `hmac` + `sha2` crates available in the workspace.

use async_trait::async_trait;
use clawdesk_types::error::ProviderError;
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::sync::Mutex;
use tracing::debug;

use crate::{
    FinishReason, Provider, ProviderRequest, ProviderResponse,
    TokenUsage,
};

const GLM_BASE_URL: &str = "https://open.bigmodel.cn/api/paas/v4";

type HmacSha256 = Hmac<Sha256>;

// ---------------------------------------------------------------------------
// JWT token management
// ---------------------------------------------------------------------------

/// Cached JWT token with expiry timestamp.
#[derive(Debug)]
struct TokenCache {
    token: String,
    cached_at: u64,
}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct CompletionRequest {
    model: String,
    messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
}

#[derive(Serialize)]
struct WireMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct CompletionResponse {
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<UsageResponse>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ResponseMessage {
    #[serde(default)]
    content: Option<String>,
}

#[derive(Deserialize)]
struct UsageResponse {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// GLM (ZhipuAI) provider with JWT HMAC-SHA256 authentication.
///
/// Uses API keys in `id.secret` format and generates short-lived JWT tokens
/// signed with the key secret.
#[derive(Debug)]
pub struct GlmProvider {
    client: Client,
    api_key_id: String,
    api_key_secret: String,
    base_url: String,
    default_model: String,
    /// Cached JWT token — reused within 3-minute window.
    token_cache: Mutex<Option<TokenCache>>,
}

impl GlmProvider {
    /// Create a new GLM provider.
    ///
    /// `api_key` must be in `id.secret` format.
    pub fn new(api_key: &str) -> Result<Self, ProviderError> {
        let parts: Vec<&str> = api_key.splitn(2, '.').collect();
        if parts.len() != 2 {
            return Err(ProviderError::AuthFailure {
                provider: "glm".into(),
                profile_id: "API key must be in 'id.secret' format".into(),
            });
        }

        Ok(Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .pool_max_idle_per_host(2)
                .build()
                .expect("failed to build HTTP client"),
            api_key_id: parts[0].to_string(),
            api_key_secret: parts[1].to_string(),
            base_url: GLM_BASE_URL.to_string(),
            default_model: "glm-4-plus".into(),
            token_cache: Mutex::new(None),
        })
    }

    /// Create with env var fallback.
    pub fn from_env() -> Result<Self, ProviderError> {
        let key = std::env::var("GLM_API_KEY")
            .or_else(|_| std::env::var("ZHIPUAI_API_KEY"))
            .map_err(|_| ProviderError::AuthFailure {
                provider: "glm".into(),
                profile_id: "set GLM_API_KEY or ZHIPUAI_API_KEY env var".into(),
            })?;

        Self::new(&key)
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into();
        self
    }

    pub fn with_default_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }

    /// Generate or retrieve a cached JWT token.
    fn get_token(&self) -> Result<String, ProviderError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Check cache (3-minute window)
        if let Ok(guard) = self.token_cache.lock() {
            if let Some(ref cached) = *guard {
                if now - cached.cached_at < 180 {
                    return Ok(cached.token.clone());
                }
            }
        }

        // Generate new JWT
        let token = self.generate_jwt(now)?;

        // Cache it
        if let Ok(mut guard) = self.token_cache.lock() {
            *guard = Some(TokenCache {
                token: token.clone(),
                cached_at: now,
            });
        }

        Ok(token)
    }

    /// Generate a JWT signed with HMAC-SHA256.
    ///
    /// Format: base64url(header).base64url(payload).base64url(signature)
    fn generate_jwt(&self, now: u64) -> Result<String, ProviderError> {
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;

        // Header: {"alg":"HS256","sign_type":"SIGN"}
        let header = serde_json::json!({
            "alg": "HS256",
            "sign_type": "SIGN"
        });
        let header_b64 = URL_SAFE_NO_PAD.encode(header.to_string().as_bytes());

        // Payload: {"api_key":"...", "exp":..., "timestamp":...}
        let exp = now + 210; // 3.5 minutes
        let payload = serde_json::json!({
            "api_key": self.api_key_id,
            "exp": exp,
            "timestamp": now
        });
        let payload_b64 = URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());

        // Sign: HMAC-SHA256(key_secret, header_b64.payload_b64)
        let signing_input = format!("{header_b64}.{payload_b64}");

        let mut mac = HmacSha256::new_from_slice(self.api_key_secret.as_bytes())
            .map_err(|e| ProviderError::FormatError {
                provider: "glm".into(),
                detail: format!("HMAC init failed: {e}"),
            })?;
        mac.update(signing_input.as_bytes());
        let signature = mac.finalize().into_bytes();
        let sig_b64 = URL_SAFE_NO_PAD.encode(&signature);

        Ok(format!("{signing_input}.{sig_b64}"))
    }
}

#[async_trait]
impl Provider for GlmProvider {
    fn name(&self) -> &str {
        "glm"
    }

    fn models(&self) -> Vec<String> {
        vec![
            "glm-4-plus".into(),
            "glm-4-flash".into(),
            "glm-4-air".into(),
            "glm-4".into(),
            "glm-4v".into(),
        ]
    }

    async fn complete(
        &self,
        request: &ProviderRequest,
    ) -> Result<ProviderResponse, ProviderError> {
        let token = self.get_token()?;
        let model = if request.model.is_empty() {
            &self.default_model
        } else {
            &request.model
        };

        let mut messages = Vec::new();
        if let Some(ref sys) = request.system_prompt {
            messages.push(WireMessage {
                role: "system".into(),
                content: sys.clone(),
            });
        }
        for msg in &request.messages {
            messages.push(WireMessage {
                role: msg.role.as_str().to_string(),
                content: msg.content.to_string(),
            });
        }

        let body = CompletionRequest {
            model: model.to_string(),
            messages,
            max_tokens: request.max_tokens,
            temperature: request.temperature,
        };

        let start = std::time::Instant::now();
        let url = format!("{}/chat/completions", self.base_url);

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout {
                        provider: "glm".into(),
                        model: model.to_string(),
                        after: std::time::Duration::from_secs(120),
                    }
                } else {
                    ProviderError::NetworkError {
                        provider: "glm".into(),
                        detail: e.to_string(),
                    }
                }
            })?;

        let status = resp.status().as_u16();
        if status != 200 {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(match status {
                429 => ProviderError::RateLimit {
                    provider: "glm".into(),
                    retry_after: None,
                },
                401 | 403 => ProviderError::AuthFailure {
                    provider: "glm".into(),
                    profile_id: String::new(),
                },
                s if s >= 500 => ProviderError::ServerError {
                    provider: "glm".into(),
                    status: s,
                },
                _ => ProviderError::FormatError {
                    provider: "glm".into(),
                    detail: format!("HTTP {status}: {body_text}"),
                },
            });
        }

        let resp_body: CompletionResponse =
            resp.json().await.map_err(|e| ProviderError::FormatError {
                provider: "glm".into(),
                detail: format!("JSON parse error: {e}"),
            })?;

        let choice = resp_body
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ProviderError::FormatError {
                provider: "glm".into(),
                detail: "no choices".into(),
            })?;

        let usage = resp_body.usage.map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cache_read_tokens: None,
            cache_write_tokens: None,
        });

        Ok(ProviderResponse {
            content: choice.message.content.unwrap_or_default(),
            model: resp_body.model.unwrap_or_else(|| model.to_string()),
            provider: "glm".into(),
            usage: usage.unwrap_or_default(),
            tool_calls: Vec::new(),
            finish_reason: match choice.finish_reason.as_deref() {
                Some("stop") => FinishReason::Stop,
                Some("length") => FinishReason::MaxTokens,
                _ => FinishReason::Stop,
            },
            latency: start.elapsed(),
        })
    }

    // No native streaming — uses default single-chunk fallback.

    async fn health_check(&self) -> Result<(), ProviderError> {
        // Validate JWT generation works
        let _token = self.get_token()?;
        debug!("glm health check passed");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_parse_valid() {
        let p = GlmProvider::new("myid.mysecret").unwrap();
        assert_eq!(p.api_key_id, "myid");
        assert_eq!(p.api_key_secret, "mysecret");
    }

    #[test]
    fn test_key_parse_invalid() {
        let err = GlmProvider::new("bad-key").unwrap_err();
        match err {
            ProviderError::AuthFailure { .. } => {}
            other => panic!("expected AuthFailure, got {other:?}"),
        }
    }

    #[test]
    fn test_jwt_generation() {
        let p = GlmProvider::new("test-id.test-secret-key-value").unwrap();
        let jwt = p.generate_jwt(1700000000).unwrap();

        // JWT has 3 parts
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);

        // Header decodes correctly
        use base64::engine::general_purpose::URL_SAFE_NO_PAD;
        use base64::Engine;
        let header_json = String::from_utf8(URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        let header: serde_json::Value = serde_json::from_str(&header_json).unwrap();
        assert_eq!(header["alg"], "HS256");
        assert_eq!(header["sign_type"], "SIGN");

        // Payload decodes correctly
        let payload_json = String::from_utf8(URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        let payload: serde_json::Value = serde_json::from_str(&payload_json).unwrap();
        assert_eq!(payload["api_key"], "test-id");
        assert_eq!(payload["exp"], 1700000210);
        assert_eq!(payload["timestamp"], 1700000000);
    }

    #[test]
    fn test_token_caching() {
        let p = GlmProvider::new("test-id.test-secret").unwrap();
        let t1 = p.get_token().unwrap();
        let t2 = p.get_token().unwrap();
        // Same token within cache window
        assert_eq!(t1, t2);
    }

    #[test]
    fn test_provider_name() {
        let p = GlmProvider::new("id.secret").unwrap();
        assert_eq!(p.name(), "glm");
    }
}
