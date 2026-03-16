//! OpenAI TTS provider — fast, affordable speech synthesis.
//!
//! API: POST /v1/audio/speech
//! Returns streaming audio in the requested format.

use crate::provider::{AudioFormat, TtsChunk, TtsError, TtsProvider, TtsRequest};
use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::warn;

/// Valid OpenAI TTS voices.
const OPENAI_VOICES: &[&str] = &["alloy", "echo", "fable", "onyx", "nova", "shimmer"];

pub struct OpenAiTtsProvider {
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl OpenAiTtsProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            base_url: "https://api.openai.com".into(),
            client: reqwest::Client::new(),
        }
    }

    pub fn with_base_url(mut self, url: &str) -> Self {
        self.base_url = url.trim_end_matches('/').to_string();
        self
    }
}

#[async_trait]
impl TtsProvider for OpenAiTtsProvider {
    fn name(&self) -> &str { "openai" }

    fn voices(&self) -> Vec<String> {
        OPENAI_VOICES.iter().map(|s| s.to_string()).collect()
    }

    async fn synthesize(&self, request: &TtsRequest, tx: mpsc::Sender<TtsChunk>) -> Result<(), TtsError> {
        if !OPENAI_VOICES.contains(&request.voice_id.as_str()) {
            return Err(TtsError::InvalidVoice(format!(
                "OpenAI voice must be one of: {}. Got: {}",
                OPENAI_VOICES.join(", "),
                request.voice_id
            )));
        }

        let url = format!("{}/v1/audio/speech", self.base_url);
        let speed = request.params.speed.unwrap_or(1.0).clamp(0.25, 4.0);
        let response_format = match request.format {
            AudioFormat::Mp3 => "mp3",
            AudioFormat::Opus => "opus",
            AudioFormat::Wav => "wav",
            AudioFormat::Pcm16 => "pcm",
        };

        let body = serde_json::json!({
            "model": "tts-1",
            "input": request.text,
            "voice": request.voice_id,
            "speed": speed,
            "response_format": response_format,
        });

        let resp = self.client
            .post(&url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| TtsError::SynthesisFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(TtsError::SynthesisFailed(format!("OpenAI TTS {status}: {text}")));
        }

        let mut seq = 0u64;
        let mut stream = resp.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(bytes) => {
                    if tx.send(TtsChunk {
                        data: bytes.to_vec(),
                        format: request.format,
                        sample_rate: 24000,
                        sequence: seq,
                        is_final: false,
                    }).await.is_err() {
                        break;
                    }
                    seq += 1;
                }
                Err(e) => { warn!(error = %e, "OpenAI TTS stream error"); break; }
            }
        }

        let _ = tx.send(TtsChunk { data: vec![], format: request.format, sample_rate: 24000, sequence: seq, is_final: true }).await;
        Ok(())
    }

    fn is_available(&self) -> bool { !self.api_key.is_empty() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_voices() {
        let provider = OpenAiTtsProvider::new("key".into());
        let voices = provider.voices();
        assert!(voices.contains(&"alloy".to_string()));
        assert!(voices.contains(&"nova".to_string()));
    }
}
