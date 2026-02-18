//! Link understanding — URL detection, metadata extraction, and content summarization.
//!
//! Scans messages for URLs, fetches pages/documents behind them, and extracts:
//! - Page title, description, canonical URL (Open Graph + HTML meta)
//! - Main content via readability heuristics
//! - Image/video preview URLs
//! - File type detection for direct links (PDF, image, video, etc.)
//!
//! Results are cached by URL with configurable TTL.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Extracted metadata from a URL.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkPreview {
    pub url: String,
    pub canonical_url: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub image_url: Option<String>,
    pub video_url: Option<String>,
    pub site_name: Option<String>,
    pub content_type: ContentType,
    pub content_text: Option<String>,
    pub favicon_url: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<String>,
}

/// Type of content behind the URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentType {
    WebPage,
    Image,
    Video,
    Audio,
    Pdf,
    Document,
    Archive,
    Unknown,
}

impl ContentType {
    /// Infer content type from MIME type string.
    pub fn from_mime(mime: &str) -> Self {
        let lower = mime.to_lowercase();
        if lower.starts_with("text/html") || lower.starts_with("application/xhtml") {
            ContentType::WebPage
        } else if lower.starts_with("image/") {
            ContentType::Image
        } else if lower.starts_with("video/") {
            ContentType::Video
        } else if lower.starts_with("audio/") {
            ContentType::Audio
        } else if lower.contains("pdf") {
            ContentType::Pdf
        } else if lower.contains("document")
            || lower.contains("spreadsheet")
            || lower.contains("presentation")
            || lower.contains("msword")
        {
            ContentType::Document
        } else if lower.contains("zip")
            || lower.contains("tar")
            || lower.contains("gzip")
            || lower.contains("rar")
        {
            ContentType::Archive
        } else {
            ContentType::Unknown
        }
    }

    /// Infer content type from URL file extension.
    pub fn from_extension(url: &str) -> Self {
        let path = url.split('?').next().unwrap_or(url);
        let ext = path.rsplit('.').next().unwrap_or("").to_lowercase();
        match ext.as_str() {
            "html" | "htm" | "php" | "asp" | "aspx" | "jsp" => ContentType::WebPage,
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "svg" | "bmp" | "ico" => ContentType::Image,
            "mp4" | "webm" | "avi" | "mkv" | "mov" | "flv" => ContentType::Video,
            "mp3" | "ogg" | "wav" | "flac" | "aac" | "m4a" => ContentType::Audio,
            "pdf" => ContentType::Pdf,
            "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "odp" => {
                ContentType::Document
            }
            "zip" | "tar" | "gz" | "bz2" | "7z" | "rar" => ContentType::Archive,
            _ => ContentType::Unknown,
        }
    }
}

/// Configuration for link understanding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkConfig {
    /// Maximum number of URLs to process per message.
    pub max_urls_per_message: usize,
    /// Maximum content length to fetch (bytes).
    pub max_content_bytes: usize,
    /// Request timeout.
    pub timeout_secs: u64,
    /// Cache TTL for link previews.
    pub cache_ttl_secs: u64,
    /// Maximum cache entries.
    pub max_cache_entries: usize,
    /// User-Agent header.
    pub user_agent: String,
    /// Whether to attempt content extraction (readability).
    pub extract_content: bool,
    /// Maximum content text length to store.
    pub max_content_text_len: usize,
}

impl Default for LinkConfig {
    fn default() -> Self {
        Self {
            max_urls_per_message: 5,
            max_content_bytes: 2 * 1024 * 1024, // 2 MB
            timeout_secs: 10,
            cache_ttl_secs: 3600, // 1 hour
            max_cache_entries: 10_000,
            user_agent: "ClawDesk-LinkBot/1.0".to_string(),
            extract_content: true,
            max_content_text_len: 4096,
        }
    }
}

/// Trait for HTTP fetching — allows mocking in tests.
#[async_trait]
pub trait HttpFetcher: Send + Sync {
    async fn fetch(&self, url: &str) -> Result<FetchResponse, LinkError>;
}

/// Raw HTTP response for link processing.
#[derive(Debug, Clone)]
pub struct FetchResponse {
    pub status: u16,
    pub content_type: Option<String>,
    pub body: String,
    pub final_url: String,
}

/// Cache entry with TTL.
struct CacheEntry {
    preview: LinkPreview,
    inserted_at: Instant,
}

/// Link understanding engine.
pub struct LinkUnderstanding {
    config: LinkConfig,
    fetcher: Arc<dyn HttpFetcher>,
    cache: Arc<RwLock<HashMap<String, CacheEntry>>>,
}

