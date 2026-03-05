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

/// FFmpeg-based video processor that shells out to `ffprobe` and `ffmpeg`.
///
/// Falls back to `StubVideoProcessor` behaviour if FFmpeg is not installed.
/// This replaces the previous stub-only implementation.
pub struct FfmpegVideoProcessor;

impl FfmpegVideoProcessor {
    /// Check if FFmpeg is available on the system PATH.
    pub fn is_available() -> bool {
        std::process::Command::new("ffprobe")
            .arg("-version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
}

#[async_trait]
impl VideoProcessor for FfmpegVideoProcessor {
    async fn extract_metadata(&self, data: &[u8]) -> Result<VideoMetadata, VideoError> {
        let format = detect_format_from_header(data);

        // Write data to a temp file for ffprobe
        let tmp_dir = std::env::temp_dir().join("clawdesk-video");
        let _ = std::fs::create_dir_all(&tmp_dir);
        let tmp_path = tmp_dir.join(format!("probe_{}.tmp", uuid::Uuid::new_v4()));
        std::fs::write(&tmp_path, data)
            .map_err(|e| VideoError::ProcessingFailed(format!("write temp: {e}")))?;

        let output = tokio::process::Command::new("ffprobe")
            .args([
                "-v", "quiet",
                "-print_format", "json",
                "-show_format",
                "-show_streams",
            ])
            .arg(&tmp_path)
            .output()
            .await
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                VideoError::ProcessingFailed(format!("ffprobe exec: {e}"))
            })?;

        let _ = std::fs::remove_file(&tmp_path);

        if !output.status.success() {
            // Fall back to header-only detection
            info!(format = ?format, size = data.len(), "ffprobe failed, using header detection");
            return Ok(VideoMetadata {
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
            });
        }

        let json: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| VideoError::ProcessingFailed(format!("parse ffprobe json: {e}")))?;

        // Extract from ffprobe JSON
        let duration = json["format"]["duration"]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        let bitrate = json["format"]["bit_rate"]
            .as_str()
            .and_then(|s| s.parse::<u64>().ok())
            .map(|b| (b / 1000) as u32);

        let streams = json["streams"].as_array();

        let (mut width, mut height, mut fps, mut codec, mut audio_codec, mut has_audio) =
            (0u32, 0u32, 0.0f32, None, None, false);

        if let Some(streams) = streams {
            for stream in streams {
                let codec_type = stream["codec_type"].as_str().unwrap_or("");
                match codec_type {
                    "video" if codec.is_none() => {
                        width = stream["width"].as_u64().unwrap_or(0) as u32;
                        height = stream["height"].as_u64().unwrap_or(0) as u32;
                        codec = stream["codec_name"].as_str().map(String::from);
                        // Parse fps from r_frame_rate (e.g., "30000/1001")
                        if let Some(rate) = stream["r_frame_rate"].as_str() {
                            let parts: Vec<&str> = rate.split('/').collect();
                            if parts.len() == 2 {
                                let num: f32 = parts[0].parse().unwrap_or(0.0);
                                let den: f32 = parts[1].parse().unwrap_or(1.0);
                                if den > 0.0 {
                                    fps = num / den;
                                }
                            }
                        }
                    }
                    "audio" if audio_codec.is_none() => {
                        audio_codec = stream["codec_name"].as_str().map(String::from);
                        has_audio = true;
                    }
                    _ => {}
                }
            }
        }

        info!(
            format = ?format,
            duration,
            width,
            height,
            fps,
            codec = ?codec,
            "ffprobe metadata extracted"
        );

