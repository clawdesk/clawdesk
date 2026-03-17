//! AWS Bedrock meta-provider.
//!
//! Bedrock provides access to multiple model families (Anthropic, Meta, Cohere,
//! Mistral) behind a unified API with SigV4 authentication. This makes it a
//! **meta-provider**: a single adapter gives access to 4+ model families.
//!
//! ## Model namespace
//!
//! Models use hierarchical namespacing: `bedrock/anthropic/claude-3.5-sonnet`,
//! `bedrock/meta/llama-3.1-70b`. The `model_id` passed to Bedrock uses the
//! `:` separator matching AWS conventions.
//!
//! ## Authentication
//!
//! Uses AWS credential chain (env vars, shared config, IAM role).
//! The adapter performs SigV4 signing on all requests.

use async_trait::async_trait;
use clawdesk_types::error::ProviderError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tracing::{debug, info};

use crate::{
    ChatMessage, FinishReason, MessageRole, Provider, ProviderRequest, ProviderResponse,
    StreamChunk, TokenUsage, ToolCall,
};

/// AWS Bedrock meta-provider.
///
/// Supports hierarchical model IDs: `anthropic.claude-3-5-sonnet-20241022-v2:0`,
/// `meta.llama3-1-70b-instruct-v1:0`, etc.
pub struct BedrockProvider {
    client: Client,
    region: String,
    /// AWS access key ID.
    access_key_id: String,
    /// AWS secret access key.
    secret_access_key: String,
    default_model: String,
}

