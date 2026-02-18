//! Media processing pipeline with concurrency control and adaptive selection.

use crate::processor::{MediaProcessor, ProcessorResult};
use crate::selector::AdaptiveSelector;
use clawdesk_types::media::{MediaInput, MediaType};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};
use tracing::{debug, warn};

/// Media processing pipeline.
pub struct MediaPipeline {
    processors: Vec<Arc<dyn MediaProcessor>>,
    selector: Arc<Mutex<AdaptiveSelector>>,
    /// Per-type concurrency semaphores.
    semaphores: HashMap<MediaType, Arc<Semaphore>>,
}

impl MediaPipeline {
    pub fn new() -> Self {
        let mut semaphores = HashMap::new();
        semaphores.insert(MediaType::Audio, Arc::new(Semaphore::new(4)));
        semaphores.insert(MediaType::Video, Arc::new(Semaphore::new(2)));
        semaphores.insert(MediaType::Image, Arc::new(Semaphore::new(8)));
        semaphores.insert(MediaType::Document, Arc::new(Semaphore::new(4)));

        Self {
            processors: Vec::new(),
            selector: Arc::new(Mutex::new(AdaptiveSelector::new())),
            semaphores,
        }
    }

    /// Register a media processor.
    pub async fn register(&mut self, processor: Arc<dyn MediaProcessor>) {
        let name = processor.name().to_string();
        self.selector.lock().await.register(&name);
        self.processors.push(processor);
    }

    /// Process media input using the best available processor.
    pub async fn process(&self, input: &MediaInput) -> ProcessorResult {
        // Acquire concurrency slot.
        let sem = self
            .semaphores
            .get(&input.media_type)
            .ok_or_else(|| format!("no concurrency slot for {:?}", input.media_type))?;
        let _permit = sem
            .acquire()
            .await
            .map_err(|e| format!("semaphore: {e}"))?;

        // Find processors that support this media type.
        let candidates: Vec<_> = self
            .processors
            .iter()
            .filter(|p| p.supported_types().contains(&input.media_type))
            .collect();

        if candidates.is_empty() {
            return Err(format!(
                "no processor available for {:?}",
                input.media_type
            ));
        }

        // Use adaptive selection to pick the best processor.
        let selected_name = {
            let sel = self.selector.lock().await;
            sel.select()
        };

        // Find the selected processor, or fall back to first candidate.
        let processor = if let Some(ref name) = selected_name {
            candidates
                .iter()
                .find(|p| p.name() == name)
                .unwrap_or(&candidates[0])
        } else {
            &candidates[0]
        };

        // Check availability.
        if !processor.is_available().await {
            warn!(processor = %processor.name(), "processor not available, trying next");
            // Try the next available one.
            for p in &candidates {
                if p.is_available().await {
                    let start = std::time::Instant::now();
                    match p.process(input).await {
                        Ok(result) => {
                            let ms = start.elapsed().as_millis() as u64;
                            self.selector
                                .lock()
                                .await
                                .record_success(p.name(), ms);
                            return Ok(result);
                        }
                        Err(e) => {
                            let ms = start.elapsed().as_millis() as u64;
                            self.selector
                                .lock()
                                .await
                                .record_failure(p.name(), ms);
                            return Err(e);
                        }
                    }
                }
            }
            return Err("all processors unavailable".to_string());
        }

        // Process with the selected processor.
        let start = std::time::Instant::now();
        let proc_name = processor.name().to_string();
        match processor.process(input).await {
            Ok(result) => {
                let ms = start.elapsed().as_millis() as u64;
                self.selector
                    .lock()
                    .await
                    .record_success(&proc_name, ms);
                debug!(processor = %proc_name, ms, "media processing succeeded");
                Ok(result)
            }
            Err(e) => {
                let ms = start.elapsed().as_millis() as u64;
                self.selector
                    .lock()
                    .await
                    .record_failure(&proc_name, ms);
                warn!(processor = %proc_name, error = %e, "media processing failed");
                Err(e)
            }
        }
    }

    /// Get all registered processors.
    pub fn processors(&self) -> &[Arc<dyn MediaProcessor>] {
        &self.processors
    }

    /// Get the selector stats.
    pub async fn provider_stats(
        &self,
    ) -> HashMap<String, crate::selector::ProviderStats> {
        self.selector.lock().await.all_stats().clone()
    }
}

impl Default for MediaPipeline {
    fn default() -> Self {
        Self::new()
    }
}
