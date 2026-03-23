//! Voyage AI embedding provider with batch support and content-type routing.
//!
//! ## Models (current as of 2026)
//!
//! - `voyage-4-large`: Highest quality (1024-d default, supports 256-2048)
//! - `voyage-4`: Balanced quality/cost (1024-d default)
//! - `voyage-4-lite`: Fastest, lowest cost (1024-d default)
//! - `voyage-3.5`: Previous gen balanced (1024-d)
//! - `voyage-code-3`: Code-optimized, better recall@10 for code (1024-d)
//!
//! ## Batch Optimization
//!
//! Batch API supports up to 1,000 inputs per request.
//! Token limit: 320K for voyage-4/3.5/2; 120K for voyage-4-large/3-large/code-3.

use async_trait::async_trait;
use clawdesk_types::error::MemoryError;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{debug, warn};

use crate::embedding::{BatchEmbeddingResult, EmbeddingProvider, EmbeddingResult};

const VOYAGE_API_URL: &str = "https://api.voyageai.com/v1/embeddings";
const MAX_BATCH_SIZE: usize = 1000;

/// Voyage AI embedding provider.
pub struct VoyageEmbeddingProvider {
    client: Client,
    api_key: String,
    /// Model for general text.
    text_model: String,
    /// Model for code content.
    code_model: String,
    /// Whether to auto-detect content type and route to code model.
    auto_route: bool,
}

impl VoyageEmbeddingProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("failed to build HTTP client"),
            api_key,
            text_model: "voyage-4".into(),
            code_model: "voyage-code-3".into(),
            auto_route: true,
        }
    }

    pub fn with_models(mut self, text: &str, code: &str) -> Self {
        self.text_model = text.to_string();
        self.code_model = code.to_string();
        self
    }

    pub fn with_auto_route(mut self, enabled: bool) -> Self {
        self.auto_route = enabled;
        self
    }

    /// Detect if content is code (simple heuristic).
    fn is_code(text: &str) -> bool {
        let code_indicators = [
            "fn ", "def ", "class ", "import ", "const ", "let ", "var ",
            "func ", "pub ", "async ", "return ", "->", "=>", "();",
            "#!/", "#include", "package ", "module ",
        ];
        let indicator_count = code_indicators
            .iter()
            .filter(|&&pat| text.contains(pat))
            .count();
        // If >= 2 code keywords found, likely code
        indicator_count >= 2
    }

    /// Select model based on content type.
    fn select_model(&self, text: &str) -> &str {
        if self.auto_route && Self::is_code(text) {
            &self.code_model
        } else {
            &self.text_model
        }
    }

    /// Dimensions for the selected model (default output dimension per docs).
    fn model_dimensions(model: &str) -> usize {
        match model {
            // All current models default to 1024-d
            // voyage-4-large/4/4-lite/3.5/3.5-lite/3-large/code-3 all support
            // MRL dimensions: 2048, 1024 (default), 512, 256
            "voyage-4-large" | "voyage-4" | "voyage-4-lite" => 1024,
            "voyage-3.5" | "voyage-3.5-lite" => 1024,
            "voyage-3-large" | "voyage-3" | "voyage-code-3" => 1024,
            "voyage-finance-2" | "voyage-law-2" => 1024,
            _ => 1024,
        }
    }

    async fn embed_with_model(
        &self,
        texts: &[String],
        model: &str,
        input_type: Option<&str>,
    ) -> Result<Vec<EmbeddingResult>, MemoryError> {
        let body = VoyageRequest {
            model: model.to_string(),
            input: texts.to_vec(),
            input_type: input_type.map(|s| s.to_string()),
        };

        let response = self
            .client
            .post(VOYAGE_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&body)
            .send()
            .await
            .map_err(|e| MemoryError::EmbeddingFailed { detail: format!("Voyage API error: {e}") })?;

        if !response.status().is_success() {
            let status = response.status();
            let err = response.text().await.unwrap_or_default();
            return Err(MemoryError::EmbeddingFailed { detail: format!(
                "Voyage API {status}: {err}"
            ) });
        }

        let resp: VoyageResponse = response.json().await.map_err(|e| {
            MemoryError::EmbeddingFailed { detail: format!("Voyage response parse error: {e}") }
        })?;

        let total_tokens = resp.usage.as_ref().map(|u| u.total_tokens).unwrap_or(0);
        let per_item_tokens = if texts.is_empty() {
            0
        } else {
            total_tokens / texts.len() as u32
        };

        let dims = Self::model_dimensions(model);
        let results = resp
            .data
            .into_iter()
            .map(|d| EmbeddingResult {
                vector: d.embedding,
                dimensions: dims,
                model: model.to_string(),
                tokens_used: per_item_tokens,
            })
            .collect();

        Ok(results)
    }
}