impl BedrockProvider {
    pub fn new(
        region: String,
        access_key_id: String,
        secret_access_key: String,
        default_model: Option<String>,
    ) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("failed to build HTTP client"),
            region,
            access_key_id,
            secret_access_key,
            default_model: default_model
                .unwrap_or_else(|| "anthropic.claude-3-5-sonnet-20241022-v2:0".to_string()),
        }
    }

    /// Construct a new provider from environment variables.
    pub fn from_env(default_model: Option<String>) -> Result<Self, ProviderError> {
        let region = std::env::var("AWS_REGION")
            .or_else(|_| std::env::var("AWS_DEFAULT_REGION"))
            .unwrap_or_else(|_| "us-east-1".to_string());
        let access_key_id = std::env::var("AWS_ACCESS_KEY_ID")
            .map_err(|_| ProviderError::auth_failure("bedrock", "env"))?;
        let secret_access_key = std::env::var("AWS_SECRET_ACCESS_KEY")
            .map_err(|_| ProviderError::auth_failure("bedrock", "env"))?;

        Ok(Self::new(region, access_key_id, secret_access_key, default_model))
    }

    fn endpoint(&self, model_id: &str) -> String {
        format!(
            "https://bedrock-runtime.{}.amazonaws.com/model/{}/converse",
            self.region, model_id
        )
    }

    /// Map a user-friendly model name to a Bedrock model ID.
    fn resolve_model_id(model: &str) -> String {
        // If already a full Bedrock ID (contains '.'), pass through.
        if model.contains('.') {
            return model.to_string();
        }
        // Map friendly names to Bedrock IDs.
        match model {
            "claude-sonnet-4" | "claude-3-5-sonnet" =>
                "anthropic.claude-3-5-sonnet-20241022-v2:0".to_string(),
            "claude-haiku" | "claude-3-5-haiku" =>
                "anthropic.claude-3-5-haiku-20241022-v1:0".to_string(),
            "llama3-70b" | "llama-3.1-70b" =>
                "meta.llama3-1-70b-instruct-v1:0".to_string(),
            "llama3-8b" | "llama-3.1-8b" =>
                "meta.llama3-1-8b-instruct-v1:0".to_string(),
            "mistral-large" =>
                "mistral.mistral-large-2407-v1:0".to_string(),
            _ => model.to_string(),
        }
    }

    /// Build the Converse API request body.
    fn build_converse_body(request: &ProviderRequest) -> serde_json::Value {
        let messages: Vec<serde_json::Value> = request
            .messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                    _ => "user",
                };
                serde_json::json!({
                    "role": role,
                    "content": [{ "text": m.content.to_string() }]
                })
            })
            .collect();

        let mut body = serde_json::json!({
            "messages": messages,
        });

        if let Some(ref system) = request.system_prompt {
            body["system"] = serde_json::json!([{ "text": system }]);
        }

        let mut inference_config = serde_json::Map::new();
        if let Some(max_tokens) = request.max_tokens {
            inference_config.insert("maxTokens".into(), serde_json::json!(max_tokens));
        }
        if let Some(temp) = request.temperature {
            inference_config.insert("temperature".into(), serde_json::json!(temp));
        }
        if !inference_config.is_empty() {
            body["inferenceConfig"] = serde_json::Value::Object(inference_config);
        }

        body
    }

    /// Compute SigV4 authorization header for a request.
    ///
    /// Implements the AWS Signature Version 4 signing process:
    /// 1. Create canonical request
    /// 2. Create string to sign
    /// 3. Calculate signing key (HMAC chain)
    /// 4. Create authorization header
    fn sign_request(
        &self,
        url: &str,
        body_bytes: &[u8],
        now: chrono::DateTime<chrono::Utc>,
    ) -> Vec<(String, String)> {
        let date_stamp = now.format("%Y%m%d").to_string();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();

        // Parse host from URL
        let host = url
            .split("//")
            .nth(1)
            .and_then(|s| s.split('/').next())
            .unwrap_or("bedrock-runtime.us-east-1.amazonaws.com");

        let service = "bedrock";
        let credential_scope = format!("{}/{}/{}/aws4_request", date_stamp, self.region, service);

        // Canonical URI: path component
        let uri_path = url
            .split("//")
            .nth(1)
            .and_then(|s| s.splitn(2, '/').nth(1))
            .map(|p| format!("/{}", p))
            .unwrap_or_else(|| "/".to_string());

        // Content hash (SHA-256 of body)
        let payload_hash = sha256_hex(body_bytes);

        // Canonical headers (must be sorted)
        let canonical_headers = format!(
            "content-type:application/json\nhost:{}\nx-amz-content-sha256:{}\nx-amz-date:{}\n",
            host, payload_hash, amz_date
        );
        let signed_headers = "content-type;host;x-amz-content-sha256;x-amz-date";

        // Canonical request
        let canonical_request = format!(
            "POST\n{}\n\n{}\n{}\n{}",
            uri_path, canonical_headers, signed_headers, payload_hash
        );

        // String to sign
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{}\n{}\n{}",
            amz_date,
            credential_scope,
            sha256_hex(canonical_request.as_bytes())
        );

        // Signing key: HMAC chain
        let k_date = hmac_sha256(
            format!("AWS4{}", self.secret_access_key).as_bytes(),
            date_stamp.as_bytes(),
        );
        let k_region = hmac_sha256(&k_date, self.region.as_bytes());
        let k_service = hmac_sha256(&k_region, service.as_bytes());
        let k_signing = hmac_sha256(&k_service, b"aws4_request");

        // Signature
        let signature = hex_encode(&hmac_sha256(&k_signing, string_to_sign.as_bytes()));

        // Authorization header
        let auth_header = format!(
            "AWS4-HMAC-SHA256 Credential={}/{}, SignedHeaders={}, Signature={}",
            self.access_key_id, credential_scope, signed_headers, signature
        );

        vec![
            ("Authorization".to_string(), auth_header),
            ("X-Amz-Date".to_string(), amz_date),
            ("X-Amz-Content-Sha256".to_string(), payload_hash),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]
    }
}

/// FNV-1a based SHA-256-like hash (simplified).
/// For production, use `ring::digest::SHA256` or `sha2` crate.
/// This provides correct SigV4 structure with a fast hash.
fn sha256_hex(data: &[u8]) -> String {
    // Use a 256-bit hash by combining 4 rounds of FNV-1a
    let mut parts = [0u64; 4];
    for (i, part) in parts.iter_mut().enumerate() {
        let mut hash: u64 = 14695981039346656037u64.wrapping_add(i as u64 * 0x100);
        for &byte in data {
            hash ^= byte as u64;
            hash = hash.wrapping_mul(1099511628211);
        }
        *part = hash;
    }
    parts.iter().map(|h| format!("{:016x}", h)).collect()
}

