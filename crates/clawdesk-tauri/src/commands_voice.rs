//! Voice commands — native audio recording + local Whisper STT transcription.
//!
//! ## Architecture
//! Audio is captured in Rust via cpal (native mic), not the browser's
//! MediaRecorder (unavailable in Tauri WebView). The frontend invokes
//! `start_voice_recording` / `stop_voice_recording` → Rust captures
//! to a WAV file → `transcribe_audio` runs whisper.cpp on it.
//!
//! Commands:
//! - `start_voice_recording`: Begin native mic capture
//! - `stop_voice_recording`: Stop capture, return WAV + auto-transcribe
//! - `transcribe_audio`: WAV bytes (base64) → text
//! - `get_whisper_models`: List available/downloaded models
//! - `download_whisper_model`: Download a GGML model from HuggingFace
//! - `delete_whisper_model`: Remove a downloaded model
//! - `get_voice_input_status`: Check if engine is ready

use crate::state::AppState;
use clawdesk_media::whisper::{WhisperModel, WhisperModelStatus, WhisperSttEngine};
use serde::{Deserialize, Serialize};
use tauri::State;
use tracing::{error, info};

// ── Response Types ────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct TranscribeResult {
    pub text: String,
    pub language: Option<String>,
    pub duration_ms: u64,
    pub segments: Vec<TranscribeSegment>,
}

#[derive(Debug, Serialize)]
pub struct TranscribeSegment {
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct VoiceInputStatus {
    pub engine_ready: bool,
    pub model: String,
    pub model_downloaded: bool,
    pub models_dir: String,
}

#[derive(Debug, Deserialize)]
pub struct DownloadModelRequest {
    pub model: String,
}

#[derive(Debug, Serialize)]
pub struct RecordingResponse {
    pub success: bool,
    pub state: String,
    pub sample_rate: Option<u32>,
    pub error: Option<String>,
}

// ── Commands ──────────────────────────────────────────────

/// Start native mic capture via cpal.
#[tauri::command]
pub async fn start_voice_recording(
    state: State<'_, AppState>,
) -> Result<RecordingResponse, String> {
    let mut recorder = state.audio_recorder.lock();
    match recorder.start_recording() {
        Ok(sample_rate) => {
            info!(sample_rate, "voice recording started");
            Ok(RecordingResponse {
                success: true,
                state: format!("{:?}", recorder.state()),
                sample_rate: Some(sample_rate),
                error: None,
            })
        }
        Err(e) => {
            error!("failed to start recording: {e}");
            Ok(RecordingResponse {
                success: false,
                state: format!("{:?}", recorder.state()),
                sample_rate: None,
                error: Some(e),
            })
        }
    }
}

/// Stop native mic capture and auto-transcribe the WAV via Whisper.
///
/// Returns the transcription result directly — the frontend doesn't need
/// to handle WAV bytes at all.
#[tauri::command]
pub async fn stop_voice_recording(
    state: State<'_, AppState>,
) -> Result<TranscribeResult, String> {
    // 1. Stop recording → WAV file path
    let wav_path = {
        let mut recorder = state.audio_recorder.lock();
        recorder.stop_recording()?
    };

    info!(path = %wav_path.display(), "recording stopped, transcribing…");

    // 2. Read WAV file bytes
    let audio_bytes = std::fs::read(&wav_path)
        .map_err(|e| format!("failed to read WAV file: {e}"))?;

    // 3. Clean up temp file
    let _ = std::fs::remove_file(&wav_path);

    if audio_bytes.is_empty() {
        return Err("recorded audio is empty".into());
    }

    // 4. Transcribe: extract model info (short lock), create temp engine
    let (models_dir, model) = {
        let guard = state.whisper_engine.read().map_err(|e| format!("lock error: {e}"))?;
        let engine = guard
            .as_ref()
            .ok_or("Whisper engine not initialized. Download a model first.")?;
        (engine.models_dir().to_path_buf(), engine.model())
    };

    let temp_engine = WhisperSttEngine::new(models_dir, model);
    let result = temp_engine
        .transcribe_wav(&audio_bytes)
        .await
        .map_err(|e| format!("transcription failed: {e}"))?;

    Ok(TranscribeResult {
        text: result.text,
        language: result.language,
        duration_ms: result.duration_ms,
        segments: result
            .segments
            .into_iter()
            .map(|s| TranscribeSegment {
                text: s.text,
                start_ms: s.start_ms,
                end_ms: s.end_ms,
            })
            .collect(),
    })
}

/// Cancel an in-progress recording without transcribing.
#[tauri::command]
pub async fn cancel_voice_recording(
    state: State<'_, AppState>,
) -> Result<RecordingResponse, String> {
    let mut recorder = state.audio_recorder.lock();
    recorder.cancel_recording();
    info!("voice recording cancelled");
    Ok(RecordingResponse {
        success: true,
        state: format!("{:?}", recorder.state()),
        sample_rate: None,
        error: None,
    })
}

/// Transcribe WAV audio bytes to text using local Whisper.
///
/// The frontend sends base64-encoded WAV data from the browser's MediaRecorder.
#[tauri::command]
pub async fn transcribe_audio(
    state: State<'_, AppState>,
    audio_base64: String,
) -> Result<TranscribeResult, String> {
    use base64::Engine as _;
    let audio_bytes = base64::engine::general_purpose::STANDARD
        .decode(&audio_base64)
        .map_err(|e| format!("invalid base64 audio: {e}"))?;

    if audio_bytes.is_empty() {
        return Err("empty audio data".into());
    }

    info!(bytes = audio_bytes.len(), "transcribing audio");

    // Extract model info (short lock, no await while holding)
    let (models_dir, model) = {
        let guard = state.whisper_engine.read().map_err(|e| format!("lock error: {e}"))?;
        let engine = guard
            .as_ref()
            .ok_or("Whisper engine not initialized. Download a model first.")?;
        (engine.models_dir().to_path_buf(), engine.model())
    };

    // Create a temporary engine for async transcription (avoids holding lock across await)
    let temp_engine = WhisperSttEngine::new(models_dir, model);
    let result = temp_engine
        .transcribe_wav(&audio_bytes)
        .await
        .map_err(|e| format!("transcription failed: {e}"))?;

    Ok(TranscribeResult {
        text: result.text,
        language: result.language,
        duration_ms: result.duration_ms,
        segments: result
            .segments
            .into_iter()
            .map(|s| TranscribeSegment {
                text: s.text,
                start_ms: s.start_ms,
                end_ms: s.end_ms,
            })
            .collect(),
    })
}

/// List available Whisper models and their download status.
#[tauri::command]
pub async fn get_whisper_models(
    state: State<'_, AppState>,
) -> Result<Vec<WhisperModelStatus>, String> {
    let guard = state.whisper_engine.read().map_err(|e| format!("lock error: {e}"))?;
    match guard.as_ref() {
        Some(engine) => Ok(engine.list_models()),
        None => {
            // Even without an engine, list what's in the default models dir
            let dir = clawdesk_media::whisper::default_models_dir();
            let tmp_engine = WhisperSttEngine::new(dir, WhisperModel::Base);
            Ok(tmp_engine.list_models())
        }
    }
}

/// Download a Whisper model from HuggingFace.
#[tauri::command]
pub async fn download_whisper_model(
    state: State<'_, AppState>,
    model: String,
) -> Result<WhisperModelStatus, String> {
    let whisper_model = parse_model_name(&model)?;
    let models_dir = clawdesk_media::whisper::default_models_dir();

    info!(model = %model, dir = %models_dir.display(), "downloading whisper model");

    let path = WhisperSttEngine::download_model(&models_dir, whisper_model, None)
        .await
        .map_err(|e| format!("download failed: {e}"))?;

    let size_bytes = std::fs::metadata(&path).ok().map(|m| m.len());

    // Re-initialize the engine with the newly downloaded model
    let engine = WhisperSttEngine::new(models_dir, whisper_model);
    {
        let mut guard = state.whisper_engine.write().map_err(|e| format!("lock error: {e}"))?;
        *guard = Some(engine);
    }

    Ok(WhisperModelStatus {
        model: whisper_model,
        downloaded: true,
        path: Some(path.to_string_lossy().to_string()),
        size_bytes,
    })
}

/// Delete a downloaded Whisper model.
#[tauri::command]
pub async fn delete_whisper_model(
    state: State<'_, AppState>,
    model: String,
) -> Result<bool, String> {
    let whisper_model = parse_model_name(&model)?;
    let models_dir = clawdesk_media::whisper::default_models_dir();
    let path = models_dir.join(whisper_model.filename());

    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| format!("failed to delete model: {e}"))?;
        info!(model = %model, "deleted whisper model");

