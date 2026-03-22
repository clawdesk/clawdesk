//! Google Cloud Vertex AI provider adapter.

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, Utc};
use clawdesk_types::error::ProviderError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, warn};

use crate::{
    ChatMessage, FinishReason, MessageRole, Provider, ProviderRequest, ProviderResponse,
    StreamChunk, ToolCall, ToolDefinition, TokenUsage,
};

/// Google Vertex AI provider.
pub struct VertexProvider {
    client: Client,
    project_id: String,
    location: String,
    default_model: String,
    token_cache: Arc<RwLock<TokenCache>>,
}

struct TokenCache {
    access_token: Option<String>,
    expires_at: i64,
}

impl VertexProvider {
    pub fn new(project_id: String, location: String, default_model: Option<String>) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("failed to build HTTP client"),
            project_id,
            location,
            default_model: default_model.unwrap_or_else(|| "gemini-1.5-pro-preview-0409".to_string()),
            token_cache: Arc::new(RwLock::new(TokenCache {
                access_token: None,
                expires_at: 0,
            })),
        }
    }

    fn model_url(&self, model: &str, method: &str) -> String {
        let base_url = if self.location == "global" {
            format!("https://aiplatform.googleapis.com/v1/projects/{}/locations/global/publishers", self.project_id)
        } else {
            format!("https://{}-aiplatform.googleapis.com/v1/projects/{}/locations/{}/publishers", self.location, self.project_id, self.location)
        };
        format!("{}/google/models/{}:{}", base_url, model, method)
    }

    /// Fetches and caches Google Application Default Credentials access token.
    async fn get_access_token(&self) -> Result<String, ProviderError> {
        let now = Utc::now().timestamp();
        
        {
            let cache = self.token_cache.read().await;
            if let Some(token) = &cache.access_token {
                // Buffer of 60 seconds
                if cache.expires_at > now + 60 {
                    return Ok(token.clone());
                }
            }
        }

        let adc_file = Self::default_adc_file().ok_or_else(|| ProviderError::auth_failure("vertex".to_string(), "default".to_string()))?;

        let data = tokio::fs::read_to_string(adc_file).await.map_err(|_| ProviderError::auth_failure("vertex", "default"))?;
        
        let adc_json: Value = serde_json::from_str(&data).map_err(|_| ProviderError::auth_failure("vertex", "default"))?;

        let (client_id, client_secret, refresh_token) = match (
            adc_json["client_id"].as_str(),
            adc_json["client_secret"].as_str(),
            adc_json["refresh_token"].as_str(),
        ) {
            (Some(cid), Some(cs), Some(rt)) => (cid, cs, rt),
            _ => return Err(ProviderError::auth_failure("vertex", "default")),
        };

        let req_body = serde_json::json!({
            "client_id": client_id,
            "client_secret": client_secret,
            "refresh_token": refresh_token,
            "grant_type": "refresh_token",
        });

        let value: Value = self.client
            .post("https://oauth2.googleapis.com/token")
            .json(&req_body)
            .send()
            .await
            .map_err(|e| ProviderError::network_error("vertex", e.to_string()))?
            .json()
            .await
            .map_err(|e| ProviderError::format_error("vertex", e.to_string()))?;

        if let (Some(access_token), Some(expires_in)) = (
            value["access_token"].as_str(),
            value["expires_in"].as_i64(),
        ) {
            let expires_at = Utc::now() + ChronoDuration::try_seconds(expires_in).unwrap_or(ChronoDuration::zero());
            let token = access_token.to_string();
            
            let mut cache = self.token_cache.write().await;
            cache.access_token = Some(token.clone());
            cache.expires_at = expires_at.timestamp();
            
            Ok(token)
        } else {
            Err(ProviderError::auth_failure("vertex", "default"))
        }
    }

    #[cfg(not(windows))]
    fn default_adc_file() -> Option<PathBuf> {
        let mut path = dirs::home_dir()?;
        path.push(".config");
        path.push("gcloud");
        path.push("application_default_credentials.json");
        Some(path)
    }

    #[cfg(windows)]
    fn default_adc_file() -> Option<PathBuf> {
        let mut path = dirs::config_dir()?;
        path.push("gcloud");
        path.push("application_default_credentials.json");
        Some(path)
    }

    /// Translate ClawDesk ToolDefinition (JSON Schema) to Gemini FunctionDeclaration.
    fn translate_tools(tools: &[ToolDefinition]) -> Option<serde_json::Value> {
        if tools.is_empty() {
            return None;
        }

        let declarations: Vec<serde_json::Value> = tools
            .iter()
            .map(|tool| {
                let params = Self::json_schema_to_openapi(&tool.parameters);
                serde_json::json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": params,
                })
            })
            .collect();

        Some(serde_json::json!([{ "functionDeclarations": declarations }]))
    }

    fn json_schema_to_openapi(schema: &serde_json::Value) -> serde_json::Value {
        let mut result = serde_json::Map::new();

        if let Some(obj) = schema.as_object() {
            // Warn about unsupported constructs.
            for key in &["$ref", "oneOf", "allOf", "anyOf"] {
                if obj.contains_key(*key) {
                    warn!(key, "vertex: stripping unsupported JSON Schema construct");
                }
            }

            if let Some(t) = obj.get("type") {
                result.insert("type".into(), t.clone());
            }
            if let Some(d) = obj.get("description") {
                result.insert("description".into(), d.clone());
            }
            if let Some(e) = obj.get("enum") {
                result.insert("enum".into(), e.clone());
            }
            if let Some(items) = obj.get("items") {
                result.insert("items".into(), Self::json_schema_to_openapi(items));
            }
            if let Some(props) = obj.get("properties").and_then(|p| p.as_object()) {
                let mut converted = serde_json::Map::new();
                for (key, val) in props {
                    converted.insert(key.clone(), Self::json_schema_to_openapi(val));
                }
                result.insert("properties".into(), serde_json::Value::Object(converted));
            }
            if let Some(req) = obj.get("required") {
                result.insert("required".into(), req.clone());
            }
        }

        serde_json::Value::Object(result)
    }

    fn build_contents(
        messages: &[ChatMessage],
        system_prompt: &Option<String>,
    ) -> (Option<serde_json::Value>, Vec<serde_json::Value>) {
        let system_instruction = system_prompt.as_ref().map(|s| {
            serde_json::json!({
                "parts": [{ "text": s }]
            })
        });

        let mut contents: Vec<serde_json::Value> = Vec::new();
        let mut current_role = String::new();
        let mut current_parts: Vec<serde_json::Value> = Vec::new();

        for m in messages {
            let role = match m.role {
                MessageRole::Assistant => "model",
                _ => "user", 
            };

            let text = if m.role == MessageRole::Tool {
                let content_str = m.content.to_string();
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content_str) {
                    if let (Some(name), Some(content)) = (
                        val.get("name").and_then(|n| n.as_str()),
                        val.get("content").and_then(|c| c.as_str()),
                    ) {
                        format!("Tool '{}' returned:\n{}", name, content)
                    } else {
                        content_str
                    }
                } else {
                    content_str
                }
            } else {
                m.content.to_string()
            };

            if role == current_role {
                current_parts.push(serde_json::json!({ "text": text }));
            } else {
                if !current_role.is_empty() {
                    contents.push(serde_json::json!({
                        "role": current_role,
                        "parts": current_parts
                    }));
                }
                current_role = role.to_string();
                current_parts = vec![serde_json::json!({ "text": text })];
            }
        }

        if !current_role.is_empty() {
            contents.push(serde_json::json!({
                "role": current_role,
                "parts": current_parts
            }));
        }

        (system_instruction, contents)
    }
}

