//! TTS engine — multi-provider orchestration with compile-time parameter safety.

use crate::provider::{AudioFormat, TtsChunk, TtsError, TtsParams, TtsProvider, TtsRequest};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, warn};

// ───────────────────────────────────────────────────────────────────────────
// Compile-time safe parameter types
// ───────────────────────────────────────────────────────────────────────────

/// ElevenLabs stability parameter — guaranteed ∈ [0.0, 1.0].
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Stability(f32);

impl Stability {
    pub fn new(value: f32) -> Result<Self, TtsError> {
        if !(0.0..=1.0).contains(&value) {
            return Err(TtsError::ParameterOutOfRange {
                field: "stability".into(), value, min: 0.0, max: 1.0,
            });
        }
        Ok(Self(value))
    }
    pub fn value(self) -> f32 { self.0 }
}

impl Default for Stability {
    fn default() -> Self { Self(0.5) }
}

/// Speed parameter — guaranteed ∈ [0.5, 2.0].
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Speed(f32);

impl Speed {
    pub fn new(value: f32) -> Result<Self, TtsError> {
        if !(0.5..=2.0).contains(&value) {
            return Err(TtsError::ParameterOutOfRange {
                field: "speed".into(), value, min: 0.5, max: 2.0,
            });
        }
        Ok(Self(value))
    }
    pub fn value(self) -> f32 { self.0 }
}

impl Default for Speed {
    fn default() -> Self { Self(1.0) }
}

// ───────────────────────────────────────────────────────────────────────────
// Engine
// ───────────────────────────────────────────────────────────────────────────

/// TTS engine configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsEngineConfig {
    /// Provider priority for selection: (quality_weight, latency_weight, cost_weight).
    pub selection_weights: (f32, f32, f32),
    /// Default audio format.
    pub default_format: AudioFormat,
    /// Chunk size in samples for streaming (200ms at 24kHz = 4800).
    pub chunk_samples: usize,
    /// Temporary audio file cleanup delay in seconds.
    pub cleanup_delay_secs: u64,
}

impl Default for TtsEngineConfig {
    fn default() -> Self {
        Self {
            selection_weights: (0.5, 0.3, 0.2),
            default_format: AudioFormat::Mp3,
            chunk_samples: 4800,
            cleanup_delay_secs: 300,
        }
    }
}

/// Provider quality/latency/cost profile for selection.
struct ProviderProfile {
    provider: Arc<dyn TtsProvider>,
    quality: f32,
    latency: f32,
    cost: f32,
}

/// Multi-provider TTS engine.
pub struct TtsEngine {
    providers: Vec<ProviderProfile>,
    config: TtsEngineConfig,
}

impl TtsEngine {
    pub fn new(config: TtsEngineConfig) -> Self {
        Self { providers: Vec::new(), config }
    }

    /// Register a provider with its quality/latency/cost profile.
    pub fn register(
        &mut self,
        provider: Arc<dyn TtsProvider>,
        quality: f32,
        latency: f32,
        cost: f32,
    ) {
        self.providers.push(ProviderProfile { provider, quality, latency, cost });
    }

    /// Select the best available provider.
    ///
    /// Score = α·Q(p) - β·L(p) - γ·C(p) where (α, β, γ) = selection_weights.
    pub fn select_provider(&self) -> Option<Arc<dyn TtsProvider>> {
        let (alpha, beta, gamma) = self.config.selection_weights;
        self.providers
            .iter()
            .filter(|p| p.provider.is_available())
            .max_by(|a, b| {
                let score_a = alpha * a.quality - beta * a.latency - gamma * a.cost;
                let score_b = alpha * b.quality - beta * b.latency - gamma * b.cost;
                score_a.partial_cmp(&score_b).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|p| Arc::clone(&p.provider))
    }

    /// Synthesize speech using the best available provider.
    pub async fn synthesize(
        &self,
        text: &str,
        voice: &str,
    ) -> Result<mpsc::Receiver<TtsChunk>, TtsError> {
        let provider = self.select_provider()
            .ok_or_else(|| TtsError::Unavailable("no TTS provider available".into()))?;

        let (tx, rx) = mpsc::channel(32);
        let request = TtsRequest {
            text: text.to_string(),
            voice_id: voice.to_string(),
            format: self.config.default_format,
            provider: provider.name().to_string(),
            params: TtsParams::default(),
        };

        let provider_clone = Arc::clone(&provider);
        tokio::spawn(async move {
            if let Err(e) = provider_clone.synthesize(&request, tx).await {
                warn!(error = %e, "TTS synthesis failed");
            }
        });

        Ok(rx)
    }

    /// Schedule cleanup of a temporary audio file after delay.
    pub fn schedule_cleanup(&self, path: std::path::PathBuf) {
        let delay = std::time::Duration::from_secs(self.config.cleanup_delay_secs);
        tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            if let Err(e) = tokio::fs::remove_file(&path).await {
                debug!(path = %path.display(), error = %e, "temp audio cleanup failed");
            }
        });
    }

    /// Number of registered providers.
    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stability_valid_range() {
        assert!(Stability::new(0.5).is_ok());
        assert!(Stability::new(-0.1).is_err());
        assert!(Stability::new(1.1).is_err());
    }

    #[test]
    fn speed_valid_range() {
        assert!(Speed::new(1.0).is_ok());
        assert!(Speed::new(0.4).is_err());
        assert!(Speed::new(2.1).is_err());
    }

    #[test]
    fn engine_no_providers() {
        let engine = TtsEngine::new(TtsEngineConfig::default());
        assert!(engine.select_provider().is_none());
    }
}
