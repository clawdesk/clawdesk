//! Google Gemini provider adapter.
//!
//! Uses the `generativelanguage.googleapis.com/v1beta` REST API.
//!
//! ## Schema translation
//!
//! Gemini uses `FunctionDeclaration` with OpenAPI Schema parameters,
//! which differs from OpenAI/Anthropic's JSON Schema format. This adapter
//! performs automatic translation at the boundary, mapping ClawDesk's
//! `ToolDefinition` (JSON Schema) to Gemini's OpenAPI subset.
//!
//! Unsupported JSON Schema features (`$ref`, `oneOf`, `allOf`) emit
//! warnings and are stripped from the translated schema.

use async_trait::async_trait;
use clawdesk_types::error::ProviderError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tracing::{debug, warn};

use crate::{
    ChatMessage, FinishReason, MessageRole, Provider, ProviderRequest, ProviderResponse,
    StreamChunk, ToolCall, ToolDefinition, TokenUsage,
};

const GEMINI_API_URL: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Google Gemini provider.
pub struct GeminiProvider {
    client: Client,
    api_key: String,
    default_model: String,
}

impl GeminiProvider {
    pub fn new(api_key: String, default_model: Option<String>) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("failed to build HTTP client"),
            api_key,
            default_model: default_model
                .unwrap_or_else(|| "gemini-2.0-flash".to_string()),
        }
    }

    fn model_url(&self, model: &str, method: &str) -> String {
        format!(
            "{}/models/{}:{}?key={}",
            GEMINI_API_URL, model, method, self.api_key
        )
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

    /// Convert JSON Schema to Gemini's OpenAPI Schema subset.
    /// Strips unsupported features ($ref, oneOf, allOf, anyOf) with warnings.
    fn json_schema_to_openapi(schema: &serde_json::Value) -> serde_json::Value {
        let mut result = serde_json::Map::new();

        if let Some(obj) = schema.as_object() {
            // Warn about unsupported constructs.
            for key in &["$ref", "oneOf", "allOf", "anyOf"] {
                if obj.contains_key(*key) {
                    warn!(key, "gemini: stripping unsupported JSON Schema construct");
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

    /// Build Gemini message contents from ClawDesk messages.
    fn build_contents(
        messages: &[ChatMessage],
        system_prompt: &Option<String>,
    ) -> (Option<serde_json::Value>, Vec<serde_json::Value>) {
        let system_instruction = system_prompt.as_ref().map(|s| {
            serde_json::json!({
                "parts": [{ "text": s }]
            })
        });

        let contents: Vec<serde_json::Value> = messages
            .iter()
            .map(|m| {
                let role = match m.role {
                    MessageRole::User => "user",
                    MessageRole::Assistant => "model",
                    MessageRole::System => "user", // Gemini handles system separately.
                    MessageRole::Tool => "function",
                };
                serde_json::json!({
                    "role": role,
                    "parts": [{ "text": m.content.to_string() }]
                })
            })
            .collect();

        (system_instruction, contents)
    }
}

// ---------------------------------------------------------------------------
// Gemini API types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    #[serde(rename = "usageMetadata")]
    usage_metadata: Option<GeminiUsage>,
}

#[derive(Debug, Deserialize)]
struct GeminiCandidate {
    content: GeminiContent,
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
impl Provider for GeminiProvider {
    fn name(&self) -> &str {
        "gemini"
    }

    fn models(&self) -> Vec<String> {
        vec![
            "gemini-2.0-flash".to_string(),
            "gemini-2.5-pro".to_string(),
            "gemini-2.5-flash".to_string(),
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

        debug!(%model, messages = request.messages.len(), "calling Gemini API");

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
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    ProviderError::Timeout {
                        provider: "gemini".into(),
                        model: model.clone(),
                        after: start.elapsed(),
                    }
                } else {
                    ProviderError::NetworkError {
                        provider: "gemini".into(),
                        detail: e.to_string(),
                    }
                }
            })?;

        let status = response.status();
        let latency = start.elapsed();

        if !status.is_success() {
            let status_code = status.as_u16();
            return match status_code {
                429 => Err(ProviderError::RateLimit {
                    provider: "gemini".into(),
                    retry_after: None,
                }),
                401 | 403 => Err(ProviderError::AuthFailure {
                    provider: "gemini".into(),
                    profile_id: "default".into(),
                }),
                _ => Err(ProviderError::ServerError {
                    provider: "gemini".into(),
                    status: status_code,
                }),
            };
        }

        let api_response: GeminiResponse =
            response.json().await.map_err(|e| ProviderError::FormatError {
                provider: "gemini".into(),
                detail: e.to_string(),
            })?;

        let candidate = api_response
            .candidates
            .and_then(|mut c| if c.is_empty() { None } else { Some(c.remove(0)) })
            .ok_or_else(|| ProviderError::FormatError {
                provider: "gemini".into(),
                detail: "no candidates in response".into(),
            })?;

        let mut text_parts = Vec::new();
        let mut tool_calls = Vec::new();

        for part in &candidate.content.parts {
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

        let finish_reason = match candidate.finish_reason.as_deref() {
            Some("STOP") => FinishReason::Stop,
            Some("MAX_TOKENS") => FinishReason::MaxTokens,
            Some("SAFETY") => FinishReason::ContentFilter,
            _ => FinishReason::Stop,
        };

        let usage = api_response.usage_metadata.unwrap_or(GeminiUsage {
            prompt_token_count: None,
            candidates_token_count: None,
        });

        Ok(ProviderResponse {
            content: text_parts.join(""),
            model,
            provider: "gemini".to_string(),
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

    /// Streaming via Gemini's `streamGenerateContent` endpoint.
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

        debug!(%model, "streaming Gemini API");

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
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError {
                provider: "gemini".into(),
                detail: e.to_string(),
            })?;

        if !response.status().is_success() {
            return Err(ProviderError::ServerError {
                provider: "gemini".into(),
                status: response.status().as_u16(),
            });
        }

        let mut buffer = String::new();
        let mut response = response;
        while let Some(chunk) = response.chunk().await.map_err(|e| {
            ProviderError::NetworkError {
                provider: "gemini".into(),
                detail: e.to_string(),
            }
        })? {
            buffer.push_str(&String::from_utf8_lossy(&chunk));

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
                        for part in &candidate.content.parts {
                            if let Some(ref text) = part.text {
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

                                let _ = chunk_tx
                                    .send(StreamChunk {
                                        delta: text.clone(),
                                        done: is_done,
                                        finish_reason: if is_done {
                                            Some(FinishReason::Stop)
                                        } else {
                                            None
                                        },
                                        usage,
                                    })
                                    .await;
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }

    async fn health_check(&self) -> Result<(), ProviderError> {
        // List models to verify API key and connectivity.
        let url = format!(
            "{}/models?key={}",
            GEMINI_API_URL, self.api_key
        );
        self.client
            .get(&url)
            .send()
            .await
            .map_err(|e| ProviderError::NetworkError {
                provider: "gemini".into(),
                detail: e.to_string(),
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gemini_default_construction() {
        let p = GeminiProvider::new("test-key".into(), None);
        assert_eq!(p.name(), "gemini");
        assert!(p.models().contains(&"gemini-2.0-flash".to_string()));
    }

    #[test]
    fn json_schema_translation_strips_ref() {
        let schema = serde_json::json!({
            "type": "object",
            "$ref": "#/definitions/Foo",
            "properties": {
                "name": { "type": "string", "description": "User name" }
            },
            "required": ["name"]
        });
        let translated = GeminiProvider::json_schema_to_openapi(&schema);
        assert!(translated.get("$ref").is_none());
        assert!(translated.get("type").is_some());
        assert!(translated.get("properties").is_some());
    }

    #[test]
    fn tool_translation_produces_declarations() {
        let tools = vec![ToolDefinition {
            name: "search".into(),
            description: "Search the web".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" }
                }
            }),
        }];
        let result = GeminiProvider::translate_tools(&tools);
        assert!(result.is_some());
        let arr = result.unwrap();
        assert!(arr.as_array().unwrap()[0].get("functionDeclarations").is_some());
    }
}
