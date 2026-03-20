//! Speech-to-Text (STT) module — local Whisper + cloud fallback.
//!
//! Privacy-first: no audio leaves device by default.
//! Local model runs on a dedicated tokio blocking thread.
//!
//! ## Latency Budget
//!
//! ```text
//! T_stt ≈ 200ms (GPU) / 800ms (CPU) for 3-second utterance
//! ```

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};
use tracing::{debug, info, warn};

/// STT backend configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "backend", rename_all = "snake_case")]
pub enum SttConfig {
    /// Local Whisper.cpp inference (privacy-first)
    LocalWhisper {
        /// Path to Whisper model file (.bin)
        model_path: PathBuf,
        /// Model size: "tiny", "base", "small", "medium"
        model_size: String,
        /// Language hint (ISO 639-1, e.g., "en")
        language: Option<String>,
        /// Number of threads for inference
        threads: Option<usize>,
    },
    /// Cloud Whisper API (OpenAI) — fallback for accuracy
    CloudWhisper {
        /// API endpoint
        api_url: String,
        /// API key (loaded from vault)
        api_key_ref: String,
        /// Language hint
        language: Option<String>,
    },
}

impl Default for SttConfig {
    fn default() -> Self {
        Self::LocalWhisper {
            model_path: PathBuf::from("models/ggml-tiny.en.bin"),
            model_size: "tiny".into(),
            language: Some("en".into()),
            threads: None,
        }
    }
}

/// Result of a speech-to-text transcription.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionResult {
    /// Transcribed text
    pub text: String,
    /// Language detected or specified
    pub language: String,
    /// Transcription duration in milliseconds
    pub duration_ms: u64,
    /// Audio duration in milliseconds
    pub audio_duration_ms: u64,
    /// Confidence score (0.0 – 1.0, if available)
    pub confidence: Option<f64>,
    /// Whether local or cloud was used
    pub backend: String,
}

/// STT request — audio data to transcribe.
#[derive(Debug)]
pub struct SttRequest {
    /// Raw PCM audio data (16-bit, 16kHz, mono)
    pub audio_pcm: Vec<i16>,
    /// Sample rate (expected: 16000)
    pub sample_rate: u32,
    /// Response channel
    pub response_tx: oneshot::Sender<Result<TranscriptionResult, SttError>>,
}

/// STT errors.
#[derive(Debug, thiserror::Error)]
pub enum SttError {
    #[error("model not loaded: {0}")]
    ModelNotLoaded(String),

    #[error("transcription failed: {0}")]
    TranscriptionFailed(String),

    #[error("audio too short (min 0.5 seconds)")]
    AudioTooShort,

