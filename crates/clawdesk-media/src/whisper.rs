//! Local Whisper (whisper.cpp) speech-to-text engine.
//!
//! Uses `whisper-rs` bindings to run GGML Whisper models locally.
//! Audio is expected as WAV (16-bit PCM, 16 kHz, mono) — the frontend
//! records via the browser's MediaRecorder and sends the bytes to Rust.
//!
//! ## Model Management
//! Models are stored in `~/.clawdesk/models/whisper/` and can be
//! downloaded from HuggingFace on first use.

use crate::voice::{AudioConfig, SampleFormat, SttEngine, SttProvider, TranscriptionResult, TranscriptionSegment, VoiceError};
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

// ── Model Catalog ─────────────────────────────────────────

/// Available Whisper model sizes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WhisperModel {
    Tiny,
    Base,
    Small,
    Medium,
    Large,
}

impl WhisperModel {
    /// HuggingFace download URL for the GGML model.
    pub fn download_url(&self) -> &'static str {
        match self {
            Self::Tiny => "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.bin",
            Self::Base => "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.bin",
            Self::Small => "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-small.bin",
            Self::Medium => "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-medium.bin",
            Self::Large => "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-large-v3.bin",
        }
    }

    /// Filename for the model.
    pub fn filename(&self) -> &'static str {
        match self {
            Self::Tiny => "ggml-tiny.bin",
            Self::Base => "ggml-base.bin",
            Self::Small => "ggml-small.bin",
            Self::Medium => "ggml-medium.bin",
            Self::Large => "ggml-large-v3.bin",
        }
    }

    /// Human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Tiny => "Tiny (~75 MB)",
            Self::Base => "Base (~142 MB)",
            Self::Small => "Small (~466 MB)",
            Self::Medium => "Medium (~1.5 GB)",
            Self::Large => "Large V3 (~3.1 GB)",
        }
    }

    /// Approximate file size in bytes (for download progress).
    pub fn approx_bytes(&self) -> u64 {
        match self {
            Self::Tiny => 75_000_000,
            Self::Base => 142_000_000,
            Self::Small => 466_000_000,
            Self::Medium => 1_500_000_000,
            Self::Large => 3_100_000_000,
        }
    }
}

impl std::fmt::Display for WhisperModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// Status of a whisper model on disk.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WhisperModelStatus {
    pub model: WhisperModel,
    pub downloaded: bool,
    pub path: Option<String>,
    pub size_bytes: Option<u64>,
}

// ── WAV Decoding ──────────────────────────────────────────

/// Decode WAV bytes to f32 samples at 16 kHz mono (what Whisper expects).
fn decode_wav_to_f32(wav_bytes: &[u8]) -> Result<Vec<f32>, VoiceError> {
    let cursor = std::io::Cursor::new(wav_bytes);
    let reader = hound::WavReader::new(cursor)
        .map_err(|e| VoiceError::AudioError(format!("failed to parse WAV: {e}")))?;

    let spec = reader.spec();
    debug!(
        sample_rate = spec.sample_rate,
        channels = spec.channels,
        bits = spec.bits_per_sample,
        format = ?spec.sample_format,
        "decoding WAV"
    );

    // Read all samples as f32
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Int => {
            let max_val = (1i64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .into_samples::<i32>()
                .filter_map(|s| s.ok())
                .map(|s| s as f32 / max_val)
                .collect()
        }
        hound::SampleFormat::Float => {
            reader
                .into_samples::<f32>()
                .filter_map(|s| s.ok())
                .collect()
        }
    };

    // Downmix to mono if stereo
    let mono = if spec.channels > 1 {
        samples
            .chunks(spec.channels as usize)
            .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
            .collect()
    } else {
        samples
    };

    // Resample to 16 kHz if needed (linear interpolation — good enough for speech)
    let resampled = if spec.sample_rate != 16000 {
        resample_linear(&mono, spec.sample_rate, 16000)
    } else {
        mono
    };

    Ok(resampled)
}

