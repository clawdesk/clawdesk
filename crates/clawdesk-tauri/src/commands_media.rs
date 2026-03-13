//! Media processing commands — audio, image, document, link preview, TTS.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;
use tracing::info;

#[derive(Debug, Serialize)]
pub struct LinkPreviewResult {
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub image: Option<String>,
    pub site_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MediaPipelineStatus {
    pub processor_count: usize,
    pub processors: Vec<String>,
}

/// Get the status of the media processing pipeline.
#[tauri::command]
pub async fn get_media_pipeline_status(
    state: State<'_, AppState>,
) -> Result<MediaPipelineStatus, String> {
    let pipeline = state.media_pipeline.read().await;
    let processors: Vec<String> = pipeline.processors().iter()
        .map(|p| p.name().to_string())
        .collect();
    Ok(MediaPipelineStatus {
        processor_count: processors.len(),
        processors,
    })
}

/// Get a rich preview for a URL.
///
/// Note: Requires an HTTP fetcher to be configured. Currently returns
/// extracted URL metadata placeholder until a production fetcher is wired.
#[tauri::command]
pub async fn get_link_preview(url: String) -> Result<LinkPreviewResult, String> {
    // LinkUnderstanding requires an Arc<dyn HttpFetcher> which needs a production
    // HTTP client. For now, return the URL as-is with empty metadata.
    // In production, wire a reqwest-based HttpFetcher implementation.
    Ok(LinkPreviewResult {
        url,
        title: None,
        description: None,
        image: None,
        site_name: None,
    })
}

// ── TTS ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TtsSynthesizeRequest {
    pub text: String,
    /// "openai" or "elevenlabs"
    #[serde(default = "default_tts_provider")]
    pub provider: String,
    #[serde(default = "default_voice")]
    pub voice: String,
    #[serde(default = "default_speed")]
    pub speed: f32,
}

fn default_tts_provider() -> String { "openai".into() }
fn default_voice() -> String { "alloy".into() }
fn default_speed() -> f32 { 1.0 }

#[derive(Debug, Serialize)]
pub struct TtsSynthesizeResult {
    /// Base64-encoded audio data
    pub audio_base64: String,
    pub format: String,
    pub characters: usize,
    pub estimated_cost_usd: f64,
}

/// Synthesize text to speech via OpenAI or ElevenLabs.
///
/// Returns base64-encoded audio data that the frontend can play directly.
#[tauri::command]
pub async fn tts_synthesize(
    request: TtsSynthesizeRequest,
    state: State<'_, AppState>,
) -> Result<TtsSynthesizeResult, String> {
    use clawdesk_media::tts::*;

    let provider = match request.provider.to_lowercase().as_str() {
        "elevenlabs" => TtsProvider::ElevenLabs,
        _ => TtsProvider::OpenAI,
    };

    // Resolve the API key from environment variables
    let api_key = match provider {
        TtsProvider::OpenAI => std::env::var("OPENAI_API_KEY")
            .map_err(|_| "OPENAI_API_KEY not set — required for OpenAI TTS".to_string())?,
        TtsProvider::ElevenLabs => std::env::var("ELEVENLABS_API_KEY")
            .map_err(|_| "ELEVENLABS_API_KEY not set — required for ElevenLabs TTS".to_string())?,
    };

    let voice_config = VoiceConfig {
        voice_id: request.voice,
        model: match provider {
            TtsProvider::OpenAI => "tts-1".to_string(),
            TtsProvider::ElevenLabs => "eleven_monolingual_v1".to_string(),
        },
        speed: request.speed,
        format: AudioOutputFormat::Mp3,
    };

    let synth_request = SynthesisRequest {
        text: request.text.clone(),
        voice: voice_config,
        provider,
    };

    let cost = estimate_cost(&request.text, provider);

    info!(
        provider = ?provider,
        chars = cost.characters,
        estimated_usd = cost.estimated_usd,
        "TTS synthesis requested"
    );

    let result = synthesize(&synth_request, &api_key)
        .await
        .map_err(|err| err.to_string())?;

    use base64::Engine;
    let audio_base64 = base64::engine::general_purpose::STANDARD.encode(&result.audio_data);

    Ok(TtsSynthesizeResult {
        audio_base64,
        format: result.format.extension().to_string(),
        characters: result.characters_used,
        estimated_cost_usd: cost.estimated_usd,
    })
}

/// List available TTS voices for a given provider.
#[tauri::command]
pub async fn tts_list_voices(
    provider: Option<String>,
) -> Result<Vec<serde_json::Value>, String> {
    use clawdesk_media::tts::*;

    let prov = match provider.as_deref() {
        Some("elevenlabs") => TtsProvider::ElevenLabs,
        _ => TtsProvider::OpenAI,
    };

    let voices: Vec<serde_json::Value> = available_voices(prov)
        .into_iter()
        .map(|v| serde_json::json!({
            "id": v.id,
            "name": v.name,
            "description": v.description,
        }))
        .collect();

    Ok(voices)
}