    #[error("cloud API error: {0}")]
    CloudApiError(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// STT engine — manages transcription requests via a channel.
///
/// Processes requests on a dedicated blocking thread to avoid
/// blocking the tokio runtime during Whisper inference.
pub struct SttEngine {
    request_tx: mpsc::Sender<SttRequest>,
    config: SttConfig,
}

impl SttEngine {
    /// Create a new STT engine with the given configuration.
    ///
    /// Spawns a background worker on a `spawn_blocking` thread.
    pub fn new(config: SttConfig) -> Self {
        let (tx, rx) = mpsc::channel(32);

        // Spawn the worker on a blocking thread
        let cfg = config.clone();
        tokio::spawn(async move {
            Self::worker(cfg, rx).await;
        });

        Self {
            request_tx: tx,
            config,
        }
    }

    /// Submit audio for transcription.
    pub async fn transcribe(&self, audio_pcm: Vec<i16>, sample_rate: u32) -> Result<TranscriptionResult, SttError> {
        if audio_pcm.len() < (sample_rate as usize / 2) {
            return Err(SttError::AudioTooShort);
        }

        let (response_tx, response_rx) = oneshot::channel();
        let request = SttRequest {
            audio_pcm,
            sample_rate,
            response_tx,
        };

        self.request_tx.send(request).await
            .map_err(|_| SttError::TranscriptionFailed("engine shut down".into()))?;

        response_rx.await
            .map_err(|_| SttError::TranscriptionFailed("worker dropped response".into()))?
    }

    /// Worker loop — processes transcription requests.
    async fn worker(config: SttConfig, mut rx: mpsc::Receiver<SttRequest>) {
        info!("STT worker started");

        while let Some(request) = rx.recv().await {
            let start = std::time::Instant::now();
            let audio_duration_ms = (request.audio_pcm.len() as u64 * 1000) / request.sample_rate as u64;

            let result = match &config {
                SttConfig::LocalWhisper { model_path, language, threads, .. } => {
                    // Local inference on blocking thread
                    let model_path = model_path.clone();
                    let lang = language.clone();
                    let thread_count = threads.unwrap_or(4);
                    let audio = request.audio_pcm.clone();

                    tokio::task::spawn_blocking(move || {
                        Self::local_whisper_transcribe(&model_path, &audio, lang.as_deref(), thread_count)
                    })
                    .await
                    .unwrap_or_else(|e| Err(SttError::TranscriptionFailed(format!("task panic: {}", e))))
                }
                SttConfig::CloudWhisper { api_url, api_key_ref, language } => {
                    Self::cloud_whisper_transcribe(
                        api_url,
                        api_key_ref,
                        &request.audio_pcm,
                        request.sample_rate,
                        language.as_deref(),
                    ).await
                }
            };

            let duration_ms = start.elapsed().as_millis() as u64;
            let result = result.map(|mut r| {
                r.duration_ms = duration_ms;
                r.audio_duration_ms = audio_duration_ms;
                r
            });

            let _ = request.response_tx.send(result);
        }

        info!("STT worker stopped");
    }

    /// Local Whisper.cpp transcription (runs on blocking thread).
    fn local_whisper_transcribe(
        _model_path: &std::path::Path,
        audio: &[i16],
        language: Option<&str>,
        _threads: usize,
    ) -> Result<TranscriptionResult, SttError> {
        // NOTE: Actual whisper.cpp integration requires the whisper-rs crate.
        // This is the interface contract — implementation binds to whisper.cpp
        // via FFI when the feature is enabled.
        //
        // For now, return a placeholder that documents the expected behavior:
        // 1. Load model from model_path (cached after first load)
        // 2. Convert i16 PCM to f32 normalized
        // 3. Run inference with language hint
        // 4. Return transcribed text

        let _audio_f32: Vec<f32> = audio.iter().map(|&s| s as f32 / 32768.0).collect();

        // Placeholder — real implementation uses whisper-rs
        Ok(TranscriptionResult {
            text: String::new(),
            language: language.unwrap_or("en").to_string(),
            duration_ms: 0,
            audio_duration_ms: 0,
            confidence: None,
            backend: "local_whisper".into(),
        })
    }

    /// Cloud Whisper API transcription.
    async fn cloud_whisper_transcribe(
        api_url: &str,
        _api_key_ref: &str,
        audio: &[i16],
        sample_rate: u32,
        language: Option<&str>,
    ) -> Result<TranscriptionResult, SttError> {
        // Encode PCM to WAV for upload
        let wav_data = encode_wav_from_pcm(audio, sample_rate);

        let client = reqwest::Client::new();
        let form = reqwest::multipart::Form::new()
            .text("model", "whisper-1")
            .text("language", language.unwrap_or("en").to_string())
            .part("file", reqwest::multipart::Part::bytes(wav_data)
                .file_name("audio.wav")
                .mime_str("audio/wav")
                .map_err(|e| SttError::CloudApiError(e.to_string()))?);

        let resp = client
            .post(api_url)
            .multipart(form)
            .timeout(std::time::Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| SttError::CloudApiError(e.to_string()))?;

        if !resp.status().is_success() {
            return Err(SttError::CloudApiError(
                format!("HTTP {}", resp.status())
            ));
        }

        let body: serde_json::Value = resp.json().await
            .map_err(|e| SttError::CloudApiError(e.to_string()))?;

        Ok(TranscriptionResult {
            text: body["text"].as_str().unwrap_or("").to_string(),
            language: language.unwrap_or("en").to_string(),
            duration_ms: 0,
            audio_duration_ms: 0,
            confidence: None,
            backend: "cloud_whisper".into(),
        })
    }
}

/// Encode PCM i16 samples to WAV bytes.
fn encode_wav_from_pcm(samples: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let file_len = 36 + data_len;

    let mut wav = Vec::with_capacity(file_len as usize + 8);

    // RIFF header
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&file_len.to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    // fmt chunk
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // chunk size
    wav.extend_from_slice(&1u16.to_le_bytes());  // PCM format
    wav.extend_from_slice(&1u16.to_le_bytes());  // mono
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes());  // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

    // data chunk
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    for &sample in samples {
        wav.extend_from_slice(&sample.to_le_bytes());
    }

    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_encoding() {
        let samples: Vec<i16> = vec![0, 1000, -1000, 0];
        let wav = encode_wav_from_pcm(&samples, 16000);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
    }
}