/// Decode raw PCM bytes (from AudioConfig) to f32 at 16 kHz.
fn decode_raw_pcm_to_f32(data: &[u8], config: &AudioConfig) -> Result<Vec<f32>, VoiceError> {
    let samples: Vec<f32> = match config.format {
        SampleFormat::I16 => {
            data.chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                .collect()
        }
        SampleFormat::F32 => {
            data.chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect()
        }
    };

    // Downmix to mono
    let mono = if config.channels > 1 {
        samples
            .chunks(config.channels as usize)
            .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
            .collect()
    } else {
        samples
    };

    // Resample to 16 kHz
    if config.sample_rate != 16000 {
        Ok(resample_linear(&mono, config.sample_rate, 16000))
    } else {
        Ok(mono)
    }
}

/// Simple linear interpolation resampler.
fn resample_linear(input: &[f32], from_rate: u32, to_rate: u32) -> Vec<f32> {
    if from_rate == to_rate || input.is_empty() {
        return input.to_vec();
    }
    let ratio = from_rate as f64 / to_rate as f64;
    let out_len = (input.len() as f64 / ratio).ceil() as usize;
    let mut output = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 * ratio;
        let idx = src_pos as usize;
        let frac = (src_pos - idx as f64) as f32;
        let sample = if idx + 1 < input.len() {
            input[idx] * (1.0 - frac) + input[idx + 1] * frac
        } else if idx < input.len() {
            input[idx]
        } else {
            0.0
        };
        output.push(sample);
    }
    output
}

// ── Whisper Engine ────────────────────────────────────────

/// Local Whisper speech-to-text engine backed by whisper.cpp.
pub struct WhisperSttEngine {
    /// Directory where GGML model files are stored.
    models_dir: PathBuf,
    /// Currently loaded whisper context (lazy-loaded on first transcription).
    context: Arc<RwLock<Option<WhisperContextHolder>>>,
    /// Which model size to use.
    model: WhisperModel,
    /// Optional language hint (e.g., "en").
    language: Option<String>,
}

struct WhisperContextHolder {
    ctx: WhisperContext,
    model: WhisperModel,
}

// WhisperContext is Send + Sync via C FFI (whisper.cpp is thread-safe for inference)
unsafe impl Send for WhisperContextHolder {}
unsafe impl Sync for WhisperContextHolder {}

impl WhisperSttEngine {
    /// Create a new Whisper engine.
    ///
    /// `models_dir` is where GGML model files are stored (e.g., `~/.clawdesk/models/whisper/`).
    pub fn new(models_dir: PathBuf, model: WhisperModel) -> Self {
        Self {
            models_dir,
            context: Arc::new(RwLock::new(None)),
            model,
            language: None,
        }
    }

    /// Set the language hint for transcription.
    pub fn with_language(mut self, lang: impl Into<String>) -> Self {
        self.language = Some(lang.into());
        self
    }

    /// Get the model directory path.
    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    /// Get the current model.
    pub fn model(&self) -> WhisperModel {
        self.model
    }

    /// Path to the model file on disk.
    pub fn model_path(&self) -> PathBuf {
        self.models_dir.join(self.model.filename())
    }

    /// Check if the model file exists on disk.
    pub fn is_model_downloaded(&self) -> bool {
        self.model_path().exists()
    }

    /// Get status of all available models.
    pub fn list_models(&self) -> Vec<WhisperModelStatus> {
        [
            WhisperModel::Tiny,
            WhisperModel::Base,
            WhisperModel::Small,
            WhisperModel::Medium,
            WhisperModel::Large,
        ]
        .iter()
        .map(|m| {
            let path = self.models_dir.join(m.filename());
            let downloaded = path.exists();
            let size_bytes = if downloaded {
                std::fs::metadata(&path).ok().map(|md| md.len())
            } else {
                None
            };
            WhisperModelStatus {
                model: *m,
                downloaded,
                path: if downloaded {
                    Some(path.to_string_lossy().to_string())
                } else {
                    None
                },
                size_bytes,
            }
        })
        .collect()
    }

