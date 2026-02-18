//! Audio processing — speech-to-text transcription.
//!
//! Supports Whisper (OpenAI), Google STT, and local whisper.cpp.
//! Handles format detection, chunking for long audio, and language detection.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Audio transcription request.
#[derive(Debug, Clone)]
pub struct TranscriptionRequest {
    /// Raw audio bytes.
    pub data: Vec<u8>,
    /// MIME type (e.g., "audio/mp3", "audio/wav", "audio/ogg").
    pub mime_type: String,
    /// Optional language hint (ISO 639-1 code).
    pub language: Option<String>,
    /// Whether to include word-level timestamps.
    pub timestamps: bool,
}

/// Audio transcription result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionResult {
    pub text: String,
    pub language: Option<String>,
    pub duration: Option<f64>,
    pub segments: Vec<TranscriptionSegment>,
    pub provider: String,
}

/// A segment of transcribed audio with timestamps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionSegment {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// Detect audio format from magic bytes.
pub fn detect_audio_format(data: &[u8]) -> &'static str {
    if data.len() < 4 {
        return "audio/octet-stream";
    }

    // ID3 tag (MP3)
    if data[..3] == [0x49, 0x44, 0x33] || (data[..2] == [0xFF, 0xFB]) {
        return "audio/mpeg";
    }

    // RIFF (WAV)
    if data[..4] == [0x52, 0x49, 0x46, 0x46] && data.len() >= 12 && &data[8..12] == b"WAVE" {
        return "audio/wav";
    }

    // OGG
    if data[..4] == [0x4F, 0x67, 0x67, 0x53] {
        return "audio/ogg";
    }

    // FLAC
    if data[..4] == [0x66, 0x4C, 0x61, 0x43] {
        return "audio/flac";
    }

    // M4A / AAC (ftyp atom)
    if data.len() >= 12 && &data[4..8] == b"ftyp" {
        return "audio/mp4";
    }

    "audio/octet-stream"
}

/// Estimate audio duration from file size and format.
/// Rough estimates for when metadata is unavailable.
pub fn estimate_duration_secs(data: &[u8], mime_type: &str) -> f64 {
    let bytes = data.len() as f64;
    match mime_type {
        "audio/mpeg" => bytes / 16_000.0,          // ~128 kbps
        "audio/wav" => bytes / 176_400.0,           // 44.1kHz 16-bit stereo
        "audio/ogg" => bytes / 12_000.0,            // ~96 kbps
        "audio/flac" => bytes / 88_200.0,           // ~50% of WAV
        _ => bytes / 16_000.0,                      // default estimate
    }
}

/// Maximum audio duration for single-request transcription (25 minutes).
pub const MAX_SINGLE_CHUNK_SECS: f64 = 1500.0;

/// Calculate chunk boundaries for long audio files.
pub fn compute_chunks(total_duration_secs: f64, chunk_size_secs: f64) -> Vec<(f64, f64)> {
    let mut chunks = Vec::new();
    let mut start = 0.0;
    while start < total_duration_secs {
        let end = (start + chunk_size_secs).min(total_duration_secs);
        chunks.push((start, end));
        start = end;
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_mp3() {
        assert_eq!(detect_audio_format(&[0x49, 0x44, 0x33, 0x04]), "audio/mpeg");
    }

    #[test]
    fn detect_wav() {
        let wav = b"RIFF\x00\x00\x00\x00WAVE";
        assert_eq!(detect_audio_format(wav), "audio/wav");
    }

    #[test]
    fn chunk_computation() {
        let chunks = compute_chunks(3700.0, 1500.0);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0], (0.0, 1500.0));
        assert_eq!(chunks[1], (1500.0, 3000.0));
        assert_eq!(chunks[2], (3000.0, 3700.0));
    }

    #[test]
    fn duration_estimate() {
        let one_mb_mp3 = vec![0u8; 1_000_000];
        let duration = estimate_duration_secs(&one_mb_mp3, "audio/mpeg");
        // ~62 seconds for 1 MB MP3 at 128kbps
        assert!(duration > 50.0 && duration < 80.0);
    }
}
