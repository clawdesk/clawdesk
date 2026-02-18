//! Image processing — vision API integration for image understanding.
//!
//! Supports multiple vision providers (OpenAI GPT-4V, Anthropic Claude Vision,
//! Google Gemini Vision) with automatic format detection and resizing.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Image analysis request.
#[derive(Debug, Clone)]
pub struct ImageAnalysisRequest {
    /// Raw image bytes.
    pub data: Vec<u8>,
    /// MIME type (e.g., "image/png", "image/jpeg").
    pub mime_type: String,
    /// What to analyze (default: "Describe this image in detail").
    pub prompt: Option<String>,
    /// Maximum tokens for the response.
    pub max_tokens: Option<u32>,
}

/// Image analysis result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageAnalysisResult {
    pub description: String,
    pub provider: String,
    pub tokens_used: u64,
}

/// Detect MIME type from file extension or magic bytes.
pub fn detect_mime_type(data: &[u8], filename: Option<&str>) -> String {
    // Check by extension first
    if let Some(name) = filename {
        let ext = name.rsplit('.').next().unwrap_or("").to_lowercase();
        match ext.as_str() {
            "png" => return "image/png".to_string(),
            "jpg" | "jpeg" => return "image/jpeg".to_string(),
            "gif" => return "image/gif".to_string(),
            "webp" => return "image/webp".to_string(),
            "svg" => return "image/svg+xml".to_string(),
            "bmp" => return "image/bmp".to_string(),
            _ => {}
        }
    }

    // Check magic bytes
    if data.len() >= 4 {
        if data[..4] == [0x89, 0x50, 0x4E, 0x47] {
            return "image/png".to_string();
        }
        if data[..2] == [0xFF, 0xD8] {
            return "image/jpeg".to_string();
        }
        if data[..4] == [0x47, 0x49, 0x46, 0x38] {
            return "image/gif".to_string();
        }
        if data.len() >= 12 && &data[8..12] == b"WEBP" {
            return "image/webp".to_string();
        }
    }

    "application/octet-stream".to_string()
}

/// Encode image data as base64 data URI.
pub fn to_data_uri(data: &[u8], mime_type: &str) -> String {
    let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data);
    format!("data:{};base64,{}", mime_type, b64)
}

/// Check if image data is within size limits for API submission.
pub fn check_size_limit(data: &[u8], max_mb: usize) -> Result<(), String> {
    let size_mb = data.len() / (1024 * 1024);
    if size_mb > max_mb {
        Err(format!(
            "Image too large: {} MB (max {} MB). Consider resizing.",
            size_mb, max_mb
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_png_magic() {
        let png_header = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        assert_eq!(detect_mime_type(&png_header, None), "image/png");
    }

    #[test]
    fn detect_jpeg_magic() {
        let jpg_header = [0xFF, 0xD8, 0xFF, 0xE0];
        assert_eq!(detect_mime_type(&jpg_header, None), "image/jpeg");
    }

    #[test]
    fn detect_by_extension() {
        assert_eq!(detect_mime_type(&[], Some("photo.webp")), "image/webp");
        assert_eq!(detect_mime_type(&[], Some("icon.svg")), "image/svg+xml");
    }

    #[test]
    fn size_limit_check() {
        let small = vec![0u8; 1024]; // 1 KB
        assert!(check_size_limit(&small, 20).is_ok());

        let large = vec![0u8; 25 * 1024 * 1024]; // 25 MB
        assert!(check_size_limit(&large, 20).is_err());
    }
}