    /// Download a model from HuggingFace.
    pub async fn download_model(
        models_dir: &Path,
        model: WhisperModel,
        progress_cb: Option<Box<dyn Fn(u64, u64) + Send + 'static>>,
    ) -> Result<PathBuf, VoiceError> {
        let model_path = models_dir.join(model.filename());

        if model_path.exists() {
            info!(model = ?model, path = %model_path.display(), "model already downloaded");
            return Ok(model_path);
        }

        // Ensure directory exists
        tokio::fs::create_dir_all(models_dir)
            .await
            .map_err(|e| VoiceError::AudioError(format!("failed to create models dir: {e}")))?;

        let url = model.download_url();
        info!(model = ?model, url, "downloading whisper model");

        let client = reqwest::Client::new();
        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| VoiceError::SttError(format!("download failed: {e}")))?;

        if !resp.status().is_success() {
            return Err(VoiceError::SttError(format!(
                "download failed: HTTP {}",
                resp.status()
            )));
        }

        let total_size = resp.content_length().unwrap_or(model.approx_bytes());

        // Stream to a temp file, then rename
        let tmp_path = model_path.with_extension("tmp");
        let mut file = tokio::fs::File::create(&tmp_path)
            .await
            .map_err(|e| VoiceError::AudioError(format!("failed to create temp file: {e}")))?;

        use tokio::io::AsyncWriteExt;
        use futures::StreamExt;
        let mut downloaded: u64 = 0;
        let mut stream = resp.bytes_stream();

        while let Some(chunk_result) = stream.next().await {
            let chunk = chunk_result.map_err(|e| VoiceError::SttError(format!("download stream error: {e}")))?;
            file.write_all(&chunk)
                .await
                .map_err(|e| VoiceError::AudioError(format!("write error: {e}")))?;
            downloaded += chunk.len() as u64;
            if let Some(ref cb) = progress_cb {
                cb(downloaded, total_size);
            }
        }

        file.flush()
            .await
            .map_err(|e| VoiceError::AudioError(format!("flush error: {e}")))?;
        drop(file);

        // Atomic rename
        tokio::fs::rename(&tmp_path, &model_path)
            .await
            .map_err(|e| VoiceError::AudioError(format!("rename error: {e}")))?;

