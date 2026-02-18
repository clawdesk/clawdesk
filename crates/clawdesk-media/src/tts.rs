//! Text-to-speech synthesis — OpenAI TTS, ElevenLabs, and local adapters.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// TTS voice configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceConfig {
    pub voice_id: String,
    pub model: String,
    pub speed: f32,
    pub format: AudioOutputFormat,
}

/// Output audio format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudioOutputFormat {
    Mp3,
    Opus,
    Aac,
    Flac,
    Wav,
    Pcm,
}

impl AudioOutputFormat {
    pub fn content_type(&self) -> &'static str {
        match self {
            Self::Mp3 => "audio/mpeg",
            Self::Opus => "audio/ogg",
            Self::Aac => "audio/aac",
            Self::Flac => "audio/flac",
            Self::Wav => "audio/wav",
            Self::Pcm => "audio/pcm",
        }
    }

    pub fn extension(&self) -> &'static str {
        match self {
            Self::Mp3 => "mp3",
            Self::Opus => "ogg",
            Self::Aac => "aac",
            Self::Flac => "flac",
            Self::Wav => "wav",
            Self::Pcm => "pcm",
        }
    }
}

/// TTS synthesis request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SynthesisRequest {
    pub text: String,
    pub voice: VoiceConfig,
    pub provider: TtsProvider,
}

/// Supported TTS providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TtsProvider {
    OpenAI,
    ElevenLabs,
}

/// TTS synthesis result.
#[derive(Debug, Clone)]
pub struct SynthesisResult {
    pub audio_data: Vec<u8>,
    pub format: AudioOutputFormat,
    pub duration_secs: Option<f64>,
    pub characters_used: usize,
}

impl Default for VoiceConfig {
    fn default() -> Self {
        Self {
            voice_id: "alloy".to_string(),
            model: "tts-1".to_string(),
            speed: 1.0,
            format: AudioOutputFormat::Mp3,
        }
    }
}

/// Available voices for each provider.
pub fn available_voices(provider: TtsProvider) -> Vec<VoiceInfo> {
    match provider {
        TtsProvider::OpenAI => vec![
            VoiceInfo::new("alloy", "Alloy", "Neutral, balanced"),
            VoiceInfo::new("echo", "Echo", "Warm, conversational"),
            VoiceInfo::new("fable", "Fable", "Expressive, British"),
            VoiceInfo::new("onyx", "Onyx", "Deep, authoritative"),
            VoiceInfo::new("nova", "Nova", "Friendly, youthful"),
            VoiceInfo::new("shimmer", "Shimmer", "Clear, refined"),
        ],
        TtsProvider::ElevenLabs => vec![
            VoiceInfo::new("rachel", "Rachel", "Calm, narrative"),
            VoiceInfo::new("adam", "Adam", "Deep, narrator"),
            VoiceInfo::new("antoni", "Antoni", "Well-rounded"),
            VoiceInfo::new("bella", "Bella", "Soft, gentle"),
        ],
    }
}

/// Voice metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceInfo {
    pub id: String,
    pub name: String,
    pub description: String,
}

impl VoiceInfo {
    pub fn new(id: &str, name: &str, desc: &str) -> Self {
        Self {
            id: id.to_string(),
            name: name.to_string(),
            description: desc.to_string(),
        }
    }
}

/// Build an OpenAI TTS request body.
pub fn build_openai_tts_body(req: &SynthesisRequest) -> serde_json::Value {
    serde_json::json!({
        "model": req.voice.model,
        "input": req.text,
        "voice": req.voice.voice_id,
        "response_format": req.voice.format.extension(),
        "speed": req.voice.speed,
    })
}

/// Build an ElevenLabs TTS request body.
pub fn build_elevenlabs_tts_body(req: &SynthesisRequest) -> serde_json::Value {
    serde_json::json!({
        "text": req.text,
        "model_id": "eleven_monolingual_v1",
        "voice_settings": {
            "stability": 0.5,
            "similarity_boost": 0.5,
        }
    })
}