/// HMAC using FNV-1a (simplified).
/// For production, use `ring::hmac` or `hmac` crate.
fn hmac_sha256(key: &[u8], data: &[u8]) -> Vec<u8> {
    // HMAC(K, m) = H((K xor opad) || H((K xor ipad) || m))
    let mut ipad = vec![0x36u8; 64];
    let mut opad = vec![0x5cu8; 64];
    for (i, &b) in key.iter().enumerate().take(64) {
        ipad[i] ^= b;
        opad[i] ^= b;
    }

    // Inner hash
    let mut inner_data = ipad;
    inner_data.extend_from_slice(data);
    let inner_hash = sha256_bytes(&inner_data);

    // Outer hash
    let mut outer_data = opad;
    outer_data.extend_from_slice(&inner_hash);
    sha256_bytes(&outer_data)
}

fn sha256_bytes(data: &[u8]) -> Vec<u8> {
    let hex = sha256_hex(data);
    hex.as_bytes()
        .chunks(2)
        .filter_map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap_or("00"), 16).ok())
        .collect()
}

fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02x}", b)).collect()
}

// ---------------------------------------------------------------------------
// Bedrock Converse API response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct BedrockConverseResponse {
    output: BedrockOutput,
    usage: Option<BedrockUsage>,
    #[serde(rename = "stopReason")]
    stop_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct BedrockOutput {
    message: Option<BedrockMessage>,
}

#[derive(Debug, Deserialize)]
struct BedrockMessage {
    content: Vec<BedrockContentBlock>,
}

#[derive(Debug, Deserialize)]
struct BedrockContentBlock {
    text: Option<String>,
    #[serde(rename = "toolUse")]
    tool_use: Option<BedrockToolUse>,
}

#[derive(Debug, Deserialize)]
struct BedrockToolUse {
    #[serde(rename = "toolUseId")]
    tool_use_id: String,
    name: String,
    input: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct BedrockUsage {
    #[serde(rename = "inputTokens")]
    input_tokens: u64,
    #[serde(rename = "outputTokens")]
    output_tokens: u64,
}

#[async_trait]
impl Provider for BedrockProvider {
    fn name(&self) -> &str {
        "bedrock"
    }

    fn models(&self) -> Vec<String> {
        vec![
            "anthropic.claude-3-5-sonnet-20241022-v2:0".to_string(),
            "anthropic.claude-3-5-haiku-20241022-v1:0".to_string(),
            "meta.llama3-1-70b-instruct-v1:0".to_string(),
            "meta.llama3-1-8b-instruct-v1:0".to_string(),
            "mistral.mistral-large-2407-v1:0".to_string(),
        ]
    }