        info!(model = ?model, path = %model_path.display(), bytes = downloaded, "model download complete");
        Ok(model_path)
    }

    /// Ensure the whisper context is loaded. Lazy-loads on first call.
    async fn ensure_context(&self) -> Result<(), VoiceError> {
        let guard = self.context.read().await;
        if let Some(holder) = guard.as_ref() {
            if holder.model == self.model {
                return Ok(());
            }
        }
        drop(guard);

        let model_path = self.model_path();
        if !model_path.exists() {
            return Err(VoiceError::ProviderUnavailable(format!(
                "Whisper model not found at {}. Download it first.",
                model_path.display()
            )));
        }

        info!(model = ?self.model, path = %model_path.display(), "loading whisper model");

        let path_str = model_path.to_string_lossy().to_string();
        let model = self.model;

        // Load in a blocking task since it's CPU-intensive
        let ctx = tokio::task::spawn_blocking(move || {
            WhisperContext::new_with_params(&path_str, WhisperContextParameters::default())
                .map_err(|e| VoiceError::SttError(format!("failed to load whisper model: {e}")))
        })
        .await
        .map_err(|e| VoiceError::SttError(format!("join error: {e}")))?
        ?;

        let mut guard = self.context.write().await;
        *guard = Some(WhisperContextHolder { ctx, model });
        info!(model = ?self.model, "whisper model loaded");
        Ok(())
    }

    /// Transcribe f32 audio samples (16 kHz mono).
    async fn transcribe_samples(&self, samples: Vec<f32>) -> Result<TranscriptionResult, VoiceError> {
        self.ensure_context().await?;

        let language = self.language.clone();
        let context = Arc::clone(&self.context);

        let start = std::time::Instant::now();

        // Run inference in a blocking task
        let result = tokio::task::spawn_blocking(move || {
            let guard = context.blocking_read();
            let holder = guard
                .as_ref()
                .ok_or_else(|| VoiceError::SttError("context not loaded".into()))?;

            let mut state = holder.ctx.create_state()
                .map_err(|e| VoiceError::SttError(format!("failed to create whisper state: {e}")))?;

            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });

            // Configure transcription
            params.set_print_special(false);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);
            params.set_token_timestamps(true);
            params.set_single_segment(false);

            if let Some(ref lang) = language {
                params.set_language(Some(lang));
            } else {
                // Auto-detect language
                params.set_language(None);
            }

            // Run transcription
            state.full(params, &samples)
                .map_err(|e| VoiceError::SttError(format!("whisper transcription failed: {e}")))?;

            let num_segments = state.full_n_segments()
                .map_err(|e| VoiceError::SttError(format!("failed to get segments: {e}")))?;

            let mut text = String::new();
            let mut segments = Vec::new();

            for i in 0..num_segments {
                let segment_text = state.full_get_segment_text(i)
                    .map_err(|e| VoiceError::SttError(format!("failed to get segment text: {e}")))?;
                let start_ts = state.full_get_segment_t0(i)
                    .map_err(|e| VoiceError::SttError(format!("failed to get segment start: {e}")))?;
                let end_ts = state.full_get_segment_t1(i)
                    .map_err(|e| VoiceError::SttError(format!("failed to get segment end: {e}")))?;

                if !text.is_empty() && !segment_text.starts_with(' ') {
                    text.push(' ');
                }
                text.push_str(segment_text.trim());

                segments.push(TranscriptionSegment {
                    text: segment_text.trim().to_string(),
                    start_ms: (start_ts as u64) * 10, // whisper timestamps are in 10ms units
                    end_ms: (end_ts as u64) * 10,
                    confidence: 0.9, // whisper.cpp doesn't expose per-segment confidence
                });
            }

            Ok::<(String, Vec<TranscriptionSegment>), VoiceError>((text, segments))
        })
        .await
        .map_err(|e| VoiceError::SttError(format!("join error: {e}")))?
        ?;

        let duration_ms = start.elapsed().as_millis() as u64;

        Ok(TranscriptionResult {
            text: result.0.trim().to_string(),
            language: self.language.clone(),
            confidence: 0.9,
            segments: result.1,
            duration_ms,
        })
    }

    /// Transcribe WAV audio bytes directly.
    pub async fn transcribe_wav(&self, wav_bytes: &[u8]) -> Result<TranscriptionResult, VoiceError> {
        let samples = decode_wav_to_f32(wav_bytes)?;
        if samples.is_empty() {
            return Err(VoiceError::AudioError("empty audio data".into()));
        }
        debug!(sample_count = samples.len(), duration_secs = samples.len() as f32 / 16000.0, "transcribing WAV");
        self.transcribe_samples(samples).await
    }
}

#[async_trait]
impl SttEngine for WhisperSttEngine {
    fn provider(&self) -> SttProvider {
        SttProvider::Whisper
    }

    async fn transcribe(
        &self,
        audio: &[u8],
        config: &AudioConfig,
    ) -> Result<TranscriptionResult, VoiceError> {
        // Detect if the audio is WAV format (has RIFF header)
        if audio.len() >= 12 && &audio[0..4] == b"RIFF" && &audio[8..12] == b"WAVE" {
            return self.transcribe_wav(audio).await;
        }

        // Otherwise treat as raw PCM according to config
        let samples = decode_raw_pcm_to_f32(audio, config)?;
        if samples.is_empty() {
            return Err(VoiceError::AudioError("empty audio data".into()));
        }
        debug!(sample_count = samples.len(), duration_secs = samples.len() as f32 / 16000.0, "transcribing raw PCM");
        self.transcribe_samples(samples).await
    }

    fn supports_streaming(&self) -> bool {
        false
    }
}

// ── Default Models Directory ──────────────────────────────

/// Get the default models directory: `~/.clawdesk/models/whisper/`
pub fn default_models_dir() -> PathBuf {
    dirs_path().join("models").join("whisper")
}

fn dirs_path() -> PathBuf {
    if let Some(home) = dirs_home() {
        home.join(".clawdesk")
    } else {
        PathBuf::from(".clawdesk")
    }
}