// Reuse Gemini types from gemini.rs
#[derive(Debug, Deserialize)]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<GeminiUsage>,
}

#[derive(Debug, Deserialize)]
struct GeminiCandidate {
    #[serde(default)]
    content: Option<GeminiContent>,
    #[serde(rename = "finishReason")]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GeminiContent {
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Deserialize)]
struct GeminiPart {
    text: Option<String>,
    #[serde(rename = "functionCall")]
    function_call: Option<GeminiFunctionCall>,
}

#[derive(Debug, Deserialize)]
struct GeminiFunctionCall {
    name: String,
    args: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct GeminiUsage {
    #[serde(rename = "promptTokenCount")]
    prompt_token_count: Option<u64>,
    #[serde(rename = "candidatesTokenCount")]
    candidates_token_count: Option<u64>,
}


#[async_trait]
impl Provider for VertexProvider {
    fn name(&self) -> &str {
        "vertex"
    }

    fn models(&self) -> Vec<String> {
        vec![
            "gemini-2.0-flash".to_string(),
            "gemini-1.5-pro".to_string(),
        ]
    }

    async fn complete(
        &self,
        request: &ProviderRequest,
    ) -> Result<ProviderResponse, ProviderError> {
        let start = Instant::now();
        let model = if request.model.is_empty() {
            self.default_model.clone()
        } else {
            request.model.clone()
        };

        debug!(%model, messages = request.messages.len(), "calling Vertex API");

        let token = self.get_access_token().await?;

        let (system_instruction, contents) =
            Self::build_contents(&request.messages, &request.system_prompt);

        let mut body = serde_json::json!({
            "contents": contents,
        });

        if let Some(si) = system_instruction {
            body["systemInstruction"] = si;
        }
        if let Some(tools) = Self::translate_tools(&request.tools) {
            body["tools"] = tools;
        }

        let mut gen_config = serde_json::Map::new();
        if let Some(max_tokens) = request.max_tokens {
            gen_config.insert("maxOutputTokens".into(), serde_json::json!(max_tokens));
        }
        if let Some(temp) = request.temperature {
            gen_config.insert("temperature".into(), serde_json::json!(temp));
        }
        if !gen_config.is_empty() {
            body["generationConfig"] = serde_json::Value::Object(gen_config);
        }

        let url = self.model_url(&model, "generateContent");

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", token))
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::timeout("vertex", model.clone(), start.elapsed())
                } else {
                    ProviderError::network_error("vertex", e.to_string())
                }
            })?;

        let status = response.status();
        let latency = start.elapsed();

        if !status.is_success() {
            let status_code = status.as_u16();
            return match status_code {
                429 => Err(ProviderError::rate_limit("vertex", None)),
                401 | 403 => Err(ProviderError::auth_failure("vertex", "default")),
                _ => Err(ProviderError::server_error("vertex", status_code)),
            };
        }

        let api_response: GeminiResponse =
            response.json().await.map_err(|e| ProviderError::format_error("vertex", e.to_string()))?;

        let candidate = api_response
            .candidates
            .and_then(|mut c| if c.is_empty() { None } else { Some(c.remove(0)) })
            .ok_or_else(|| ProviderError::format_error("vertex", "no candidates in response"))?;

        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        if let Some(content) = &candidate.content {
            for part in &content.parts {
                if let Some(ref text) = part.text {
                    text_parts.push(text.clone());
                }
                if let Some(ref fc) = part.function_call {
                    tool_calls.push(ToolCall {
                        id: uuid::Uuid::new_v4().to_string(),
                        name: fc.name.clone(),
                        arguments: fc.args.clone(),
                    });
                }
            }
        }

        let mut finish_reason = match candidate.finish_reason.as_deref() {
            Some("STOP") => FinishReason::Stop,
            Some("MAX_TOKENS") => FinishReason::MaxTokens,
            Some("SAFETY") => FinishReason::ContentFilter,
            _ => FinishReason::Stop,
        };
        if !tool_calls.is_empty() {
            finish_reason = FinishReason::ToolUse;
        }

        let usage = api_response.usage_metadata.unwrap_or(GeminiUsage {
            prompt_token_count: None,
            candidates_token_count: None,
        });

        Ok(ProviderResponse {
            content: text_parts.join(""),
            model,
            provider: "vertex".to_string(),
            usage: TokenUsage {
                input_tokens: usage.prompt_token_count.unwrap_or(0),
                output_tokens: usage.candidates_token_count.unwrap_or(0),
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
            tool_calls,
            finish_reason,
            latency,
        })
    }

    async fn stream(
        &self,
        request: &ProviderRequest,
        chunk_tx: tokio::sync::mpsc::Sender<StreamChunk>,
    ) -> Result<(), ProviderError> {
        let model = if request.model.is_empty() {
            self.default_model.clone()
        } else {
            request.model.clone()
        };

        debug!(%model, "streaming Vertex API");

        let token = self.get_access_token().await?;

        let (system_instruction, contents) =
            Self::build_contents(&request.messages, &request.system_prompt);

        let mut body = serde_json::json!({ "contents": contents });
        if let Some(si) = system_instruction {
            body["systemInstruction"] = si;
        }

        let url = self.model_url(&model, "streamGenerateContent")
            + "&alt=sse";

        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", token))
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::network_error("vertex", e.to_string()))?;

        if !response.status().is_success() {
            return Err(ProviderError::server_error("vertex", response.status().as_u16()));
        }

        let mut buffer = String::new();
        let mut byte_buf: Vec<u8> = Vec::new();
        let mut response = response;
        while let Some(chunk) = response.chunk().await.map_err(|e| {
            ProviderError::network_error("vertex", e.to_string())
        })? {
            byte_buf.extend_from_slice(&chunk);
            let valid_len = match std::str::from_utf8(&byte_buf) {
                Ok(s) => s.len(),
                Err(e) => e.valid_up_to(),
            };
            if valid_len == 0 { continue; }
            let text = std::str::from_utf8(&byte_buf[..valid_len]).expect("valid UTF-8");
            buffer.push_str(text);
            byte_buf.drain(..valid_len);

            while let Some(boundary) = buffer.find("\n\n") {
                let event_block = buffer[..boundary].to_string();
                buffer = buffer[boundary + 2..].to_string();

                let data_line = event_block
                    .lines()
                    .find(|l| l.starts_with("data: "))
                    .map(|l| &l[6..]);

                let Some(data) = data_line else { continue };

                let Ok(chunk_json) = serde_json::from_str::<GeminiResponse>(data) else {
                    continue;
                };

                if let Some(candidates) = chunk_json.candidates {
                    for candidate in &candidates {
                        let is_done = candidate.finish_reason.is_some();
                        let usage = if is_done {
                            chunk_json.usage_metadata.as_ref().map(|u| TokenUsage {
                                input_tokens: u.prompt_token_count.unwrap_or(0),
                                output_tokens: u.candidates_token_count.unwrap_or(0),
                                cache_read_tokens: None,
                                cache_write_tokens: None,
                            })
                        } else {
                            None
                        };

                        let mut text_parts = String::new();
                        let mut tool_calls = Vec::new();

                        if let Some(ref content) = candidate.content {
                            for part in &content.parts {
                                if let Some(ref text) = part.text {
                                    text_parts.push_str(text);
                                }
                                if let Some(ref fc) = part.function_call {
                                    tool_calls.push(ToolCall {
                                        id: uuid::Uuid::new_v4().to_string(),
                                        name: fc.name.clone(),
                                        arguments: fc.args.clone(),
                                    });
                                }
                            }
                        }

                        let mut finish_reason = None;
                        if is_done {
                            finish_reason = match candidate.finish_reason.as_deref() {
                                Some("STOP") => Some(FinishReason::Stop),
                                Some("MAX_TOKENS") => Some(FinishReason::MaxTokens),
                                Some("SAFETY") => Some(FinishReason::ContentFilter),
                                _ => Some(FinishReason::Stop),
                            };
                            if !tool_calls.is_empty() {
                                finish_reason = Some(FinishReason::ToolUse);
                            }
                        }

                        if !text_parts.is_empty() || !tool_calls.is_empty() || is_done {
                            let _ = chunk_tx
                                .send(StreamChunk {
                                    delta: text_parts,
                                    reasoning_delta: String::new(),
                                    done: is_done,
                                    finish_reason,
                                    usage,
                                    tool_calls,
                                })
                                .await;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn health_check(&self) -> Result<(), ProviderError> {
        let token = self.get_access_token().await?;
        let _ = token;
        // Basic check: resolving API token means we authenticated
        Ok(())
    }
}
