//! Voice pipeline — real-time speech-to-text, text-to-speech, and voice activity detection.
//!
//! ## Architecture
//! ```text
//! Microphone → VAD → STT → Agent → TTS → Speaker
//!                ↓                    ↓
//!           VoiceMetrics        AudioConfig
//! ```
//!
//! - **VoiceActivityDetector**: Detects speech segments in audio stream
//! - **SttEngine**: Speech-to-text transcription (Whisper, Deepgram, Azure)
//! - **TtsEngine**: Text-to-speech synthesis (ElevenLabs, Azure, system TTS)
//! - **VoicePipeline**: Orchestrates the full voice loop
//! - **AudioBuffer**: Manages audio chunk buffering and resampling

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};
use chrono::{DateTime, Utc};

// ── Audio Types ───────────────────────────────────────────

/// Audio sample format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleFormat {
    I16,
    F32,
}

/// Audio configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub channels: u16,
    pub format: SampleFormat,
    pub chunk_duration_ms: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 16000,
            channels: 1,
            format: SampleFormat::I16,
            chunk_duration_ms: 30,
        }
    }
}

impl AudioConfig {
    /// Samples per chunk.
    pub fn chunk_samples(&self) -> usize {
        (self.sample_rate as usize * self.chunk_duration_ms as usize / 1000)
            * self.channels as usize
    }

    /// Bytes per chunk.
    pub fn chunk_bytes(&self) -> usize {
        let bytes_per_sample = match self.format {
            SampleFormat::I16 => 2,
            SampleFormat::F32 => 4,
        };
        self.chunk_samples() * bytes_per_sample
    }
}

/// A chunk of audio data.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    pub data: Vec<u8>,
    pub config: AudioConfig,
    pub timestamp: DateTime<Utc>,
    pub duration_ms: u32,
}

/// Voice activity detection result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VadResult {
    /// Whether speech is detected.
    pub is_speech: bool,
    /// Confidence [0.0, 1.0].
    pub confidence: f32,
    /// RMS energy of the chunk.
    pub energy: f32,
}

// ── VAD ───────────────────────────────────────────────────

/// Voice Activity Detection configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VadConfig {
    /// Energy threshold for speech detection.
    pub energy_threshold: f32,
    /// Minimum speech duration to trigger (ms).
    pub min_speech_ms: u32,
    /// Silence padding after speech ends (ms).
    pub silence_padding_ms: u32,
    /// Consecutive speech frames needed to start.
    pub onset_frames: u32,
    /// Consecutive silence frames needed to stop.
    pub offset_frames: u32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            energy_threshold: 0.01,
            min_speech_ms: 200,
            silence_padding_ms: 300,
            onset_frames: 3,
            offset_frames: 10,
        }
    }
}

/// Voice Activity Detector — detects speech segments in audio.
pub struct VoiceActivityDetector {
    config: VadConfig,
    speech_frames: u32,
    silence_frames: u32,
    in_speech: bool,
}

impl VoiceActivityDetector {
    pub fn new(config: VadConfig) -> Self {
        Self {
            config,
            speech_frames: 0,
            silence_frames: 0,
            in_speech: false,
        }
    }

    /// Process an audio chunk and return VAD result.
    pub fn process(&mut self, chunk: &AudioChunk) -> VadResult {
        let energy = compute_rms_energy(&chunk.data, &chunk.config);
        let is_active = energy > self.config.energy_threshold;

        if is_active {
            self.speech_frames += 1;
            self.silence_frames = 0;
        } else {
            self.silence_frames += 1;
            if !self.in_speech {
                self.speech_frames = 0;
            }
        }

        // State transitions
        if !self.in_speech && self.speech_frames >= self.config.onset_frames {
            self.in_speech = true;
            debug!(energy, "speech onset detected");
        } else if self.in_speech && self.silence_frames >= self.config.offset_frames {
            self.in_speech = false;
            self.speech_frames = 0;
            debug!(energy, "speech offset detected");
        }

        VadResult {
            is_speech: self.in_speech,
            confidence: if self.in_speech {
                (self.speech_frames as f32 / (self.speech_frames as f32 + 5.0)).min(1.0)
            } else {
                0.0
            },
            energy,
        }
    }

    /// Reset state.
    pub fn reset(&mut self) {
        self.speech_frames = 0;
        self.silence_frames = 0;
        self.in_speech = false;
    }