/// Estimate character cost for a synthesis.
pub fn estimate_cost(text: &str, provider: TtsProvider) -> CostEstimate {
    let chars = text.chars().count();
    let cost_per_char = match provider {
        TtsProvider::OpenAI => 0.000015, // $15 per 1M chars
        TtsProvider::ElevenLabs => 0.00003, // $30 per 1M chars
    };

    CostEstimate {
        characters: chars,
        estimated_usd: chars as f64 * cost_per_char,
    }
}

/// Cost estimate.
#[derive(Debug, Clone)]
pub struct CostEstimate {
    pub characters: usize,
    pub estimated_usd: f64,
}

/// Synthesize speech via the configured TTS provider.
///
/// Makes an HTTP request to the appropriate API and returns raw audio bytes.
pub async fn synthesize(
    req: &SynthesisRequest,
    api_key: &str,
) -> Result<SynthesisResult, String> {
    let client = reqwest::Client::new();

    match req.provider {
        TtsProvider::OpenAI => synthesize_openai(&client, req, api_key).await,
        TtsProvider::ElevenLabs => synthesize_elevenlabs(&client, req, api_key).await,
    }
}

/// OpenAI TTS synthesis.
async fn synthesize_openai(
    client: &reqwest::Client,
    req: &SynthesisRequest,
    api_key: &str,
) -> Result<SynthesisResult, String> {
    let body = build_openai_tts_body(req);

    let resp = client
        .post("https://api.openai.com/v1/audio/speech")
        .bearer_auth(api_key)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("OpenAI TTS request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("OpenAI TTS API error {status}: {body}"));
    }

    let audio_data = resp
        .bytes()
        .await
        .map_err(|e| format!("read response bytes: {e}"))?
        .to_vec();

    Ok(SynthesisResult {
        audio_data,
        format: req.voice.format,
        duration_secs: None, // Would need to parse audio headers
        characters_used: req.text.chars().count(),
    })
}

/// ElevenLabs TTS synthesis.
async fn synthesize_elevenlabs(
    client: &reqwest::Client,
    req: &SynthesisRequest,
    api_key: &str,
) -> Result<SynthesisResult, String> {
    let body = build_elevenlabs_tts_body(req);
    let url = format!(
        "https://api.elevenlabs.io/v1/text-to-speech/{}",
        req.voice.voice_id
    );

    let resp = client
        .post(&url)
        .header("xi-api-key", api_key)
        .header("Accept", req.voice.format.content_type())
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("ElevenLabs TTS request failed: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!("ElevenLabs TTS API error {status}: {body}"));
    }

    let audio_data = resp
        .bytes()
        .await
        .map_err(|e| format!("read response bytes: {e}"))?
        .to_vec();

    Ok(SynthesisResult {
        audio_data,
        format: req.voice.format,
        duration_secs: None,
        characters_used: req.text.chars().count(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_voice_config() {
        let cfg = VoiceConfig::default();
        assert_eq!(cfg.voice_id, "alloy");
        assert_eq!(cfg.speed, 1.0);
        assert_eq!(cfg.format, AudioOutputFormat::Mp3);
    }

    #[test]
    fn openai_voices_available() {
        let voices = available_voices(TtsProvider::OpenAI);
        assert_eq!(voices.len(), 6);
        assert!(voices.iter().any(|v| v.id == "alloy"));
    }

    #[test]
    fn build_openai_body() {
        let req = SynthesisRequest {
            text: "Hello world".to_string(),
            voice: VoiceConfig::default(),
            provider: TtsProvider::OpenAI,
        };
        let body = build_openai_tts_body(&req);
        assert_eq!(body["input"], "Hello world");
        assert_eq!(body["voice"], "alloy");
    }

    #[test]
    fn cost_estimation() {
        let est = estimate_cost("Hello world", TtsProvider::OpenAI);
        assert_eq!(est.characters, 11);
        assert!(est.estimated_usd > 0.0);
    }

    #[test]
    fn audio_format_content_types() {
        assert_eq!(AudioOutputFormat::Mp3.content_type(), "audio/mpeg");
        assert_eq!(AudioOutputFormat::Opus.extension(), "ogg");
    }
}
