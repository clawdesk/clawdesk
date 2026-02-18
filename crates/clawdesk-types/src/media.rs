//! Media types — audio, video, image, document understanding.

use serde::{Deserialize, Serialize};

/// Supported media types for understanding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MediaType {
    Audio,
    Video,
    Image,
    Document,
}

/// Media understanding provider identifier.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MediaProvider {
    Deepgram,
    OpenAiWhisper,
    GoogleSpeech,
    GroqWhisper,
    GoogleVision,
    LocalWhisper,
}

/// Input for media processing.
#[derive(Debug, Clone)]
pub struct MediaInput {
    pub media_type: MediaType,
    pub mime_type: String,
    pub data: MediaData,
    pub metadata: MediaMetadata,
}

/// How media data is provided.
#[derive(Debug, Clone)]
pub enum MediaData {
    /// Raw bytes in memory.
    Bytes(Vec<u8>),
    /// Path to file on disk.
    FilePath(String),
    /// URL to fetch.
    Url(String),
}

/// Metadata about the media.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MediaMetadata {
    pub filename: Option<String>,
    pub size_bytes: Option<u64>,
    pub duration_secs: Option<f64>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub language: Option<String>,
}

/// Result of media understanding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaResult {
    pub media_type: MediaType,
    pub provider: String,
    /// Transcription or description text.
    pub text: String,
    /// Confidence score (0.0 - 1.0).
    pub confidence: Option<f64>,
    /// Processing time in milliseconds.
    pub processing_ms: u64,
    /// Token equivalent of the result.
    pub estimated_tokens: usize,
    /// Additional structured data.
    pub extra: serde_json::Value,
}

/// Media provider quality metrics (for adaptive selection).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MediaProviderMetrics {
    pub total_requests: u64,
    pub successes: u64,
    pub failures: u64,
    pub avg_latency_ms: f64,
    pub avg_quality_score: f64,
    pub cost_per_minute: f64,
}

/// Media processing concurrency config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaConcurrencyConfig {
    pub audio_slots: usize,
    pub video_slots: usize,
    pub image_slots: usize,
    pub document_slots: usize,
    pub stream_buffer_bytes: usize,
}

impl Default for MediaConcurrencyConfig {
    fn default() -> Self {
        Self {
            audio_slots: 4,
            video_slots: 2,
            image_slots: 8,
            document_slots: 4,
            stream_buffer_bytes: 1024 * 1024, // 1MB
        }
    }
}