        Ok(VideoMetadata {
            duration_seconds: duration,
            width,
            height,
            fps,
            codec,
            audio_codec,
            bitrate_kbps: bitrate,
            file_size_bytes: data.len() as u64,
            format,
            has_audio,
        })
    }

    async fn extract_frames(
        &self,
        data: &[u8],
        strategy: &FrameStrategy,
    ) -> Result<Vec<VideoFrame>, VideoError> {
        let tmp_dir = std::env::temp_dir().join("clawdesk-video");
        let _ = std::fs::create_dir_all(&tmp_dir);
        let session_id = uuid::Uuid::new_v4();
        let tmp_path = tmp_dir.join(format!("frames_{session_id}.tmp"));
        let out_pattern = tmp_dir.join(format!("frame_{session_id}_%04d.raw"));

        std::fs::write(&tmp_path, data)
            .map_err(|e| VideoError::ProcessingFailed(format!("write temp: {e}")))?;

        // Build ffmpeg args based on strategy
        let mut args = vec![
            "-i".to_string(),
            tmp_path.to_string_lossy().to_string(),
            "-f".to_string(),
            "rawvideo".to_string(),
            "-pix_fmt".to_string(),
            "rgba".to_string(),
        ];

        match strategy {
            FrameStrategy::Thumbnail { position } => {
                // Get metadata first to know duration
                let meta = self.extract_metadata(data).await?;
                let seek = meta.duration_seconds * (*position as f64);
                args.insert(0, "-ss".to_string());
                args.insert(1, format!("{seek:.2}"));
                args.push("-frames:v".to_string());
                args.push("1".to_string());
            }
            FrameStrategy::EvenlySpaced { count } => {
                let meta = self.extract_metadata(data).await?;
                let interval = meta.duration_seconds / (*count as f64);
                args.push("-vf".to_string());
                args.push(format!("fps=1/{interval:.2}"));
                args.push("-frames:v".to_string());
                args.push(count.to_string());
            }
            FrameStrategy::Keyframes { max } => {
                args.push("-vf".to_string());
                args.push("select=eq(pict_type\\,I)".to_string());
                args.push("-vsync".to_string());
                args.push("vfr".to_string());
                args.push("-frames:v".to_string());
                args.push(max.to_string());
            }
            FrameStrategy::EveryNth { n, max } => {
                args.push("-vf".to_string());
                args.push(format!("select=not(mod(n\\,{n}))"));
                args.push("-vsync".to_string());
                args.push("vfr".to_string());
                args.push("-frames:v".to_string());
                args.push(max.to_string());
            }
            FrameStrategy::AtTimestamps(timestamps) => {
                // For specific timestamps, use select filter
                let select_expr: Vec<String> = timestamps
                    .iter()
                    .map(|t| format!("between(t,{t},{:.2})", t + 0.04))
                    .collect();
                args.push("-vf".to_string());
                args.push(format!("select='{}'", select_expr.join("+")));
                args.push("-vsync".to_string());
                args.push("vfr".to_string());
            }
        }

        args.push(out_pattern.to_string_lossy().to_string());

        let output = tokio::process::Command::new("ffmpeg")
            .arg("-y") // overwrite
            .args(&args)
            .output()
            .await
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                VideoError::ProcessingFailed(format!("ffmpeg exec: {e}"))
            })?;

        let _ = std::fs::remove_file(&tmp_path);

        if !output.status.success() {
            return Err(VideoError::ProcessingFailed(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        // Read extracted frames (raw RGBA files)
        let mut frames = Vec::new();
        for i in 1..10000 {
            let frame_path = tmp_dir.join(format!("frame_{session_id}_{i:04}.raw"));
            if !frame_path.exists() {
                break;
            }
            let raw_data = std::fs::read(&frame_path).unwrap_or_default();
            let _ = std::fs::remove_file(&frame_path);
            if !raw_data.is_empty() {
                frames.push(VideoFrame {
                    data: raw_data,
                    width: 0,  // Would need ffprobe to get actual resolution
                    height: 0,
                    timestamp_seconds: i as f64, // Approximate
                });
            }
        }

        if frames.is_empty() {
            return Err(VideoError::NoFrames);
        }

        Ok(frames)
    }

    async fn thumbnail(
        &self,
        data: &[u8],
        width: u32,
        height: u32,
        position: f32,
    ) -> Result<Vec<u8>, VideoError> {
        let tmp_dir = std::env::temp_dir().join("clawdesk-video");
        let _ = std::fs::create_dir_all(&tmp_dir);
        let session_id = uuid::Uuid::new_v4();
        let tmp_path = tmp_dir.join(format!("thumb_{session_id}.tmp"));
        let out_path = tmp_dir.join(format!("thumb_{session_id}.png"));

        std::fs::write(&tmp_path, data)
            .map_err(|e| VideoError::ProcessingFailed(format!("write temp: {e}")))?;

        // Calculate seek position
        let meta = self.extract_metadata(data).await?;
        let seek = meta.duration_seconds * (position as f64);

        let output = tokio::process::Command::new("ffmpeg")
            .args([
                "-y",
                "-ss", &format!("{seek:.2}"),
                "-i", &tmp_path.to_string_lossy(),
                "-vf", &format!("scale={width}:{height}:force_original_aspect_ratio=decrease"),
                "-frames:v", "1",
                "-f", "image2",
            ])
            .arg(&out_path)
            .output()
            .await
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                VideoError::ProcessingFailed(format!("ffmpeg thumbnail: {e}"))
            })?;

        let _ = std::fs::remove_file(&tmp_path);

        if !output.status.success() {
            return Err(VideoError::ProcessingFailed(
                String::from_utf8_lossy(&output.stderr).to_string(),
            ));
        }

        let png_data = std::fs::read(&out_path).unwrap_or_default();
        let _ = std::fs::remove_file(&out_path);

        if png_data.is_empty() {
            return Err(VideoError::NoFrames);
        }

        Ok(png_data)
    }
}

/// Stub video processor for environments without FFmpeg.
/// Returns metadata based on file header detection only.
/// Use `FfmpegVideoProcessor` for full capabilities.
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
        // Stub: FFmpeg not available
        Ok(Vec::new())
    }

    async fn thumbnail(
        &self,
        _data: &[u8],
        _width: u32,
        _height: u32,
        _position: f32,
    ) -> Result<Vec<u8>, VideoError> {
        // Stub: FFmpeg not available
        Ok(Vec::new())
    }
}

/// Create the best available video processor for this environment.
///
/// Returns `FfmpegVideoProcessor` if FFmpeg is installed, otherwise
/// falls back to `StubVideoProcessor` with header-only detection.
pub fn create_video_processor() -> Box<dyn VideoProcessor> {
    if FfmpegVideoProcessor::is_available() {
        info!("FFmpeg detected — using FfmpegVideoProcessor for video processing");
        Box::new(FfmpegVideoProcessor)
    } else {
        info!("FFmpeg not found — using StubVideoProcessor (metadata only)");
        Box::new(StubVideoProcessor)
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
