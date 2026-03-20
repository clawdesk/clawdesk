//! Media directive parser — extracts `MEDIA:` prefixed lines from tool output.
//!
//! When a tool (e.g., `browser_screenshot`, `generate_image`) produces media,
//! it emits a `MEDIA:<path>` line in its output. This module parses those lines,
//! separating the text content from the media references.
//!
//! ## Format
//! ```text
//! MEDIA:/tmp/clawdesk/screenshots/abc123.png
//! MEDIA:data:image/png;base64,iVBOR...
//! MEDIA:/tmp/clawdesk/audio/narration.mp3 VOICE
//! ```
//!
//! The optional `VOICE` suffix marks audio media for voice-message delivery
//! on channels that support it (Telegram voice notes, Discord voice messages).
//!
//! ## Complexity
//! O(N) total for N lines, O(1) per line.

/// Result of splitting text content from media directives.
#[derive(Debug, Clone, Default)]
pub struct MediaSplit {
    /// Text content with MEDIA: lines removed.
    pub text: String,
    /// Extracted media URLs / file paths.
    pub media_urls: Vec<String>,
    /// Whether any extracted audio should be sent as a voice message.
    pub audio_as_voice: bool,
}

/// Valid media path prefixes — prevents arbitrary path injection.
const VALID_PREFIXES: &[&str] = &[
    "/tmp/clawdesk/",
    "/tmp/clawdesk-",
    "data:image/",
    "data:audio/",
    "data:video/",
    "data:application/pdf",
    "https://",
    "http://localhost",
];

/// Parse media directives from tool output, separating text from media references.
///
/// # Security
/// Only paths matching `VALID_PREFIXES` are accepted. Arbitrary file paths
/// are rejected to prevent path traversal attacks.
pub fn parse_media_directives(content: &str) -> MediaSplit {
    let mut result = MediaSplit::default();
    let mut text_lines = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(media_ref) = trimmed.strip_prefix("MEDIA:") {
            let media_ref = media_ref.trim();
            // Check for VOICE suffix
            let (path, is_voice) = if media_ref.ends_with(" VOICE") {
                (&media_ref[..media_ref.len() - 6], true)
            } else {
                (media_ref, false)
            };

            // Validate path against allowed prefixes
            if is_valid_media_path(path) {
                result.media_urls.push(path.to_string());
                if is_voice {
                    result.audio_as_voice = true;
                }
            } else {
                // Invalid path — keep as text for debugging
                text_lines.push(line);
            }
        } else {
            text_lines.push(line);
        }
    }

    result.text = text_lines.join("\n");
    result
}

/// Check if a media path is from a trusted source.
fn is_valid_media_path(path: &str) -> bool {
    VALID_PREFIXES.iter().any(|prefix| path.starts_with(prefix))
}

/// Convert media URL strings to typed `MediaAttachment` structs for channel delivery.
///
/// Delegates to [`clawdesk_types::message::media_urls_to_attachments`] — the
/// canonical implementation. This re-export preserves backward compatibility.
pub fn media_urls_to_attachments(urls: &[String]) -> Vec<clawdesk_types::message::MediaAttachment> {
    clawdesk_types::message::media_urls_to_attachments(urls)
}

/// Infer MIME type from file path extension.
fn mime_from_path(path: &str) -> String {
    let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "mp3" => "audio/mpeg",
        "ogg" => "audio/ogg",
        "wav" => "audio/wav",
        "mp4" => "video/mp4",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_no_media() {
        let split = parse_media_directives("Hello, here is your response.");
        assert!(split.media_urls.is_empty());
        assert_eq!(split.text, "Hello, here is your response.");
        assert!(!split.audio_as_voice);
    }

    #[test]
    fn parse_with_screenshot() {
        let input = "Screenshot taken.\nMEDIA:/tmp/clawdesk/screenshots/abc.png\nDone.";
        let split = parse_media_directives(input);
        assert_eq!(split.media_urls, vec!["/tmp/clawdesk/screenshots/abc.png"]);
        assert_eq!(split.text, "Screenshot taken.\nDone.");
    }

    #[test]
    fn parse_voice_audio() {
        let input = "MEDIA:/tmp/clawdesk/audio/out.mp3 VOICE";
        let split = parse_media_directives(input);
        assert_eq!(split.media_urls, vec!["/tmp/clawdesk/audio/out.mp3"]);
        assert!(split.audio_as_voice);
    }

    #[test]
    fn reject_invalid_path() {
        let input = "MEDIA:/etc/passwd";
        let split = parse_media_directives(input);
        assert!(split.media_urls.is_empty());
        assert_eq!(split.text, "MEDIA:/etc/passwd");
    }

    #[test]
    fn data_uri_accepted() {
        let input = "MEDIA:data:image/png;base64,iVBORw0KGgo=";
        let split = parse_media_directives(input);
        assert_eq!(split.media_urls.len(), 1);
        assert!(split.media_urls[0].starts_with("data:image/png"));
    }
}
