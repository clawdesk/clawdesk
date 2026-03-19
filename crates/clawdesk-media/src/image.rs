//! Image processing — vision API integration, normalization, and quality ladder.
//!
//! Supports multiple vision providers (OpenAI GPT-4V, Anthropic Claude Vision,
//! Google Gemini Vision) with automatic format detection, resizing, and
//! progressive JPEG quality-ladder descent for size-constrained delivery.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

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

// ═══════════════════════════════════════════════════════════════════════════
// Screenshot normalization — Lanczos3 downscaling + JPEG quality ladder
// ═══════════════════════════════════════════════════════════════════════════

/// Configuration for screenshot normalization.
#[derive(Debug, Clone)]
pub struct NormalizeConfig {
    /// Maximum dimension (width or height) in pixels.
    pub max_side: u32,
    /// Maximum output size in bytes.
    pub max_bytes: usize,
    /// JPEG quality steps to try in descending order.
    pub quality_steps: Vec<u8>,
}

impl Default for NormalizeConfig {
    fn default() -> Self {
        Self {
            max_side: 2000,
            max_bytes: 5 * 1024 * 1024, // 5 MB
            quality_steps: vec![60, 40, 25],
        }
    }
}

/// Result of screenshot normalization.
#[derive(Debug, Clone)]
pub struct NormalizedScreenshot {
    /// The normalized image bytes (JPEG or original format if already small enough).
    pub data: Vec<u8>,
    /// MIME type of the normalized image.
    pub mime_type: String,
    /// Original dimensions (width, height).
    pub original_dimensions: (u32, u32),
    /// Normalized dimensions (width, height).
    pub final_dimensions: (u32, u32),
    /// Whether any transformation was applied.
    pub was_transformed: bool,
}

impl NormalizedScreenshot {
    /// Human-readable placeholder string for LLM context injection.
    ///
    /// Returns e.g. `"[screenshot captured: 1920x1080, 340KB]"`
    pub fn placeholder(&self) -> String {
        let kb = self.data.len() / 1024;
        format!(
            "[screenshot captured: {}x{}, {}KB]",
            self.final_dimensions.0, self.final_dimensions.1, kb
        )
    }
}

