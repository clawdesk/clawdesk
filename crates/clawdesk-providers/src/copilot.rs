//! GitHub Copilot provider — authenticate via GitHub OAuth device code flow.
//!
//! Uses GitHub's device code OAuth flow to obtain a Copilot API token,
//! then routes chat completions through the Copilot API using OpenAI's
//! chat format.
//!
//! ## Authentication Flow
//!
//! 1. Request device code from `github.com/login/device/code`
//! 2. User visits `github.com/login/device` and enters the code
//! 3. Poll `github.com/login/oauth/access_token` until authorized
//! 4. Exchange GitHub token → Copilot API key via `api.github.com/copilot_internal/v2/token`
//! 5. Cache token to disk for reuse, refresh when within 120s of expiry
//!
//! Implements the full device code flow with disk caching, mutex-guarded
//! refresh, and VS Code editor spoofing.

use async_trait::async_trait;
use clawdesk_types::error::ProviderError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::{
    FinishReason, MessageRole, Provider, ProviderRequest, ProviderResponse,
    ToolCall, ToolDefinition, TokenUsage,
};

// GitHub OAuth client ID (VS Code's registered app)
const GITHUB_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";
const COPILOT_CHAT_URL: &str = "https://api.githubcopilot.com/chat/completions";
const COPILOT_TOKEN_URL: &str = "https://api.github.com/copilot_internal/v2/token";

// ---------------------------------------------------------------------------
// Token management
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedApiKey {
    token: String,
    expires_at: u64,
}

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    interval: u64,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct CopilotTokenResponse {
    token: String,
    expires_at: u64,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<WireTool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
}

#[derive(Serialize)]
struct WireMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<WireToolCallOut>>,
}

#[derive(Serialize)]
struct WireToolCallOut {
    id: String,
    r#type: String,
    function: WireFunctionOut,
}

#[derive(Serialize)]
struct WireFunctionOut {
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct WireTool {
    r#type: String,
    function: WireFunction,
}

#[derive(Serialize)]
struct WireFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
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
    #[serde(default)]
    tool_calls: Option<Vec<ResponseToolCall>>,
}

#[derive(Deserialize)]
struct ResponseToolCall {
    id: String,
    function: ResponseFunction,
}

#[derive(Deserialize)]
struct ResponseFunction {
    name: String,
    arguments: String,
}

#[derive(Deserialize)]
struct UsageResponse {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

// ---------------------------------------------------------------------------
// Provider implementation
// ---------------------------------------------------------------------------

/// GitHub Copilot chat provider.
///
/// Authenticates via GitHub OAuth device code flow and routes requests
/// through the Copilot Chat API. Tokens are cached to disk and
/// automatically refreshed.
pub struct CopilotProvider {
    client: Client,
    /// GitHub access token (obtained via device flow or provided directly).
    github_token: Option<String>,
    /// Mutex-guarded cached Copilot API key.
    cached_key: Arc<Mutex<Option<CachedApiKey>>>,
    /// Directory for token persistence.
    token_dir: PathBuf,
    default_model: String,
}

impl CopilotProvider {
    /// Create a new Copilot provider.
    ///
    /// If `github_token` is `None`, the device code flow will be triggered
    /// on first use.
    pub fn new(github_token: Option<String>) -> Self {
        let token_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("clawdesk")
            .join("copilot");

        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .pool_max_idle_per_host(4)
                .build()
                .expect("failed to build HTTP client"),
            github_token,
            cached_key: Arc::new(Mutex::new(None)),
            token_dir,
            default_model: "gpt-4o".into(),
        }
    }

    /// Create with a specific token directory.
    pub fn with_token_dir(mut self, dir: PathBuf) -> Self {
        self.token_dir = dir;
        self
    }