    /// Current speech state.
    pub fn is_speech(&self) -> bool {
        self.in_speech
    }
}

/// Compute RMS energy from raw audio bytes.
fn compute_rms_energy(data: &[u8], config: &AudioConfig) -> f32 {
    match config.format {
        SampleFormat::I16 => {
            if data.len() < 2 {
                return 0.0;
            }
            let samples: Vec<f32> = data
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                .collect();
            let sum: f32 = samples.iter().map(|s| s * s).sum();
            (sum / samples.len() as f32).sqrt()
        }
        SampleFormat::F32 => {
            if data.len() < 4 {
                return 0.0;
            }
            let samples: Vec<f32> = data
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            let sum: f32 = samples.iter().map(|s| s * s).sum();
            (sum / samples.len() as f32).sqrt()
        }
    }
}

// ── STT Engine ────────────────────────────────────────────

/// Speech-to-text result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionResult {
    pub text: String,
    pub language: Option<String>,
    pub confidence: f32,
    pub segments: Vec<TranscriptionSegment>,
    pub duration_ms: u64,
}

/// A segment of the transcription with timing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionSegment {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub confidence: f32,
}

/// STT provider identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SttProvider {
    Whisper,
    Deepgram,
    Azure,
    Google,
    SystemDefault,
}

/// Speech-to-text engine trait.
#[async_trait]
pub trait SttEngine: Send + Sync {
    /// Provider identifier.
    fn provider(&self) -> SttProvider;

    /// Transcribe audio data.
    async fn transcribe(
        &self,
        audio: &[u8],
        config: &AudioConfig,
    ) -> Result<TranscriptionResult, VoiceError>;

    /// Whether this engine supports streaming transcription.
    fn supports_streaming(&self) -> bool {
        false
    }
}

// ── TTS Engine ────────────────────────────────────────────

/// TTS synthesis result.
#[derive(Debug, Clone)]
pub struct SynthesisResult {
    pub audio_data: Vec<u8>,
    pub config: AudioConfig,
    pub duration_ms: u64,
}

/// TTS voice specification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceSpec {
    pub voice_id: String,
    pub name: String,
    pub language: String,
    pub gender: Option<String>,
    pub style: Option<String>,
}

/// TTS provider identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TtsProvider {
    ElevenLabs,
    Azure,
    Google,
    SystemDefault,
}

/// Text-to-speech engine trait.
#[async_trait]
pub trait TtsEngine: Send + Sync {
    fn provider(&self) -> TtsProvider;

    /// List available voices.
    async fn list_voices(&self) -> Result<Vec<VoiceSpec>, VoiceError>;

    /// Synthesize text to speech.
    async fn synthesize(
        &self,
        text: &str,
        voice: &VoiceSpec,
    ) -> Result<SynthesisResult, VoiceError>;
}

// ── Voice Pipeline ────────────────────────────────────────

/// Voice pipeline configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoicePipelineConfig {
    pub audio: AudioConfig,
    pub vad: VadConfig,
    pub stt_provider: SttProvider,
    pub tts_provider: TtsProvider,
    pub auto_send_after_silence_ms: u32,
    pub max_recording_seconds: u32,
    pub echo_cancellation: bool,
    pub noise_suppression: bool,
}

impl Default for VoicePipelineConfig {
    fn default() -> Self {
        Self {
            audio: AudioConfig::default(),
            vad: VadConfig::default(),
            stt_provider: SttProvider::Whisper,
            tts_provider: TtsProvider::SystemDefault,
            auto_send_after_silence_ms: 1500,
            max_recording_seconds: 120,
            echo_cancellation: true,
            noise_suppression: true,
        }
    }
}

/// Voice pipeline state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineState {
    Idle,
    Listening,
    Recording,
    Transcribing,
    Processing,
    Speaking,
    Error,
}

/// Voice pipeline event (for UI updates).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum VoiceEvent {
    StateChanged { state: PipelineState },
    VadUpdate { is_speech: bool, energy: f32 },
    TranscriptionPartial { text: String },
    TranscriptionFinal { text: String },
    SpeakingStarted,
    SpeakingFinished,
    Error { message: String },
}

/// Metrics for voice pipeline performance.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VoiceMetrics {
    pub total_audio_seconds: f64,
    pub total_speech_seconds: f64,
    pub transcriptions: u64,
    pub tts_requests: u64,
    pub avg_transcription_latency_ms: f64,
    pub avg_tts_latency_ms: f64,
    pub errors: u64,
}

