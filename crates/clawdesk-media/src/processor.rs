//! Media processor trait and built-in processor implementations.

use async_trait::async_trait;
use clawdesk_types::media::{MediaData, MediaInput, MediaResult, MediaType};
use tracing::debug;

/// Result of media processing.
pub type ProcessorResult = Result<MediaResult, String>;

/// Trait for media understanding processors.
#[async_trait]
pub trait MediaProcessor: Send + Sync {
    /// Processor name.
    fn name(&self) -> &str;

    /// Which media types this processor supports.
    fn supported_types(&self) -> Vec<MediaType>;

    /// Maximum input size in bytes (0 = unlimited).
    fn max_input_size(&self) -> u64;

    /// Process media input and return a result.
    async fn process(&self, input: &MediaInput) -> ProcessorResult;

    /// Check if the processor is currently available.
    async fn is_available(&self) -> bool;
}

/// OpenAI Whisper audio transcription processor.
pub struct WhisperProcessor {
    name: String,
    api_key: String,
    api_url: String,
    model: String,
    client: reqwest::Client,
}

impl WhisperProcessor {
    /// Create an OpenAI Whisper processor.
    pub fn openai(api_key: &str) -> Self {
        Self {
            name: "openai-whisper".to_string(),
            api_key: api_key.to_string(),
            api_url: "https://api.openai.com/v1/audio/transcriptions".to_string(),
            model: "whisper-1".to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Create a Groq Whisper processor.
    pub fn groq(api_key: &str) -> Self {
        Self {
            name: "groq-whisper".to_string(),
            api_key: api_key.to_string(),
            api_url: "https://api.groq.com/openai/v1/audio/transcriptions".to_string(),
            model: "whisper-large-v3".to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl MediaProcessor for WhisperProcessor {
    fn name(&self) -> &str {
        &self.name
    }

    fn supported_types(&self) -> Vec<MediaType> {
        vec![MediaType::Audio]
    }

    fn max_input_size(&self) -> u64 {
        25 * 1024 * 1024 // 25 MB.
    }

    async fn process(&self, input: &MediaInput) -> ProcessorResult {
        let start = std::time::Instant::now();
        debug!(processor = %self.name, "processing audio");

        let bytes = match &input.data {
            MediaData::Bytes(b) => b.clone(),
            MediaData::FilePath(p) => {
                std::fs::read(p).map_err(|e| format!("read file: {e}"))?
            }
            MediaData::Url(url) => {
                self.client
                    .get(url)
                    .send()
                    .await
                    .map_err(|e| format!("fetch: {e}"))?
                    .bytes()
                    .await
                    .map_err(|e| format!("read bytes: {e}"))?
                    .to_vec()
            }
        };

        let filename = input
            .metadata
            .filename
            .as_deref()
            .unwrap_or("audio.wav")
            .to_string();

        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(filename)
            .mime_str(&input.mime_type)
            .map_err(|e| format!("mime: {e}"))?;

        let form = reqwest::multipart::Form::new()
            .text("model", self.model.clone())
            .part("file", part);

        let resp = self
            .client
            .post(&self.api_url)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(|e| format!("request: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("whisper API error {status}: {body}"));
        }

        let body: serde_json::Value =
            resp.json().await.map_err(|e| format!("parse: {e}"))?;
        let text = body["text"].as_str().unwrap_or("").to_string();

        Ok(MediaResult {
            media_type: MediaType::Audio,
            provider: self.name.clone(),
            text,
            confidence: None,
            processing_ms: start.elapsed().as_millis() as u64,
            estimated_tokens: 0,
            extra: body,
        })
    }

    async fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }
}

/// Vision processor for image understanding (OpenAI/Anthropic).
pub struct VisionProcessor {
    name: String,
    api_key: String,
    api_url: String,
    model: String,
    client: reqwest::Client,
}

impl VisionProcessor {
    /// Create an OpenAI vision processor.
    pub fn openai(api_key: &str) -> Self {
        Self {
            name: "openai-vision".to_string(),
            api_key: api_key.to_string(),
            api_url: "https://api.openai.com/v1/chat/completions".to_string(),
            model: "gpt-4o".to_string(),
            client: reqwest::Client::new(),
        }
    }

    /// Create an Anthropic vision processor.
    pub fn anthropic(api_key: &str) -> Self {
        Self {
            name: "anthropic-vision".to_string(),
            api_key: api_key.to_string(),
            api_url: "https://api.anthropic.com/v1/messages".to_string(),
            model: "claude-sonnet-4-20250514".to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl MediaProcessor for VisionProcessor {
    fn name(&self) -> &str {
        &self.name
    }

    fn supported_types(&self) -> Vec<MediaType> {
        vec![MediaType::Image]
    }

    fn max_input_size(&self) -> u64 {
        20 * 1024 * 1024 // 20 MB.
    }

    async fn process(&self, input: &MediaInput) -> ProcessorResult {
        let start = std::time::Instant::now();
        debug!(processor = %self.name, "processing image");

        let bytes = match &input.data {
            MediaData::Bytes(b) => b.clone(),
            MediaData::FilePath(p) => {
                std::fs::read(p).map_err(|e| format!("read file: {e}"))?
            }
            MediaData::Url(url) => {
                self.client
                    .get(url)
                    .send()
                    .await
                    .map_err(|e| format!("fetch: {e}"))?
                    .bytes()
                    .await
                    .map_err(|e| format!("read bytes: {e}"))?
                    .to_vec()
            }
        };

        use base64::Engine;
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        let data_url = format!("data:{};base64,{}", input.mime_type, b64);

        let body = serde_json::json!({
            "model": self.model,
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "Describe this image in detail."},
                    {"type": "image_url", "image_url": {"url": data_url}},
                ]
            }],
            "max_tokens": 1024
        });

        let resp = self
            .client
            .post(&self.api_url)
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("request: {e}"))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("vision API error {status}: {body}"));
        }

        let resp_body: serde_json::Value =
            resp.json().await.map_err(|e| format!("parse: {e}"))?;

        let text = resp_body["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("")
            .to_string();

        Ok(MediaResult {
            media_type: MediaType::Image,
            provider: self.name.clone(),
            text,
            confidence: None,
            processing_ms: start.elapsed().as_millis() as u64,
            estimated_tokens: 0,
            extra: resp_body,
        })
    }

    async fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }
}
