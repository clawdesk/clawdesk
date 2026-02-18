//! Batch pipeline — AIMD-style adaptive batching for embedding requests.

use crate::embedding::{BatchEmbeddingResult, EmbeddingProvider};
use clawdesk_types::error::MemoryError;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, warn};

/// AIMD (Additive Increase, Multiplicative Decrease) batch size controller.
///
/// Starts with a small batch size and increases it on success,
/// decreases on failure — similar to TCP congestion control.
struct AimdController {
    current_batch_size: usize,
    min_batch_size: usize,
    max_batch_size: usize,
    /// Additive increase step.
    increase_step: usize,
    /// Multiplicative decrease factor (e.g., 0.5 = halve on failure).
    decrease_factor: f64,
    /// Consecutive successes.
    success_count: u32,
    /// Successes needed before increasing.
    increase_threshold: u32,
}

impl AimdController {
    fn new(initial: usize, min: usize, max: usize) -> Self {
        Self {
            current_batch_size: initial,
            min_batch_size: min,
            max_batch_size: max,
            increase_step: 4,
            decrease_factor: 0.5,
            success_count: 0,
            increase_threshold: 3,
        }
    }

    fn on_success(&mut self) {
        self.success_count += 1;
        if self.success_count >= self.increase_threshold {
            self.current_batch_size =
                (self.current_batch_size + self.increase_step).min(self.max_batch_size);
            self.success_count = 0;
            debug!(new_batch_size = self.current_batch_size, "AIMD: batch size increased");
        }
    }

    fn on_failure(&mut self) {
        self.current_batch_size = ((self.current_batch_size as f64 * self.decrease_factor) as usize)
            .max(self.min_batch_size);
        self.success_count = 0;
        warn!(new_batch_size = self.current_batch_size, "AIMD: batch size decreased");
    }

    fn batch_size(&self) -> usize {
        self.current_batch_size
    }
}

/// Batch pipeline for embedding large sets of text with adaptive batching.
///
/// Uses AIMD to dynamically adjust batch size based on provider response patterns.
/// This prevents overwhelming providers while maximizing throughput.
pub struct BatchPipeline {
    provider: Arc<dyn EmbeddingProvider>,
    controller: Mutex<AimdController>,
    /// Maximum retries per batch.
    max_retries: u32,
}

impl BatchPipeline {
    pub fn new(provider: Arc<dyn EmbeddingProvider>) -> Self {
        Self {
            provider,
            controller: Mutex::new(AimdController::new(16, 1, 256)),
            max_retries: 3,
        }
    }

    /// Configure the pipeline's batch size parameters.
    pub fn with_batch_config(
        provider: Arc<dyn EmbeddingProvider>,
        initial: usize,
        min: usize,
        max: usize,
    ) -> Self {
        Self {
            provider,
            controller: Mutex::new(AimdController::new(initial, min, max)),
            max_retries: 3,
        }
    }

    /// Embed all texts using adaptive batching.
    ///
    /// Returns embeddings in the same order as the input texts.
    pub async fn embed_all(
        &self,
        texts: &[String],
    ) -> Result<BatchEmbeddingResult, MemoryError> {
        if texts.is_empty() {
            return Ok(BatchEmbeddingResult {
                embeddings: vec![],
                total_tokens: 0,
            });
        }

        let mut all_embeddings = Vec::with_capacity(texts.len());
        let mut total_tokens = 0u32;
        let mut offset = 0;

        while offset < texts.len() {
            let batch_size = {
                let ctrl = self.controller.lock().await;
                ctrl.batch_size().min(texts.len() - offset)
            };

            let batch = &texts[offset..offset + batch_size];

            match self.embed_batch_with_retry(batch).await {
                Ok(result) => {
                    total_tokens += result.total_tokens;
                    all_embeddings.extend(result.embeddings);
                    offset += batch_size;

                    let mut ctrl = self.controller.lock().await;
                    ctrl.on_success();
                }
                Err(e) => {
                    let mut ctrl = self.controller.lock().await;
                    ctrl.on_failure();

                    // If batch size is already at minimum, propagate error.
                    if ctrl.batch_size() <= ctrl.min_batch_size && batch_size <= 1 {
                        return Err(e);
                    }
                    // Otherwise, retry with smaller batch (loop continues).
                    debug!(error = %e, "Retrying with smaller batch size");
                }
            }
        }

        Ok(BatchEmbeddingResult {
            embeddings: all_embeddings,
            total_tokens,
        })
    }

    /// Embed a single batch with exponential backoff retry.
    async fn embed_batch_with_retry(
        &self,
        texts: &[String],
    ) -> Result<BatchEmbeddingResult, MemoryError> {
        let texts_owned: Vec<String> = texts.to_vec();
        let mut last_error = None;
        let mut delay = tokio::time::Duration::from_millis(100);

        for attempt in 0..self.max_retries {
            match self.provider.embed_batch(&texts_owned).await {
                Ok(result) => return Ok(result),
                Err(e) => {
                    warn!(
                        attempt = attempt + 1,
                        max = self.max_retries,
                        error = %e,
                        "Embedding batch failed, retrying"
                    );
                    last_error = Some(e);
                    tokio::time::sleep(delay).await;
                    delay *= 2;
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            MemoryError::EmbeddingFailed { detail: "All retries exhausted".to_string() }
        }))
    }

    /// Get current batch size (for monitoring).
    pub async fn current_batch_size(&self) -> usize {
        self.controller.lock().await.batch_size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::MockEmbeddingProvider;

    #[tokio::test]
    async fn test_embed_all_empty() {
        let provider = Arc::new(MockEmbeddingProvider::new(32));
        let pipeline = BatchPipeline::new(provider);
        let result = pipeline.embed_all(&[]).await.unwrap();
        assert!(result.embeddings.is_empty());
        assert_eq!(result.total_tokens, 0);
    }

    #[tokio::test]
    async fn test_embed_all_small() {
        let provider = Arc::new(MockEmbeddingProvider::new(32));
        let pipeline = BatchPipeline::new(provider);
        let texts: Vec<String> = (0..5).map(|i| format!("text {}", i)).collect();
        let result = pipeline.embed_all(&texts).await.unwrap();
        assert_eq!(result.embeddings.len(), 5);
    }

    #[tokio::test]
    async fn test_embed_all_large() {
        let provider = Arc::new(MockEmbeddingProvider::new(16));
        let pipeline = BatchPipeline::with_batch_config(
            provider,
            4,  // initial batch of 4
            1,  // min 1
            32, // max 32
        );
        let texts: Vec<String> = (0..50).map(|i| format!("document {}", i)).collect();
        let result = pipeline.embed_all(&texts).await.unwrap();
        assert_eq!(result.embeddings.len(), 50);
    }

    #[tokio::test]
    async fn test_aimd_increase() {
        let mut ctrl = AimdController::new(8, 1, 64);
        let initial = ctrl.batch_size();
        for _ in 0..ctrl.increase_threshold {
            ctrl.on_success();
        }
        assert!(ctrl.batch_size() > initial);
    }

    #[tokio::test]
    async fn test_aimd_decrease() {
        let mut ctrl = AimdController::new(16, 1, 64);
        ctrl.on_failure();
        assert_eq!(ctrl.batch_size(), 8);
    }
}