/// The voice pipeline — orchestrates VAD → STT → Agent → TTS.
pub struct VoicePipeline {
    config: VoicePipelineConfig,
    state: Arc<RwLock<PipelineState>>,
    metrics: Arc<RwLock<VoiceMetrics>>,
    audio_buffer: Arc<RwLock<Vec<u8>>>,
    event_tx: Option<mpsc::Sender<VoiceEvent>>,
}

impl VoicePipeline {
    pub fn new(config: VoicePipelineConfig) -> Self {
        Self {
            config,
            state: Arc::new(RwLock::new(PipelineState::Idle)),
            metrics: Arc::new(RwLock::new(VoiceMetrics::default())),
            audio_buffer: Arc::new(RwLock::new(Vec::new())),
            event_tx: None,
        }
    }

    /// Set the event channel for UI updates.
    pub fn with_event_channel(mut self, tx: mpsc::Sender<VoiceEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// Get current pipeline state.
    pub async fn state(&self) -> PipelineState {
        *self.state.read().await
    }

    /// Set pipeline state and emit event.
    async fn set_state(&self, new_state: PipelineState) {
        let mut state = self.state.write().await;
        *state = new_state;
        if let Some(tx) = &self.event_tx {
            let _ = tx
                .send(VoiceEvent::StateChanged { state: new_state })
                .await;
        }
        info!(state = ?new_state, "voice pipeline state changed");
    }

    /// Start listening for voice input.
    pub async fn start_listening(&self) -> Result<(), VoiceError> {
        let current = self.state().await;
        if current != PipelineState::Idle {
            return Err(VoiceError::InvalidState(format!(
                "cannot start listening from {:?}",
                current
            )));
        }
        self.audio_buffer.write().await.clear();
        self.set_state(PipelineState::Listening).await;
        Ok(())
    }

    /// Stop listening and return to idle.
    pub async fn stop_listening(&self) -> Result<(), VoiceError> {
        self.set_state(PipelineState::Idle).await;
        self.audio_buffer.write().await.clear();
        Ok(())
    }

    /// Feed an audio chunk into the pipeline.
    pub async fn feed_audio(&self, chunk: AudioChunk) -> Result<(), VoiceError> {
        let state = self.state().await;
        if state != PipelineState::Listening && state != PipelineState::Recording {
            return Ok(()); // Ignore audio when not in listening states
        }

        let mut buffer = self.audio_buffer.write().await;
        buffer.extend_from_slice(&chunk.data);

        // Update metrics
        let mut metrics = self.metrics.write().await;
        metrics.total_audio_seconds +=
            chunk.duration_ms as f64 / 1000.0;

        Ok(())
    }

    /// Get the accumulated audio buffer.
    pub async fn get_audio_buffer(&self) -> Vec<u8> {
        self.audio_buffer.read().await.clone()
    }

    /// Get voice metrics.
    pub async fn metrics(&self) -> VoiceMetrics {
        self.metrics.read().await.clone()
    }

    /// Reset metrics.
    pub async fn reset_metrics(&self) {
        *self.metrics.write().await = VoiceMetrics::default();
    }

    /// Get pipeline configuration.
    pub fn config(&self) -> &VoicePipelineConfig {
        &self.config
    }
}

/// Voice pipeline error.
#[derive(Debug, thiserror::Error)]
pub enum VoiceError {
    #[error("STT error: {0}")]
    SttError(String),
    #[error("TTS error: {0}")]
    TtsError(String),
    #[error("audio error: {0}")]
    AudioError(String),
    #[error("invalid state: {0}")]
    InvalidState(String),
    #[error("provider unavailable: {0}")]
    ProviderUnavailable(String),
    #[error("configuration error: {0}")]
    ConfigError(String),
}

// ── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audio_config() {
        let config = AudioConfig::default();
        assert_eq!(config.sample_rate, 16000);
        assert_eq!(config.channels, 1);
        // 16000 * 30 / 1000 = 480 samples per 30ms chunk
        assert_eq!(config.chunk_samples(), 480);
        // 480 * 2 bytes (i16) = 960 bytes
        assert_eq!(config.chunk_bytes(), 960);
    }

    #[test]
    fn test_vad_onset_offset() {
        let mut vad = VoiceActivityDetector::new(VadConfig {
            energy_threshold: 0.01,
            onset_frames: 2,
            offset_frames: 3,
            ..Default::default()
        });

        let config = AudioConfig::default();

        // Create silent chunk (all zeros = zero energy)
        let silent_chunk = AudioChunk {
            data: vec![0u8; 960],
            config: config.clone(),
            timestamp: Utc::now(),
            duration_ms: 30,
        };

        // Create speech chunk (high amplitude signal)
        let mut speech_data = Vec::with_capacity(960);
        for i in 0..480 {
            let sample = ((i as f32 * 0.1).sin() * 16000.0) as i16;
            speech_data.extend_from_slice(&sample.to_le_bytes());
        }
        let speech_chunk = AudioChunk {
            data: speech_data,
            config: config.clone(),
            timestamp: Utc::now(),
            duration_ms: 30,
        };

        // Initially not in speech
        assert!(!vad.is_speech());

        // One speech frame — not enough for onset (need 2)
        let r = vad.process(&speech_chunk);
        assert!(!r.is_speech);

        // Second speech frame — onset triggered
        let r = vad.process(&speech_chunk);
        assert!(r.is_speech);

        // Silent frames, but need 3 for offset
        let r = vad.process(&silent_chunk);
        assert!(r.is_speech); // still in speech

        let r = vad.process(&silent_chunk);
        assert!(r.is_speech); // still in speech

        let r = vad.process(&silent_chunk);
        assert!(!r.is_speech); // offset triggered
    }

    #[test]
    fn test_rms_energy_silence() {
        let config = AudioConfig::default();
        let data = vec![0u8; 960];
        let energy = compute_rms_energy(&data, &config);
        assert_eq!(energy, 0.0);
    }

    #[test]
    fn test_rms_energy_signal() {
        let config = AudioConfig::default();
        // Max amplitude signal
        let mut data = Vec::new();
        for _ in 0..480 {
            data.extend_from_slice(&i16::MAX.to_le_bytes());
        }
        let energy = compute_rms_energy(&data, &config);
        assert!(energy > 0.9);
    }

    #[tokio::test]
    async fn test_pipeline_states() {
        let pipeline = VoicePipeline::new(VoicePipelineConfig::default());
        assert_eq!(pipeline.state().await, PipelineState::Idle);

        pipeline.start_listening().await.unwrap();
        assert_eq!(pipeline.state().await, PipelineState::Listening);

        pipeline.stop_listening().await.unwrap();
        assert_eq!(pipeline.state().await, PipelineState::Idle);
    }

    #[tokio::test]
    async fn test_pipeline_invalid_state() {
        let pipeline = VoicePipeline::new(VoicePipelineConfig::default());
        pipeline.start_listening().await.unwrap();

        // Cannot start listening when already listening
        let result = pipeline.start_listening().await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_audio_buffering() {
        let pipeline = VoicePipeline::new(VoicePipelineConfig::default());
        pipeline.start_listening().await.unwrap();

        let config = AudioConfig::default();
        let chunk = AudioChunk {
            data: vec![1, 2, 3, 4],
            config,
            timestamp: Utc::now(),
            duration_ms: 30,
        };

        pipeline.feed_audio(chunk).await.unwrap();
        let buffer = pipeline.get_audio_buffer().await;
        assert_eq!(buffer, vec![1, 2, 3, 4]);
    }

    #[tokio::test]
    async fn test_metrics() {
        let pipeline = VoicePipeline::new(VoicePipelineConfig::default());
        pipeline.start_listening().await.unwrap();

        let config = AudioConfig::default();
        for _ in 0..10 {
            let chunk = AudioChunk {
                data: vec![0u8; 960],
                config: config.clone(),
                timestamp: Utc::now(),
                duration_ms: 30,
            };
            pipeline.feed_audio(chunk).await.unwrap();
        }

        let metrics = pipeline.metrics().await;
        assert!(metrics.total_audio_seconds > 0.0);
    }

    #[test]
    fn test_voice_event_serde() {
        let event = VoiceEvent::StateChanged {
            state: PipelineState::Listening,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("listening"));

        let event = VoiceEvent::VadUpdate {
            is_speech: true,
            energy: 0.5,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("0.5"));
    }

    #[test]
    fn test_pipeline_config_default() {
        let config = VoicePipelineConfig::default();
        assert_eq!(config.auto_send_after_silence_ms, 1500);
        assert_eq!(config.max_recording_seconds, 120);
        assert!(config.echo_cancellation);
        assert!(config.noise_suppression);
    }
}