    /// Set the default model.
    pub fn with_default_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }

    /// Get a valid Copilot API token, refreshing if needed.
    async fn get_api_token(&self) -> Result<String, ProviderError> {
        let mut cached = self.cached_key.lock().await;

        // Check if existing token is still valid (with 120s buffer)
        if let Some(ref key) = *cached {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            if key.expires_at > now + 120 {
                return Ok(key.token.clone());
            }
        }

        // Try loading from disk
        let disk_path = self.token_dir.join("copilot_token.json");
        if disk_path.exists() {
            if let Ok(data) = tokio::fs::read_to_string(&disk_path).await {
                if let Ok(key) = serde_json::from_str::<CachedApiKey>(&data) {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();

                    if key.expires_at > now + 120 {
                        *cached = Some(key.clone());
                        return Ok(key.token);
                    }
                }
            }
        }

        // Need a GitHub token to exchange
        let github_token = self.github_token.as_deref().ok_or_else(|| {
            ProviderError::AuthFailure {
                provider: "copilot".into(),
                profile_id: "Run device code flow to authenticate with GitHub".into(),
            }
        })?;

        // Exchange GitHub token for Copilot API key
        let resp = self
            .client
            .get(COPILOT_TOKEN_URL)
            .header("Authorization", format!("token {github_token}"))
            .header("Accept", "application/json")
            .header("User-Agent", "clawdesk/1.0")
            .header("Editor-Version", "vscode/1.85.1")
            .header("Editor-Plugin-Version", "copilot/1.155.0")
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError {
                provider: "copilot".into(),
                detail: e.to_string(),
            })?;

        let status = resp.status().as_u16();
        if status != 200 {
            let body = resp.text().await.unwrap_or_default();
            return Err(ProviderError::AuthFailure {
                provider: "copilot".into(),
                profile_id: format!("token exchange failed: HTTP {status}: {body}"),
            });
        }

        let token_resp: CopilotTokenResponse =
            resp.json().await.map_err(|e| ProviderError::FormatError {
                provider: "copilot".into(),
                detail: format!("token parse error: {e}"),
            })?;

        let key = CachedApiKey {
            token: token_resp.token.clone(),
            expires_at: token_resp.expires_at,
        };

        // Persist to disk
        if let Err(e) = tokio::fs::create_dir_all(&self.token_dir).await {
            warn!("failed to create copilot token dir: {e}");
        }
        if let Ok(json) = serde_json::to_string_pretty(&key) {
            if let Err(e) = tokio::fs::write(&disk_path, json).await {
                warn!("failed to write copilot token: {e}");
            } else {
                // Set file permissions to 0o600 (owner read/write only)
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(
                        &disk_path,
                        std::fs::Permissions::from_mode(0o600),
                    );
                }
            }
        }

        *cached = Some(key);
        Ok(token_resp.token)
    }

    /// Run the GitHub OAuth device code flow interactively.
    ///
    /// Returns a GitHub access token that can be used with `new(Some(token))`.
    pub async fn device_code_flow() -> Result<String, ProviderError> {
        let client = Client::new();

        // Step 1: Request device code
        let resp = client
            .post("https://github.com/login/device/code")
            .header("Accept", "application/json")
            .form(&[
                ("client_id", GITHUB_CLIENT_ID),
                ("scope", "read:user"),
            ])
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError {
                provider: "copilot".into(),
                detail: e.to_string(),
            })?;

        let device: DeviceCodeResponse = resp.json().await.map_err(|e| {
            ProviderError::FormatError {
                provider: "copilot".into(),
                detail: format!("device code parse error: {e}"),
            }
        })?;

        info!(
            "GitHub Device Code Flow:\n  \
             Visit: {}\n  \
             Enter code: {}",
            device.verification_uri, device.user_code
        );

        // Step 2: Poll for authorization
        let interval = std::time::Duration::from_secs(device.interval.max(5));
        loop {
            tokio::time::sleep(interval).await;

            let resp = client
                .post("https://github.com/login/oauth/access_token")
                .header("Accept", "application/json")
                .form(&[
                    ("client_id", GITHUB_CLIENT_ID),
                    ("device_code", device.device_code.as_str()),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ])
                .send()
                .await
                .map_err(|e| ProviderError::NetworkError {
                    provider: "copilot".into(),
                    detail: e.to_string(),
                })?;

            let token_resp: TokenResponse = resp.json().await.map_err(|e| {
                ProviderError::FormatError {
                    provider: "copilot".into(),
                    detail: format!("token poll parse error: {e}"),
                }
            })?;

            if let Some(token) = token_resp.access_token {
                info!("GitHub authentication successful");
                return Ok(token);
            }

            match token_resp.error.as_deref() {
                Some("authorization_pending") => {
                    debug!("waiting for user authorization...");
                    continue;
                }
                Some("slow_down") => {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                    continue;
                }
                Some("expired_token") => {
                    return Err(ProviderError::AuthFailure {
                        provider: "copilot".into(),
                        profile_id: "device code expired — please restart".into(),
                    });
                }
                Some(other) => {
                    return Err(ProviderError::AuthFailure {
                        provider: "copilot".into(),
                        profile_id: format!("auth error: {other}"),
                    });
                }
                None => continue,
            }
        }
    }

    fn build_messages(request: &ProviderRequest) -> Vec<WireMessage> {
        let mut msgs = Vec::new();

        if let Some(ref sys) = request.system_prompt {
            msgs.push(WireMessage {
                role: "system".into(),
                content: sys.clone(),
                tool_call_id: None,
                tool_calls: None,
            });
        }

        for msg in &request.messages {
            let role_str = msg.role.as_str().to_string();
            let content = msg.content.to_string();

            if msg.role == MessageRole::Assistant {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(tcs) = parsed.get("tool_calls").and_then(|v| v.as_array()) {
                        let wire_tcs: Vec<WireToolCallOut> = tcs
                            .iter()
                            .filter_map(|tc| {
                                Some(WireToolCallOut {
                                    id: tc.get("id")?.as_str()?.to_string(),
                                    r#type: "function".to_string(),
                                    function: WireFunctionOut {
                                        name: tc.get("function")?.get("name")?.as_str()?.to_string(),
                                        arguments: tc.get("function")?.get("arguments")?.as_str()?.to_string(),
                                    },
                                })
                            })
                            .collect();

                        if !wire_tcs.is_empty() {
                            let text_content = parsed
                                .get("content")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            msgs.push(WireMessage {
                                role: role_str,
                                content: text_content,
                                tool_call_id: None,
                                tool_calls: Some(wire_tcs),
                            });
                            continue;
                        }
                    }
                }
            }

            if msg.role == MessageRole::Tool {
                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) {
                    let tool_call_id = parsed
                        .get("tool_call_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let result_content = parsed
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&content)
                        .to_string();
                    msgs.push(WireMessage {
                        role: "tool".into(),
                        content: result_content,
                        tool_call_id: Some(tool_call_id),
                        tool_calls: None,
                    });
                    continue;
                }
            }

            msgs.push(WireMessage {
                role: role_str,
                content,
                tool_call_id: None,
                tool_calls: None,
            });
        }

        msgs
    }

    fn build_tools(tools: &[ToolDefinition]) -> Vec<WireTool> {
        tools
            .iter()
            .map(|t| WireTool {
                r#type: "function".to_string(),
                function: WireFunction {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    parameters: t.parameters.clone(),
                },
            })
            .collect()
    }

    fn parse_finish_reason(reason: Option<&str>) -> FinishReason {
        match reason {
            Some("stop") => FinishReason::Stop,
            Some("tool_calls") | Some("function_call") => FinishReason::ToolUse,
            Some("length") => FinishReason::MaxTokens,
            Some("content_filter") => FinishReason::ContentFilter,
            _ => FinishReason::Stop,
        }
    }
}

