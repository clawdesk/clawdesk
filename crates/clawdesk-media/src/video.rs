//! Video processing — frame extraction, thumbnail generation, and video metadata.
//!
//! Extends the media pipeline with video understanding capabilities:
//! - Thumbnail extraction from key frames
//! - Duration/resolution/codec metadata
//! - Frame sampling for vision API analysis
//! - Video format detection

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::info;

/// Video metadata extracted from a video file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoMetadata {
    pub duration_seconds: f64,
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    pub codec: Option<String>,
    pub audio_codec: Option<String>,
    pub bitrate_kbps: Option<u32>,
    pub file_size_bytes: u64,
    pub format: VideoFormat,
    pub has_audio: bool,
}

/// Known video formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VideoFormat {
    Mp4,
    Webm,
    Mkv,
    Avi,
    Mov,
    Flv,
    Unknown,
}

impl VideoFormat {
    pub fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "mp4" | "m4v" => VideoFormat::Mp4,
            "webm" => VideoFormat::Webm,
            "mkv" => VideoFormat::Mkv,
            "avi" => VideoFormat::Avi,
            "mov" => VideoFormat::Mov,
            "flv" => VideoFormat::Flv,
            _ => VideoFormat::Unknown,
        }
    }

    pub fn from_mime(mime: &str) -> Self {
        let lower = mime.to_lowercase();
        if lower.contains("mp4") { VideoFormat::Mp4 }
        else if lower.contains("webm") { VideoFormat::Webm }
        else if lower.contains("matroska") || lower.contains("mkv") { VideoFormat::Mkv }
        else if lower.contains("avi") { VideoFormat::Avi }
        else if lower.contains("quicktime") || lower.contains("mov") { VideoFormat::Mov }
        else if lower.contains("flv") || lower.contains("flash") { VideoFormat::Flv }
        else { VideoFormat::Unknown }
    }
}

/// A single extracted frame.
#[derive(Debug, Clone)]
pub struct VideoFrame {
    /// Raw RGBA pixel data.
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Timestamp in the video (seconds).
    pub timestamp_seconds: f64,
}

/// Frame extraction strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameStrategy {
    /// Extract frame at specific timestamps.
    AtTimestamps(Vec<f64>),
    /// Extract N evenly-spaced frames.
    EvenlySpaced { count: usize },
    /// Extract keyframes (I-frames) only.
    Keyframes { max: usize },
    /// Extract every N-th frame.
    EveryNth { n: usize, max: usize },
    /// Single thumbnail at given position (0.0-1.0 of duration).
    Thumbnail { position: f32 },
}

impl Default for FrameStrategy {
    fn default() -> Self {
        FrameStrategy::Thumbnail { position: 0.25 }
    }
}

/// Video processing configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoConfig {
    /// Maximum video file size to process (bytes).
    pub max_file_size: u64,
    /// Maximum duration to process (seconds).
    pub max_duration_seconds: f64,
    /// Output thumbnail dimensions.
    pub thumbnail_width: u32,
    pub thumbnail_height: u32,
    /// Default frame extraction strategy.
    pub default_strategy: FrameStrategy,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            max_file_size: 500 * 1024 * 1024, // 500 MB
            max_duration_seconds: 3600.0,       // 1 hour
            thumbnail_width: 320,
            thumbnail_height: 240,
            default_strategy: FrameStrategy::default(),
        }
    }
}

/// Trait for video processing backends.
///
/// Implementations would wrap FFmpeg, GStreamer, or cloud video APIs.
#[async_trait]
pub trait VideoProcessor: Send + Sync {
    /// Extract metadata from video data.
    async fn extract_metadata(&self, data: &[u8]) -> Result<VideoMetadata, VideoError>;

    /// Extract frames according to strategy.
    async fn extract_frames(
        &self,
        data: &[u8],
        strategy: &FrameStrategy,
    ) -> Result<Vec<VideoFrame>, VideoError>;

    /// Generate a thumbnail (convenience method).
    async fn thumbnail(
        &self,
        data: &[u8],
        width: u32,
        height: u32,
        position: f32,
    ) -> Result<Vec<u8>, VideoError>;
}

/// Video processing result for agent consumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoAnalysis {
    pub metadata: VideoMetadata,
    pub frame_descriptions: Vec<FrameDescription>,
    pub summary: Option<String>,
}

/// Description of an extracted frame (from vision API).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameDescription {
    pub timestamp_seconds: f64,
    pub description: String,
    pub labels: Vec<String>,
    pub confidence: f32,
}

/// Video processing error.
#[derive(Debug, thiserror::Error)]
pub enum VideoError {
    #[error("file too large: {size} bytes (max {max})")]
    FileTooLarge { size: u64, max: u64 },
    #[error("unsupported format: {0}")]
    UnsupportedFormat(String),
    #[error("processing failed: {0}")]
    ProcessingFailed(String),
    #[error("no frames extracted")]
    NoFrames,
    #[error("duration too long: {duration}s (max {max}s)")]
    DurationTooLong { duration: f64, max: f64 },
    #[error("codec error: {0}")]
    CodecError(String),
}

/// Stub video processor for environments without FFmpeg.
/// Returns metadata based on file header detection.
pub struct StubVideoProcessor;