fn dirs_home() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var("USERPROFILE").ok().map(PathBuf::from)
    }
    #[cfg(target_os = "linux")]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}

// ── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_model_catalog() {
        let models = [
            WhisperModel::Tiny,
            WhisperModel::Base,
            WhisperModel::Small,
            WhisperModel::Medium,
            WhisperModel::Large,
        ];
        for m in &models {
            assert!(!m.filename().is_empty());
            assert!(m.download_url().starts_with("https://"));
            assert!(m.approx_bytes() > 0);
            assert!(!m.label().is_empty());
        }
    }

    #[test]
    fn test_resample_identity() {
        let input = vec![0.1, 0.2, 0.3, 0.4, 0.5];
        let output = resample_linear(&input, 16000, 16000);
        assert_eq!(output, input);
    }

    #[test]
    fn test_resample_downsample() {
        // 32kHz → 16kHz should roughly halve the samples
        let input: Vec<f32> = (0..320).map(|i| (i as f32 * 0.01).sin()).collect();
        let output = resample_linear(&input, 32000, 16000);
        assert!(output.len() > 150 && output.len() < 170);
    }

    #[test]
    fn test_resample_upsample() {
        let input: Vec<f32> = (0..160).map(|i| (i as f32 * 0.01).sin()).collect();
        let output = resample_linear(&input, 8000, 16000);
        assert!(output.len() > 300 && output.len() < 340);
    }

    #[test]
    fn test_decode_wav_valid() {
        // Create a minimal valid WAV in memory
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
            for i in 0..1600 {
                let sample = ((i as f32 * 0.1).sin() * 16000.0) as i16;
                writer.write_sample(sample).unwrap();
            }
            writer.finalize().unwrap();
        }
        let wav_bytes = cursor.into_inner();
        let samples = decode_wav_to_f32(&wav_bytes).unwrap();
        assert_eq!(samples.len(), 1600);
    }

    #[test]
    fn test_decode_wav_stereo_downmix() {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 16000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
            for _ in 0..1600 {
                writer.write_sample(1000i16).unwrap(); // left
                writer.write_sample(3000i16).unwrap(); // right
            }
            writer.finalize().unwrap();
        }
        let wav_bytes = cursor.into_inner();
        let samples = decode_wav_to_f32(&wav_bytes).unwrap();
        // Stereo → mono: 1600 frames
        assert_eq!(samples.len(), 1600);
    }

    #[test]
    fn test_decode_raw_pcm_i16() {
        let config = AudioConfig {
            sample_rate: 16000,
            channels: 1,
            format: SampleFormat::I16,
            chunk_duration_ms: 30,
        };
        let data: Vec<u8> = (0..480)
            .flat_map(|i| {
                let sample = ((i as f32 * 0.1).sin() * 16000.0) as i16;
                sample.to_le_bytes().to_vec()
            })
            .collect();
        let samples = decode_raw_pcm_to_f32(&data, &config).unwrap();
        assert_eq!(samples.len(), 480);
    }

    #[test]
    fn test_default_models_dir() {
        let dir = default_models_dir();
        assert!(dir.to_string_lossy().contains("whisper"));
    }

    #[test]
    fn test_engine_model_not_downloaded() {
        let engine = WhisperSttEngine::new(PathBuf::from("/nonexistent"), WhisperModel::Tiny);
        assert!(!engine.is_model_downloaded());
    }

    #[test]
    fn test_model_status_list() {
        let engine = WhisperSttEngine::new(PathBuf::from("/tmp/test-whisper-models"), WhisperModel::Tiny);
        let statuses = engine.list_models();
        assert_eq!(statuses.len(), 5);
        for s in &statuses {
            assert!(!s.downloaded); // nothing downloaded in temp dir
        }
    }

    #[test]
    fn test_wav_detection_in_transcribe() {
        // Verify the RIFF/WAVE detection logic
        let riff_header = b"RIFF\x00\x00\x00\x00WAVE";
        assert!(riff_header.len() >= 12);
        assert_eq!(&riff_header[0..4], b"RIFF");
        assert_eq!(&riff_header[8..12], b"WAVE");
    }
}
