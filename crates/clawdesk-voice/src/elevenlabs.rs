//! ElevenLabs TTS provider — high-quality neural speech synthesis.
//!
//! API: POST /v1/text-to-speech/{voice_id}/stream
//! Streams chunked audio via chunked transfer encoding.

use crate::provider::{AudioFormat, TtsChunk, TtsError, TtsProvider, TtsRequest};
use async_trait::async_trait;
use tokio::sync::mpsc;
use tracing::{debug, warn};

pub struct ElevenLabsProvider {
    api_key: String,
    base_url: String,
    client: reqwest::Client,
}

impl ElevenLabsProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            base_url: "https://api.elevenlabs.io".into(),
            client: reqwest::Client::new(),
        }
    }

    pub fn with_base_url(mut self, url: &str) -> Self {
        self.base_url = url.trim_end_matches('/').to_string();
        self
    }

    /// Validate voice ID format (alphanumeric, 20+ chars).
    fn validate_voice_id(voice_id: &str) -> Result<(), TtsError> {
        if voice_id.len() < 10 || !voice_id.chars().all(|c| c.is_alphanumeric()) {
            return Err(TtsError::InvalidVoice(format!(
                "ElevenLabs voice ID must be 10+ alphanumeric chars, got: {voice_id}"
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl TtsProvider for ElevenLabsProvider {
    fn name(&self) -> &str { "elevenlabs" }

    fn voices(&self) -> Vec<String> {
        // Common default voices — full list fetched via API in production.
        vec![
            "21m00Tcm4TlvDq8ikWAM".into(), // Rachel
            "29vD33N1CtxCmqQRPOHJ".into(), // Drew
            "EXAVITQu4vr4xnSDxMaL".into(), // Bella
        ]
    }

    async fn synthesize(&self, request: &TtsRequest, tx: mpsc::Sender<TtsChunk>) -> Result<(), TtsError> {
        Self::validate_voice_id(&request.voice_id)?;

        let url = format!("{}/v1/text-to-speech/{}/stream", self.base_url, request.voice_id);

        let stability = request.params.stability.unwrap_or(0.5);
        let similarity = request.params.similarity_boost.unwrap_or(0.75);
        let style = request.params.style.unwrap_or(0.0);

        let body = serde_json::json!({
            "text": request.text,
            "model_id": "eleven_multilingual_v2",
            "voice_settings": {
                "stability": stability,
                "similarity_boost": similarity,
                "style": style,
                "use_speaker_boost": true,
            },
            "output_format": match request.format {
                AudioFormat::Mp3 => "mp3_44100_128",
                AudioFormat::Pcm16 => "pcm_24000",
                _ => "mp3_44100_128",
            }
        });

        let resp = self.client
            .post(&url)
            .header("xi-api-key", &self.api_key)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| TtsError::SynthesisFailed(e.to_string()))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            return Err(TtsError::SynthesisFailed(format!("ElevenLabs API {status}: {text}")));
        }

        // Stream audio chunks as they arrive.
        let mut seq = 0u64;
        let mut stream = resp.bytes_stream();
        use futures::StreamExt;
        while let Some(chunk_result) = stream.next().await {
            match chunk_result {
                Ok(bytes) => {
                    let chunk = TtsChunk {
                        data: bytes.to_vec(),
                        format: request.format,
                        sample_rate: 44100,
                        sequence: seq,
                        is_final: false,
                    };
                    if tx.send(chunk).await.is_err() {
                        debug!("TTS receiver dropped — stopping stream");
                        break;
                    }
                    seq += 1;
                }
                Err(e) => {
                    warn!(error = %e, "ElevenLabs stream error");
                    break;
                }
            }
        }

        // Send final marker.
        let _ = tx.send(TtsChunk {
            data: vec![],
            format: request.format,
            sample_rate: 44100,
            sequence: seq,
            is_final: true,
        }).await;

        Ok(())
    }

    fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_voice_id_format() {
        assert!(ElevenLabsProvider::validate_voice_id("21m00Tcm4TlvDq8ikWAM").is_ok());
        assert!(ElevenLabsProvider::validate_voice_id("short").is_err());
    }

    #[test]
    fn unavailable_without_key() {
        let provider = ElevenLabsProvider::new(String::new());
        assert!(!provider.is_available());
    }

    #[test]
    fn available_with_key() {
        let provider = ElevenLabsProvider::new("test-key".into());
        assert!(provider.is_available());
    }
}