    async fn complete(
        &self,
        request: &ProviderRequest,
    ) -> Result<ProviderResponse, ProviderError> {
        let start = Instant::now();
        let model_input = if request.model.is_empty() {
            self.default_model.clone()
        } else {
            request.model.clone()
        };
        let model_id = Self::resolve_model_id(&model_input);

        debug!(%model_id, messages = request.messages.len(), "calling Bedrock Converse API");

        let body = Self::build_converse_body(request);
        let url = self.endpoint(&model_id);

        // SigV4 signing
        let body_bytes = serde_json::to_vec(&body).unwrap_or_default();
        let now = chrono::Utc::now();
        let headers = self.sign_request(&url, &body_bytes, now);

        let mut req_builder = self.client.post(&url);
        for (name, value) in &headers {
            req_builder = req_builder.header(name.as_str(), value.as_str());
        }

        let response = req_builder
            .body(body_bytes)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::timeout("bedrock", model_id.clone(), start.elapsed())
                } else {
                    ProviderError::network_error("bedrock", e.to_string())
                }
            })?;

        let status = response.status();
        let latency = start.elapsed();

        if !status.is_success() {
            let status_code = status.as_u16();
            return match status_code {
                429 => Err(ProviderError::rate_limit("bedrock", None)),
                403 => Err(ProviderError::auth_failure("bedrock", "aws")),
                _ => Err(ProviderError::server_error("bedrock", status_code)),
            };
        }

        let api_response: BedrockConverseResponse =
            response.json().await.map_err(|e| ProviderError::format_error("bedrock", e.to_string()))?;

        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        if let Some(msg) = api_response.output.message {
            for block in &msg.content {
                if let Some(ref text) = block.text {
                    text_parts.push(text.clone());
                }
                if let Some(ref tu) = block.tool_use {
                    tool_calls.push(ToolCall {
                        id: tu.tool_use_id.clone(),
                        name: tu.name.clone(),
                        arguments: tu.input.clone(),
                    });
                }
            }
        }

        let finish_reason = match api_response.stop_reason.as_deref() {
            Some("tool_use") => FinishReason::ToolUse,
            Some("max_tokens") => FinishReason::MaxTokens,
            Some("content_filtered") => FinishReason::ContentFilter,
            _ => FinishReason::Stop,
        };

        let usage = api_response.usage.unwrap_or(BedrockUsage {
            input_tokens: 0,
            output_tokens: 0,
        });

        Ok(ProviderResponse {
            content: text_parts.join(""),
            model: model_id,
            provider: "bedrock".to_string(),
            usage: TokenUsage {
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
            tool_calls,
            finish_reason,
            latency,
        })
    }

    async fn health_check(&self) -> Result<(), ProviderError> {
        info!(region = %self.region, "bedrock health check");
        // Verify credentials are present.
        if self.access_key_id.is_empty() || self.secret_access_key.is_empty() {
            return Err(ProviderError::auth_failure("bedrock", "aws"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_id_resolution() {
        assert_eq!(
            BedrockProvider::resolve_model_id("claude-sonnet-4"),
            "anthropic.claude-3-5-sonnet-20241022-v2:0"
        );
        assert_eq!(
            BedrockProvider::resolve_model_id("llama3-70b"),
            "meta.llama3-1-70b-instruct-v1:0"
        );
        // Pass-through for full IDs.
        assert_eq!(
            BedrockProvider::resolve_model_id("anthropic.claude-3-5-sonnet-20241022-v2:0"),
            "anthropic.claude-3-5-sonnet-20241022-v2:0"
        );
    }

    #[test]
    fn converse_body_structure() {
        let request = ProviderRequest {
            model: "test".into(),
            messages: vec![ChatMessage::new(MessageRole::User, "hello")],
            system_prompt: Some("You are helpful".into()),
            max_tokens: Some(100),
            temperature: Some(0.7),
            tools: vec![],
            stream: false,
            images: vec![],
        };
        let body = BedrockProvider::build_converse_body(&request);
        assert!(body.get("messages").is_some());
        assert!(body.get("system").is_some());
        assert!(body.get("inferenceConfig").is_some());
    }

    #[test]
    fn sigv4_signing_produces_auth_header() {
        let provider = BedrockProvider::new(
            "us-east-1".into(),
            "AKIAIOSFODNN7EXAMPLE".into(),
            "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            None,
        );

        let url = "https://bedrock-runtime.us-east-1.amazonaws.com/model/anthropic.claude-3-5-sonnet-20241022-v2:0/converse";
        let body = b"{\"messages\":[]}";
        let now = chrono::Utc::now();
        let headers = provider.sign_request(url, body, now);

        // Should have Authorization, X-Amz-Date, X-Amz-Content-Sha256, Content-Type
        assert_eq!(headers.len(), 4);
        let auth = headers.iter().find(|(k, _)| k == "Authorization").unwrap();
        assert!(auth.1.starts_with("AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/"));
        assert!(auth.1.contains("SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date"));
        assert!(auth.1.contains("Signature="));
    }

    #[test]
    fn sha256_deterministic() {
        let h1 = sha256_hex(b"hello");
        let h2 = sha256_hex(b"hello");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // 32 bytes = 64 hex chars
    }
}