#[async_trait]
impl Provider for CopilotProvider {
    fn name(&self) -> &str {
        "copilot"
    }

    fn models(&self) -> Vec<String> {
        vec![
            "gpt-4o".into(),
            "gpt-4o-mini".into(),
            "o1".into(),
            "o1-mini".into(),
            "claude-sonnet-4-20250514".into(),
        ]
    }

    async fn complete(
        &self,
        request: &ProviderRequest,
    ) -> Result<ProviderResponse, ProviderError> {
        let api_token = self.get_api_token().await?;
        let model = if request.model.is_empty() {
            &self.default_model
        } else {
            &request.model
        };

        let tools = if !request.tools.is_empty() {
            Some(Self::build_tools(&request.tools))
        } else {
            None
        };

        let body = CompletionRequest {
            model: model.to_string(),
            messages: Self::build_messages(request),
            max_tokens: request.max_tokens,
            temperature: request.temperature,
            tools,
            stream: None,
        };

        let start = std::time::Instant::now();

        let resp = self
            .client
            .post(COPILOT_CHAT_URL)
            .header("Authorization", format!("Bearer {api_token}"))
            .header("Editor-Version", "vscode/1.85.1")
            .header("Editor-Plugin-Version", "copilot/1.155.0")
            .header("Openai-Intent", "conversation-panel")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout {
                        provider: "copilot".into(),
                        model: model.to_string(),
                        after: std::time::Duration::from_secs(120),
                    }
                } else {
                    ProviderError::NetworkError {
                        provider: "copilot".into(),
                        detail: e.to_string(),
                    }
                }
            })?;

        let status = resp.status().as_u16();
        if status != 200 {
            let body_text = resp.text().await.unwrap_or_default();
            return Err(match status {
                429 => ProviderError::RateLimit {
                    provider: "copilot".into(),
                    retry_after: None,
                },
                401 | 403 => {
                    // Invalidate cached token
                    *self.cached_key.lock().await = None;
                    ProviderError::AuthFailure {
                        provider: "copilot".into(),
                        profile_id: String::new(),
                    }
                }
                s if s >= 500 => ProviderError::ServerError {
                    provider: "copilot".into(),
                    status: s,
                },
                _ => ProviderError::FormatError {
                    provider: "copilot".into(),
                    detail: format!("HTTP {status}: {body_text}"),
                },
            });
        }

        let resp_body: CompletionResponse =
            resp.json().await.map_err(|e| ProviderError::FormatError {
                provider: "copilot".into(),
                detail: format!("JSON parse error: {e}"),
            })?;

        let choice = resp_body
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| ProviderError::FormatError {
                provider: "copilot".into(),
                detail: "no choices".into(),
            })?;

        let content = choice.message.content.unwrap_or_default();
        let finish_reason = Self::parse_finish_reason(choice.finish_reason.as_deref());

        let tool_calls: Vec<ToolCall> = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| ToolCall {
                id: tc.id,
                name: tc.function.name,
                arguments: serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(serde_json::Value::String(tc.function.arguments)),
            })
            .collect();

        let usage = resp_body.usage.map(|u| TokenUsage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            cache_read_tokens: None,
            cache_write_tokens: None,
        });

        Ok(ProviderResponse {
            content,
            model: resp_body.model.unwrap_or_else(|| model.to_string()),
            provider: "copilot".into(),
            usage: usage.unwrap_or_default(),
            tool_calls,
            finish_reason,
            latency: start.elapsed(),
        })
    }

    async fn health_check(&self) -> Result<(), ProviderError> {
        // Validate we can get an API token
        let _token = self.get_api_token().await?;
        debug!("copilot health check passed");
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ChatMessage;

    #[test]
    fn test_parse_finish_reason() {
        assert_eq!(CopilotProvider::parse_finish_reason(Some("stop")), FinishReason::Stop);
        assert_eq!(CopilotProvider::parse_finish_reason(Some("tool_calls")), FinishReason::ToolUse);
        assert_eq!(CopilotProvider::parse_finish_reason(Some("length")), FinishReason::MaxTokens);
    }

    #[test]
    fn test_build_messages_with_system() {
        let request = ProviderRequest {
            model: "gpt-4o".into(),
            messages: vec![ChatMessage::new(MessageRole::User, "Hello")],
            system_prompt: Some("Be helpful".into()),
            max_tokens: None,
            temperature: None,
            tools: Vec::new(),
            stream: false,
        };

        let msgs = CopilotProvider::build_messages(&request);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs[0].content, "Be helpful");
        assert_eq!(msgs[1].role, "user");
        assert_eq!(msgs[1].content, "Hello");
    }

    #[test]
    fn test_provider_name_and_models() {
        let provider = CopilotProvider::new(None);
        assert_eq!(provider.name(), "copilot");
        assert!(provider.models().contains(&"gpt-4o".to_string()));
    }
}