#[async_trait]
impl VideoProcessor for StubVideoProcessor {
    async fn extract_metadata(&self, data: &[u8]) -> Result<VideoMetadata, VideoError> {
        let format = detect_format_from_header(data);
        info!(format = ?format, size = data.len(), "stub video metadata extraction");

        Ok(VideoMetadata {
            duration_seconds: 0.0,
            width: 0,
            height: 0,
            fps: 0.0,
            codec: None,
            audio_codec: None,
            bitrate_kbps: None,
            file_size_bytes: data.len() as u64,
            format,
            has_audio: false,
        })
    }

    async fn extract_frames(
        &self,
        _data: &[u8],
        _strategy: &FrameStrategy,
    ) -> Result<Vec<VideoFrame>, VideoError> {
        // Stub: would call FFmpeg
        Ok(Vec::new())
    }

    async fn thumbnail(
        &self,
        _data: &[u8],
        _width: u32,
        _height: u32,
        _position: f32,
    ) -> Result<Vec<u8>, VideoError> {
        // Stub: would call FFmpeg
        Ok(Vec::new())
    }
}

/// Detect video format from file header magic bytes.
fn detect_format_from_header(data: &[u8]) -> VideoFormat {
    if data.len() < 12 {
        return VideoFormat::Unknown;
    }

    // MP4: ftyp box at offset 4
    if data.len() >= 8 && &data[4..8] == b"ftyp" {
        return VideoFormat::Mp4;
    }

    // WebM: EBML header (0x1A45DFA3)
    if data.len() >= 4 && data[0] == 0x1A && data[1] == 0x45 && data[2] == 0xDF && data[3] == 0xA3
    {
        return VideoFormat::Webm;
    }

    // AVI: RIFF....AVI
    if data.len() >= 12
        && &data[0..4] == b"RIFF"
        && &data[8..12] == b"AVI "
    {
        return VideoFormat::Avi;
    }

    // FLV: FLV header
    if data.len() >= 3 && &data[0..3] == b"FLV" {
        return VideoFormat::Flv;
    }

    VideoFormat::Unknown
}

// ── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_video_format_from_extension() {
        assert_eq!(VideoFormat::from_extension("mp4"), VideoFormat::Mp4);
        assert_eq!(VideoFormat::from_extension("webm"), VideoFormat::Webm);
        assert_eq!(VideoFormat::from_extension("MKV"), VideoFormat::Mkv);
        assert_eq!(VideoFormat::from_extension("xyz"), VideoFormat::Unknown);
    }

    #[test]
    fn test_video_format_from_mime() {
        assert_eq!(VideoFormat::from_mime("video/mp4"), VideoFormat::Mp4);
        assert_eq!(VideoFormat::from_mime("video/webm"), VideoFormat::Webm);
        assert_eq!(
            VideoFormat::from_mime("video/quicktime"),
            VideoFormat::Mov
        );
    }

    #[test]
    fn test_detect_mp4() {
        let mut data = vec![0u8; 12];
        data[4] = b'f';
        data[5] = b't';
        data[6] = b'y';
        data[7] = b'p';
        assert_eq!(detect_format_from_header(&data), VideoFormat::Mp4);
    }

    #[test]
    fn test_detect_webm() {
        let data = vec![0x1A, 0x45, 0xDF, 0xA3, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(detect_format_from_header(&data), VideoFormat::Webm);
    }

    #[test]
    fn test_detect_avi() {
        let mut data = vec![0u8; 12];
        data[0..4].copy_from_slice(b"RIFF");
        data[8..12].copy_from_slice(b"AVI ");
        assert_eq!(detect_format_from_header(&data), VideoFormat::Avi);
    }

    #[test]
    fn test_detect_unknown() {
        let data = vec![0u8; 12];
        assert_eq!(detect_format_from_header(&data), VideoFormat::Unknown);
    }

    #[tokio::test]
    async fn test_stub_processor() {
        let proc = StubVideoProcessor;
        let data = vec![0u8; 100];
        let meta = proc.extract_metadata(&data).await.unwrap();
        assert_eq!(meta.file_size_bytes, 100);
    }

    #[test]
    fn test_frame_strategy_default() {
        let strategy = FrameStrategy::default();
        match strategy {
            FrameStrategy::Thumbnail { position } => assert!((position - 0.25).abs() < 0.01),
            _ => panic!("unexpected default strategy"),
        }
    }

    #[test]
    fn test_video_config_default() {
        let config = VideoConfig::default();
        assert_eq!(config.max_file_size, 500 * 1024 * 1024);
        assert_eq!(config.thumbnail_width, 320);
    }

    #[test]
    fn test_metadata_serde() {
        let meta = VideoMetadata {
            duration_seconds: 120.5,
            width: 1920,
            height: 1080,
            fps: 30.0,
            codec: Some("h264".to_string()),
            audio_codec: Some("aac".to_string()),
            bitrate_kbps: Some(5000),
            file_size_bytes: 10_000_000,
            format: VideoFormat::Mp4,
            has_audio: true,
        };
        let json = serde_json::to_string(&meta).unwrap();
        let parsed: VideoMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.width, 1920);
        assert_eq!(parsed.format, VideoFormat::Mp4);
    }
}
