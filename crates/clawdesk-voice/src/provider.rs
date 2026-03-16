//! TTS provider trait and provider-specific types.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Audio format for TTS output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioFormat {
    Mp3,
    Opus,
    Wav,
    Pcm16,
}

/// A chunk of audio data from TTS streaming.
#[derive(Debug, Clone)]
pub struct TtsChunk {
    pub data: Vec<u8>,
    pub format: AudioFormat,
    pub sample_rate: u32,
    pub sequence: u64,
    pub is_final: bool,
}

/// Request for TTS synthesis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsRequest {
    pub text: String,
    pub voice_id: String,
    pub format: AudioFormat,
    pub provider: String,
    pub params: TtsParams,
}

/// Provider-agnostic TTS parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsParams {
    pub speed: Option<f32>,
    pub stability: Option<f32>,
    pub similarity_boost: Option<f32>,
    pub style: Option<f32>,
    pub seed: Option<u64>,
}

impl Default for TtsParams {
    fn default() -> Self {
        Self {
            speed: Some(1.0),
            stability: None,
            similarity_boost: None,
            style: None,
            seed: None,
        }
    }
}

/// Provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsProviderConfig {
    pub name: String,
    pub api_key: Option<String>,
    pub base_url: String,
    pub default_voice: String,
    pub default_format: AudioFormat,
}

/// Trait for TTS providers.
#[async_trait]
pub trait TtsProvider: Send + Sync {
    /// Provider name.
    fn name(&self) -> &str;

    /// Available voices.
    fn voices(&self) -> Vec<String>;

    /// Synthesize speech, streaming chunks through the sender.
    async fn synthesize(
        &self,
        request: &TtsRequest,
        tx: mpsc::Sender<TtsChunk>,
    ) -> Result<(), TtsError>;

    /// Whether this provider is available (has valid credentials).
    fn is_available(&self) -> bool;
}

/// TTS error types.
#[derive(Debug, thiserror::Error)]
pub enum TtsError {
    #[error("provider not available: {0}")]
    Unavailable(String),
    #[error("invalid voice ID: {0}")]
    InvalidVoice(String),
    #[error("synthesis failed: {0}")]
    SynthesisFailed(String),
    #[error("streaming error: {0}")]
    StreamError(String),
    #[error("parameter out of range: {field} = {value} (expected {min}..={max})")]
    ParameterOutOfRange {
        field: String,
        value: f32,
        min: f32,
        max: f32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_params() {
        let p = TtsParams::default();
        assert_eq!(p.speed, Some(1.0));
    }

    #[test]
    fn audio_format_serialize() {
        let f = AudioFormat::Mp3;
        let s = serde_json::to_string(&f).unwrap();
        assert_eq!(s, "\"mp3\"");
    }
}