#[async_trait]
impl EmbeddingProvider for VoyageEmbeddingProvider {
    fn name(&self) -> &str {
        "voyage"
    }

    fn dimensions(&self) -> usize {
        Self::model_dimensions(&self.text_model)
    }

    fn max_tokens(&self) -> usize {
        320_000 // Voyage batch token limit
    }

    async fn embed(&self, text: &str) -> Result<EmbeddingResult, MemoryError> {
        let model = self.select_model(text);
        // Use None for input_type — caller doesn't specify query vs document
        let results = self
            .embed_with_model(&[text.to_string()], model, None)
            .await?;
        results
            .into_iter()
            .next()
            .ok_or_else(|| MemoryError::EmbeddingFailed { detail: "No embedding returned".into() })
    }

    async fn embed_batch(&self, texts: &[String]) -> Result<BatchEmbeddingResult, MemoryError> {
        if texts.is_empty() {
            return Ok(BatchEmbeddingResult {
                embeddings: Vec::new(),
                total_tokens: 0,
            });
        }

        // Split into batches of MAX_BATCH_SIZE
        let mut all_results = Vec::with_capacity(texts.len());
        let mut total_tokens = 0u32;

        for chunk in texts.chunks(MAX_BATCH_SIZE) {
            // Use "document" input_type for batch ingestion per Voyage docs
            let results = self
                .embed_with_model(&chunk.to_vec(), &self.text_model, Some("document"))
                .await?;
            for r in &results {
                total_tokens += r.tokens_used;
            }
            all_results.extend(results);
        }

        Ok(BatchEmbeddingResult {
            embeddings: all_results,
            total_tokens,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Voyage API Types
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct VoyageRequest {
    model: String,
    input: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_type: Option<String>,
}

#[derive(Deserialize)]
struct VoyageResponse {
    data: Vec<VoyageEmbedding>,
    #[serde(default)]
    usage: Option<VoyageUsage>,
}

#[derive(Deserialize)]
struct VoyageEmbedding {
    embedding: Vec<f32>,
    #[serde(default)]
    index: usize,
}

#[derive(Deserialize)]
struct VoyageUsage {
    total_tokens: u32,
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_detection() {
        assert!(VoyageEmbeddingProvider::is_code(
            "fn main() { let x = 5; return x; }"
        ));
        assert!(!VoyageEmbeddingProvider::is_code(
            "The quick brown fox jumps over the lazy dog."
        ));
        assert!(VoyageEmbeddingProvider::is_code(
            "import os\ndef hello():\n    return 'world'"
        ));
    }

    #[test]
    fn model_selection() {
        let p = VoyageEmbeddingProvider::new("key".into());
        assert_eq!(
            p.select_model("fn main() { let x = 5; return x; }"),
            "voyage-code-3"
        );
        assert_eq!(
            p.select_model("Hello world, this is a test document."),
            "voyage-4"
        );
    }

    #[test]
    fn dimensions() {
        assert_eq!(VoyageEmbeddingProvider::model_dimensions("voyage-4"), 1024);
        assert_eq!(
            VoyageEmbeddingProvider::model_dimensions("voyage-4-lite"),
            1024
        );
    }

    #[test]
    fn provider_name() {
        let p = VoyageEmbeddingProvider::new("key".into());
        assert_eq!(p.name(), "voyage");
    }
}