/// Normalize a screenshot: downscale if oversized, then apply JPEG quality ladder
/// to fit within `config.max_bytes`.
///
/// # Algorithm
/// 1. Decode the input PNG/JPEG/WebP image.
/// 2. If either dimension exceeds `max_side`, downscale using Lanczos3.
/// 3. If the result already fits within `max_bytes` as PNG, return it.
/// 4. Otherwise, encode as JPEG at each quality step until size fits.
///
/// # Complexity
/// - Downscale: O(W·H·(6/r)²) via Lanczos3 kernel, effectively O(W·H).
/// - Quality ladder: at most `quality_steps.len()` JPEG encodes, each O(W·H).
/// - Peak memory: ~70MB for a 5120×2880 Retina screenshot (input + output + encode buffer).
pub fn normalize_screenshot(data: &[u8], config: &NormalizeConfig) -> Result<NormalizedScreenshot, String> {
    use image::imageops::FilterType;
    use image::ImageEncoder;
    use image::ImageFormat;

    // Decode the input image
    let img = image::load_from_memory(data)
        .map_err(|e| format!("failed to decode image: {}", e))?;

    let (orig_w, orig_h) = (img.width(), img.height());
    debug!(width = orig_w, height = orig_h, bytes = data.len(), "normalizing screenshot");

    // Step 1: Downscale if either dimension exceeds max_side
    let img = if orig_w > config.max_side || orig_h > config.max_side {
        let scale = config.max_side as f64 / orig_w.max(orig_h) as f64;
        let new_w = (orig_w as f64 * scale).round() as u32;
        let new_h = (orig_h as f64 * scale).round() as u32;
        debug!(new_w, new_h, "downscaling with Lanczos3");
        img.resize(new_w, new_h, FilterType::Lanczos3)
    } else {
        img
    };

    let (final_w, final_h) = (img.width(), img.height());

    // Step 2: If original data is already small enough and no resize needed, return as-is
    if data.len() <= config.max_bytes && final_w == orig_w && final_h == orig_h {
        let mime = detect_mime_type(data, None);
        return Ok(NormalizedScreenshot {
            data: data.to_vec(),
            mime_type: mime,
            original_dimensions: (orig_w, orig_h),
            final_dimensions: (final_w, final_h),
            was_transformed: false,
        });
    }

    // Step 3: Try encoding as PNG first (lossless, if it fits)
    if final_w <= config.max_side && final_h <= config.max_side {
        let mut png_buf = std::io::Cursor::new(Vec::new());
        if img.write_to(&mut png_buf, ImageFormat::Png).is_ok() {
            let png_data = png_buf.into_inner();
            if png_data.len() <= config.max_bytes {
                debug!(bytes = png_data.len(), "PNG fits within limit");
                return Ok(NormalizedScreenshot {
                    data: png_data,
                    mime_type: "image/png".to_string(),
                    original_dimensions: (orig_w, orig_h),
                    final_dimensions: (final_w, final_h),
                    was_transformed: final_w != orig_w || final_h != orig_h,
                });
            }
        }
    }

    // Step 4: JPEG quality ladder descent
    let rgb_img = img.to_rgb8();
    for &quality in &config.quality_steps {
        let mut jpeg_buf = std::io::Cursor::new(Vec::new());
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_buf, quality);
        if let Err(e) = encoder.write_image(
            rgb_img.as_raw(),
            final_w,
            final_h,
            image::ExtendedColorType::Rgb8,
        ) {
            warn!(quality, error = %e, "JPEG encode failed, trying next quality step");
            continue;
        }
        let jpeg_data = jpeg_buf.into_inner();
        if jpeg_data.len() <= config.max_bytes {
            debug!(quality, bytes = jpeg_data.len(), "JPEG fits within limit");
            return Ok(NormalizedScreenshot {
                data: jpeg_data,
                mime_type: "image/jpeg".to_string(),
                original_dimensions: (orig_w, orig_h),
                final_dimensions: (final_w, final_h),
                was_transformed: true,
            });
        }
        debug!(quality, bytes = jpeg_data.len(), max = config.max_bytes, "JPEG too large, trying lower quality");
    }

    // Step 5: Last resort — encode at lowest quality step (return even if over limit)
    let last_quality = config.quality_steps.last().copied().unwrap_or(25);
    let mut jpeg_buf = std::io::Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_buf, last_quality);
    encoder
        .write_image(
            rgb_img.as_raw(),
            final_w,
            final_h,
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| format!("final JPEG encode failed: {}", e))?;
    let jpeg_data = jpeg_buf.into_inner();
    warn!(
        bytes = jpeg_data.len(),
        max = config.max_bytes,
        "screenshot exceeds max_bytes even at lowest quality"
    );
    Ok(NormalizedScreenshot {
        data: jpeg_data,
        mime_type: "image/jpeg".to_string(),
        original_dimensions: (orig_w, orig_h),
        final_dimensions: (final_w, final_h),
        was_transformed: true,
    })
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

    #[test]
    fn normalize_config_defaults() {
        let config = NormalizeConfig::default();
        assert_eq!(config.max_side, 2000);
        assert_eq!(config.max_bytes, 5 * 1024 * 1024);
        assert_eq!(config.quality_steps, vec![60, 40, 25]);
    }

    #[test]
    fn placeholder_format() {
        let ns = NormalizedScreenshot {
            data: vec![0u8; 340 * 1024],
            mime_type: "image/jpeg".to_string(),
            original_dimensions: (5120, 2880),
            final_dimensions: (1920, 1080),
            was_transformed: true,
        };
        assert_eq!(ns.placeholder(), "[screenshot captured: 1920x1080, 340KB]");
    }
}