impl LinkUnderstanding {
    pub fn new(config: LinkConfig, fetcher: Arc<dyn HttpFetcher>) -> Self {
        Self {
            config,
            fetcher,
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Extract URLs from text.
    pub fn extract_urls(text: &str) -> Vec<String> {
        let mut urls = Vec::new();
        for word in text.split_whitespace() {
            let trimmed = word.trim_matches(|c: char| {
                c == '<' || c == '>' || c == '(' || c == ')' || c == '[' || c == ']'
            });
            if (trimmed.starts_with("http://") || trimmed.starts_with("https://"))
                && trimmed.len() > 10
            {
                urls.push(trimmed.to_string());
            }
        }
        urls
    }

    /// Process all URLs in a message text, returning previews.
    pub async fn process_message(&self, text: &str) -> Vec<LinkPreview> {
        let urls = Self::extract_urls(text);
        let urls: Vec<String> = urls
            .into_iter()
            .take(self.config.max_urls_per_message)
            .collect();

        let mut previews = Vec::new();

        for url in &urls {
            match self.get_preview(url).await {
                Ok(preview) => previews.push(preview),
                Err(e) => {
                    warn!(url = url.as_str(), error = %e, "failed to get link preview");
                }
            }
        }

        previews
    }

    /// Get a link preview, using cache if available.
    pub async fn get_preview(&self, url: &str) -> Result<LinkPreview, LinkError> {
        // Check cache
        {
            let cache = self.cache.read().await;
            if let Some(entry) = cache.get(url) {
                let ttl = Duration::from_secs(self.config.cache_ttl_secs);
                if entry.inserted_at.elapsed() < ttl {
                    debug!(url, "cache hit for link preview");
                    return Ok(entry.preview.clone());
                }
            }
        }

        // Fetch and parse
        let preview = self.fetch_and_parse(url).await?;

        // Update cache
        {
            let mut cache = self.cache.write().await;
            // Evict if over limit
            if cache.len() >= self.config.max_cache_entries {
                // Remove oldest entry
                let oldest_key = cache
                    .iter()
                    .min_by_key(|(_, v)| v.inserted_at)
                    .map(|(k, _)| k.clone());
                if let Some(key) = oldest_key {
                    cache.remove(&key);
                }
            }
            cache.insert(
                url.to_string(),
                CacheEntry {
                    preview: preview.clone(),
                    inserted_at: Instant::now(),
                },
            );
        }

        Ok(preview)
    }

    /// Fetch a URL and extract metadata.
    async fn fetch_and_parse(&self, url: &str) -> Result<LinkPreview, LinkError> {
        let response = self.fetcher.fetch(url).await?;

        let content_type = response
            .content_type
            .as_deref()
            .map(ContentType::from_mime)
            .unwrap_or_else(|| ContentType::from_extension(url));

        let mut preview = LinkPreview {
            url: url.to_string(),
            canonical_url: if response.final_url != url {
                Some(response.final_url.clone())
            } else {
                None
            },
            title: None,
            description: None,
            image_url: None,
            video_url: None,
            site_name: None,
            content_type,
            content_text: None,
            favicon_url: None,
            author: None,
            published_at: None,
        };

        if content_type == ContentType::WebPage {
            self.extract_html_metadata(&response.body, &mut preview);
            if self.config.extract_content {
                let content = self.extract_readable_content(&response.body);
                if !content.is_empty() {
                    let truncated = if content.len() > self.config.max_content_text_len {
                        format!(
                            "{}…",
                            &content[..self.config.max_content_text_len]
                        )
                    } else {
                        content
                    };
                    preview.content_text = Some(truncated);
                }
            }
        }

        info!(
            url = url,
            title = preview.title.as_deref().unwrap_or("-"),
            content_type = ?content_type,
            "link preview extracted"
        );

        Ok(preview)
    }

    /// Extract Open Graph and HTML meta tags from HTML.
    fn extract_html_metadata(&self, html: &str, preview: &mut LinkPreview) {
        // Open Graph meta tags
        preview.title = extract_meta_content(html, "og:title")
            .or_else(|| extract_tag_content(html, "title"));
        preview.description = extract_meta_content(html, "og:description")
            .or_else(|| extract_meta_content(html, "description"));
        preview.image_url = extract_meta_content(html, "og:image");
        preview.video_url = extract_meta_content(html, "og:video");
        preview.site_name = extract_meta_content(html, "og:site_name");
        preview.author = extract_meta_content(html, "author")
            .or_else(|| extract_meta_content(html, "article:author"));
        preview.published_at = extract_meta_content(html, "article:published_time");

        // Favicon
        preview.favicon_url = extract_link_href(html, "icon")
            .or_else(|| extract_link_href(html, "shortcut icon"));
    }

    /// Simplified readability extraction: strips HTML tags, scripts, styles,
    /// and returns main text content.
    fn extract_readable_content(&self, html: &str) -> String {
        let mut text = html.to_string();

        // Remove script/style blocks
        while let Some(start) = text.find("<script") {
            if let Some(end) = text[start..].find("</script>") {
                text = format!("{}{}", &text[..start], &text[start + end + 9..]);
            } else {
                break;
            }
        }
        while let Some(start) = text.find("<style") {
            if let Some(end) = text[start..].find("</style>") {
                text = format!("{}{}", &text[..start], &text[start + end + 8..]);
            } else {
                break;
            }
        }

        // Strip HTML tags
        let mut result = String::new();
        let mut in_tag = false;
        for ch in text.chars() {
            match ch {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => result.push(ch),
                _ => {}
            }
        }

        // Decode common HTML entities
        let result = result
            .replace("&amp;", "&")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&nbsp;", " ");

        // Collapse whitespace
        let mut collapsed = String::new();
        let mut last_was_space = false;
        for ch in result.chars() {
            if ch.is_whitespace() {
                if !last_was_space {
                    collapsed.push(' ');
                    last_was_space = true;
                }
            } else {
                collapsed.push(ch);
                last_was_space = false;
            }
        }

        collapsed.trim().to_string()
    }

    /// Clear the cache.
    pub async fn clear_cache(&self) {
        let mut cache = self.cache.write().await;
        cache.clear();
        info!("link preview cache cleared");
    }

    /// Get cache statistics.
    pub async fn cache_stats(&self) -> (usize, usize) {
        let cache = self.cache.read().await;
        let total = cache.len();
        let ttl = Duration::from_secs(self.config.cache_ttl_secs);
        let expired = cache
            .values()
            .filter(|e| e.inserted_at.elapsed() >= ttl)
            .count();
        (total, expired)
    }
}

/// Error type for link understanding.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    #[error("fetch failed: {0}")]
    FetchFailed(String),
    #[error("timeout fetching URL")]
    Timeout,
    #[error("content too large")]
    ContentTooLarge,
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    #[error("blocked domain: {0}")]
    BlockedDomain(String),
}

// ── HTML helper functions ─────────────────────────────────

/// Extract content attribute from a meta tag with matching property/name.
fn extract_meta_content(html: &str, property: &str) -> Option<String> {
    // Search for <meta property="..." content="..."> or <meta name="..." content="...">
    let lower = html.to_lowercase();
    let patterns = [
        format!("property=\"{}\"", property),
        format!("name=\"{}\"", property),
        format!("property='{}'", property),
        format!("name='{}'", property),
    ];

    for pattern in &patterns {
        if let Some(pos) = lower.find(pattern.as_str()) {
            // Find the meta tag boundaries
            let tag_start = lower[..pos].rfind('<')?;
            let tag_end_offset = lower[tag_start..].find('>')?;
            let tag = &html[tag_start..tag_start + tag_end_offset + 1];

            // Extract content attribute
            return extract_attribute(tag, "content");
        }
    }
    None
}

/// Extract content of an HTML tag like <title>...</title>.
fn extract_tag_content(html: &str, tag: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let open = format!("<{}", tag);
    let close = format!("</{}>", tag);

    let start = lower.find(&open)?;
    let content_start = html[start..].find('>')? + start + 1;
    let end = lower[content_start..].find(&close)? + content_start;

    Some(html[content_start..end].trim().to_string())
}

/// Extract href attribute from a link tag with matching rel.
fn extract_link_href(html: &str, rel: &str) -> Option<String> {
    let lower = html.to_lowercase();
    let pattern = format!("rel=\"{}\"", rel);

    if let Some(pos) = lower.find(&pattern) {
        let tag_start = lower[..pos].rfind('<')?;
        let tag_end_offset = lower[tag_start..].find('>')?;
        let tag = &html[tag_start..tag_start + tag_end_offset + 1];
        return extract_attribute(tag, "href");
    }
    None
}

/// Extract a named attribute value from an HTML tag string.
fn extract_attribute(tag: &str, attr: &str) -> Option<String> {
    let lower = tag.to_lowercase();
    for quote in ['"', '\''] {
        let pattern = format!("{}={}", attr, quote);
        if let Some(start) = lower.find(&pattern) {
            let value_start = start + pattern.len();
            if let Some(end) = tag[value_start..].find(quote) {
                return Some(tag[value_start..value_start + end].to_string());
            }
        }
    }
    None
}

// ── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_url_extraction() {
        let text = "Check out https://example.com and http://foo.bar/path?q=1";
        let urls = LinkUnderstanding::extract_urls(text);
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0], "https://example.com");
        assert_eq!(urls[1], "http://foo.bar/path?q=1");
    }

    #[test]
    fn test_url_extraction_with_brackets() {
        let text = "See <https://example.com> for details";
        let urls = LinkUnderstanding::extract_urls(text);
        assert_eq!(urls.len(), 1);
        assert_eq!(urls[0], "https://example.com");
    }

    #[test]
    fn test_content_type_from_mime() {
        assert_eq!(ContentType::from_mime("text/html"), ContentType::WebPage);
        assert_eq!(
            ContentType::from_mime("image/png"),
            ContentType::Image
        );
        assert_eq!(
            ContentType::from_mime("application/pdf"),
            ContentType::Pdf
        );
        assert_eq!(ContentType::from_mime("video/mp4"), ContentType::Video);
    }

    #[test]
    fn test_content_type_from_extension() {
        assert_eq!(
            ContentType::from_extension("https://example.com/doc.pdf"),
            ContentType::Pdf
        );
        assert_eq!(
            ContentType::from_extension("https://example.com/pic.jpg?w=100"),
            ContentType::Image
        );
        assert_eq!(
            ContentType::from_extension("https://example.com/page.html"),
            ContentType::WebPage
        );
    }

    #[test]
    fn test_extract_meta_content() {
        let html = r#"<html><head>
            <meta property="og:title" content="Test Title">
            <meta property="og:description" content="A description">
            <meta name="author" content="John">
            <title>Page Title</title>
        </head></html>"#;

        assert_eq!(
            extract_meta_content(html, "og:title"),
            Some("Test Title".to_string())
        );
        assert_eq!(
            extract_meta_content(html, "og:description"),
            Some("A description".to_string())
        );
        assert_eq!(
            extract_meta_content(html, "author"),
            Some("John".to_string())
        );
    }

    #[test]
    fn test_extract_tag_content() {
        let html = "<html><head><title>My Page</title></head></html>";
        assert_eq!(
            extract_tag_content(html, "title"),
            Some("My Page".to_string())
        );
    }

    #[test]
    fn test_no_urls() {
        let text = "No links here, just text.";
        let urls = LinkUnderstanding::extract_urls(text);
        assert!(urls.is_empty());
    }

    struct MockFetcher {
        response: FetchResponse,
    }

    #[async_trait]
    impl HttpFetcher for MockFetcher {
        async fn fetch(&self, _url: &str) -> Result<FetchResponse, LinkError> {
            Ok(self.response.clone())
        }
    }

    #[tokio::test]
    async fn test_link_understanding_with_mock() {
        let html = r#"<html><head>
            <meta property="og:title" content="Test">
            <meta property="og:description" content="Desc">
            <title>Fallback</title>
        </head><body><p>Content here</p></body></html>"#;

        let fetcher = Arc::new(MockFetcher {
            response: FetchResponse {
                status: 200,
                content_type: Some("text/html".to_string()),
                body: html.to_string(),
                final_url: "https://example.com".to_string(),
            },
        });

        let lu = LinkUnderstanding::new(LinkConfig::default(), fetcher);
        let preview = lu.get_preview("https://example.com").await.unwrap();

        assert_eq!(preview.title, Some("Test".to_string()));
        assert_eq!(preview.description, Some("Desc".to_string()));
        assert_eq!(preview.content_type, ContentType::WebPage);
    }

    #[tokio::test]
    async fn test_cache_hit() {
        let fetcher = Arc::new(MockFetcher {
            response: FetchResponse {
                status: 200,
                content_type: Some("text/html".to_string()),
                body: "<html><title>Cached</title></html>".to_string(),
                final_url: "https://cached.com".to_string(),
            },
        });

        let lu = LinkUnderstanding::new(LinkConfig::default(), fetcher);

        // First call: cache miss
        let _ = lu.get_preview("https://cached.com").await.unwrap();
        let (total, _) = lu.cache_stats().await;
        assert_eq!(total, 1);

        // Second call: cache hit (same result)
        let preview = lu.get_preview("https://cached.com").await.unwrap();
        assert_eq!(preview.title, Some("Cached".to_string()));
    }
}