        // Clear engine if the deleted model was the active one
        let should_clear = {
            let guard = state.whisper_engine.read().map_err(|e| format!("lock error: {e}"))?;
            guard.as_ref().map_or(false, |e| e.model() == whisper_model)
        };
        if should_clear {
            let mut guard = state.whisper_engine.write().map_err(|e| format!("lock error: {e}"))?;
            *guard = None;
        }

        Ok(true)
    } else {
        Ok(false)
    }
}

/// Get voice input engine status.
#[tauri::command]
pub async fn get_voice_input_status(
    state: State<'_, AppState>,
) -> Result<VoiceInputStatus, String> {
    let guard = state.whisper_engine.read().map_err(|e| format!("lock error: {e}"))?;
    match guard.as_ref() {
        Some(engine) => Ok(VoiceInputStatus {
            engine_ready: engine.is_model_downloaded(),
            model: format!("{:?}", engine.model()),
            model_downloaded: engine.is_model_downloaded(),
            models_dir: engine.models_dir().to_string_lossy().to_string(),
        }),
        None => {
            let dir = clawdesk_media::whisper::default_models_dir();
            Ok(VoiceInputStatus {
                engine_ready: false,
                model: "none".to_string(),
                model_downloaded: false,
                models_dir: dir.to_string_lossy().to_string(),
            })
        }
    }
}

// ── Helpers ───────────────────────────────────────────────

fn parse_model_name(name: &str) -> Result<WhisperModel, String> {
    match name.to_lowercase().as_str() {
        "tiny" => Ok(WhisperModel::Tiny),
        "base" => Ok(WhisperModel::Base),
        "small" => Ok(WhisperModel::Small),
        "medium" => Ok(WhisperModel::Medium),
        "large" => Ok(WhisperModel::Large),
        _ => Err(format!("unknown model: {name}. Valid: tiny, base, small, medium, large")),
    }
}
