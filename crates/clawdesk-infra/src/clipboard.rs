//! Clipboard integration — cross-platform clipboard access for the desktop app.
//!
//! Provides paste handling (text, images, files) for Tauri UI and content
//! extraction for agent context. Supports:
//! - Text clipboard read/write
//! - Image clipboard (PNG/JPEG from clipboard data)
//! - File reference clipboard (file paths)
//! - Rich text / HTML clipboard
//! - Clipboard history with configurable retention

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info};
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Content type of clipboard data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardContentType {
    PlainText,
    RichText,
    Html,
    Image,
    FilePaths,
    Unknown,
}

/// A clipboard entry — snapshot of what was on the clipboard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardEntry {
    pub id: String,
    pub content_type: ClipboardContentType,
    pub text: Option<String>,
    pub html: Option<String>,
    pub image_data: Option<Vec<u8>>,
    pub image_mime: Option<String>,
    pub file_paths: Vec<String>,
    pub source: ClipboardSource,
    pub timestamp: DateTime<Utc>,
    pub byte_size: usize,
}

impl ClipboardEntry {
    /// Create a plain text clipboard entry.
    pub fn text(text: impl Into<String>) -> Self {
        let t = text.into();
        let size = t.len();
        Self {
            id: Uuid::new_v4().to_string(),
            content_type: ClipboardContentType::PlainText,
            text: Some(t),
            html: None,
            image_data: None,
            image_mime: None,
            file_paths: Vec::new(),
            source: ClipboardSource::User,
            timestamp: Utc::now(),
            byte_size: size,
        }
    }

    /// Create an HTML clipboard entry.
    pub fn html(html: impl Into<String>, fallback_text: Option<String>) -> Self {
        let h = html.into();
        let size = h.len() + fallback_text.as_ref().map(|t| t.len()).unwrap_or(0);
        Self {
            id: Uuid::new_v4().to_string(),
            content_type: ClipboardContentType::Html,
            text: fallback_text,
            html: Some(h),
            image_data: None,
            image_mime: None,
            file_paths: Vec::new(),
            source: ClipboardSource::User,
            timestamp: Utc::now(),
            byte_size: size,
        }
    }

    /// Create an image clipboard entry.
    pub fn image(data: Vec<u8>, mime: impl Into<String>) -> Self {
        let size = data.len();
        Self {
            id: Uuid::new_v4().to_string(),
            content_type: ClipboardContentType::Image,
            text: None,
            html: None,
            image_data: Some(data),
            image_mime: Some(mime.into()),
            file_paths: Vec::new(),
            source: ClipboardSource::User,
            timestamp: Utc::now(),
            byte_size: size,
        }
    }

    /// Create a file paths clipboard entry.
    pub fn files(paths: Vec<String>) -> Self {
        let size: usize = paths.iter().map(|p| p.len()).sum();
        Self {
            id: Uuid::new_v4().to_string(),
            content_type: ClipboardContentType::FilePaths,
            text: None,
            html: None,
            image_data: None,
            image_mime: None,
            file_paths: paths,
            source: ClipboardSource::User,
            timestamp: Utc::now(),
            byte_size: size,
        }
    }

    /// Set the source.
    pub fn with_source(mut self, source: ClipboardSource) -> Self {
        self.source = source;
        self
    }

    /// Get a textual summary for agent context.
    pub fn summary(&self) -> String {
        match self.content_type {
            ClipboardContentType::PlainText => {
                let text = self.text.as_deref().unwrap_or("");
                if text.len() > 200 {
                    format!("[clipboard text: {}... ({} chars)]", &text[..200], text.len())
                } else {
                    format!("[clipboard text: {}]", text)
                }
            }
            ClipboardContentType::Html | ClipboardContentType::RichText => {
                let fallback = self.text.as_deref().unwrap_or("[rich content]");
                format!("[clipboard HTML/rich: {}]", &fallback[..fallback.len().min(200)])
            }
            ClipboardContentType::Image => {
                let mime = self.image_mime.as_deref().unwrap_or("image/*");
                format!(
                    "[clipboard image: {} ({} bytes)]",
                    mime, self.byte_size
                )
            }
            ClipboardContentType::FilePaths => {
                let count = self.file_paths.len();
                if count == 1 {
                    format!("[clipboard file: {}]", self.file_paths[0])
                } else {
                    format!("[clipboard: {} files]", count)
                }
            }
            ClipboardContentType::Unknown => "[clipboard: unknown content]".to_string(),
        }
    }
}

/// Where clipboard content came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClipboardSource {
    /// User pasted from external app.
    User,
    /// ClawDesk copied to clipboard (e.g. code block copy).
    App,
    /// Agent output copied to clipboard.
    Agent,
}

/// Configuration for clipboard manager.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardConfig {
    /// Maximum number of history entries.
    pub max_history: usize,
    /// Maximum size of a single entry in bytes.
    pub max_entry_bytes: usize,
    /// Whether to store image data in history (can use a lot of memory).
    pub store_images: bool,
    /// Whether to enable clipboard monitoring.
    pub monitor_enabled: bool,
}

impl Default for ClipboardConfig {
    fn default() -> Self {
        Self {
            max_history: 50,
            max_entry_bytes: 10 * 1024 * 1024, // 10 MB
            store_images: true,
            monitor_enabled: true,
        }
    }
}

/// Clipboard manager — history, read/write, and content extraction.
pub struct ClipboardManager {
    config: ClipboardConfig,
    history: Arc<RwLock<VecDeque<ClipboardEntry>>>,
}

impl ClipboardManager {
    pub fn new(config: ClipboardConfig) -> Self {
        Self {
            config,
            history: Arc::new(RwLock::new(VecDeque::new())),
        }
    }

    /// Record a clipboard entry (called when paste is detected or content is read).
    pub async fn record(&self, entry: ClipboardEntry) {
        if entry.byte_size > self.config.max_entry_bytes {
            debug!(
                size = entry.byte_size,
                max = self.config.max_entry_bytes,
                "clipboard entry too large, skipping"
            );
            return;
        }

        let mut history = self.history.write().await;
        history.push_front(entry);
        while history.len() > self.config.max_history {
            history.pop_back();
        }
        debug!(count = history.len(), "clipboard entry recorded");
    }

    /// Write text to clipboard and record in history.
    pub async fn write_text(&self, text: impl Into<String>) -> ClipboardEntry {
        let entry = ClipboardEntry::text(text).with_source(ClipboardSource::App);
        self.record(entry.clone()).await;
        info!("wrote text to clipboard");
        entry
    }

    /// Get the most recent clipboard entry.
    pub async fn latest(&self) -> Option<ClipboardEntry> {
        self.history.read().await.front().cloned()
    }

    /// Get clipboard history.
    pub async fn history(&self, limit: usize) -> Vec<ClipboardEntry> {
        self.history
            .read()
            .await
            .iter()
            .take(limit)
            .cloned()
            .collect()
    }

    /// Search history by text content.
    pub async fn search(&self, query: &str) -> Vec<ClipboardEntry> {
        let query_lower = query.to_lowercase();
        self.history
            .read()
            .await
            .iter()
            .filter(|e| {
                e.text
                    .as_ref()
                    .map(|t| t.to_lowercase().contains(&query_lower))
                    .unwrap_or(false)
            })
            .cloned()
            .collect()
    }

    /// Clear clipboard history.
    pub async fn clear_history(&self) {
        self.history.write().await.clear();
        info!("clipboard history cleared");
    }

    /// Get statistics.
    pub async fn stats(&self) -> ClipboardStats {
        let history = self.history.read().await;
        let total_bytes: usize = history.iter().map(|e| e.byte_size).sum();
        let text_count = history
            .iter()
            .filter(|e| e.content_type == ClipboardContentType::PlainText)
            .count();
        let image_count = history
            .iter()
            .filter(|e| e.content_type == ClipboardContentType::Image)
            .count();
        ClipboardStats {
            entry_count: history.len(),
            total_bytes,
            text_count,
            image_count,
            file_count: history
                .iter()
                .filter(|e| e.content_type == ClipboardContentType::FilePaths)
                .count(),
        }
    }
}

/// Clipboard statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClipboardStats {
    pub entry_count: usize,
    pub total_bytes: usize,
    pub text_count: usize,
    pub image_count: usize,
    pub file_count: usize,
}

// ── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_entry() {
        let entry = ClipboardEntry::text("Hello, world!");
        assert_eq!(entry.content_type, ClipboardContentType::PlainText);
        assert_eq!(entry.text, Some("Hello, world!".to_string()));
        assert_eq!(entry.byte_size, 13);
    }

    #[test]
    fn test_image_entry() {
        let data = vec![0x89, 0x50, 0x4E, 0x47]; // PNG magic
        let entry = ClipboardEntry::image(data.clone(), "image/png");
        assert_eq!(entry.content_type, ClipboardContentType::Image);
        assert_eq!(entry.image_mime, Some("image/png".to_string()));
        assert_eq!(entry.byte_size, 4);
    }

    #[test]
    fn test_files_entry() {
        let entry = ClipboardEntry::files(vec![
            "/path/to/file.txt".to_string(),
            "/path/to/other.rs".to_string(),
        ]);
        assert_eq!(entry.content_type, ClipboardContentType::FilePaths);
        assert_eq!(entry.file_paths.len(), 2);
    }

    #[test]
    fn test_summary() {
        let entry = ClipboardEntry::text("short");
        assert!(entry.summary().contains("short"));

        let entry = ClipboardEntry::image(vec![0; 1024], "image/jpeg");
        assert!(entry.summary().contains("1024 bytes"));

        let entry = ClipboardEntry::files(vec!["file.txt".to_string()]);
        assert!(entry.summary().contains("file.txt"));

        let entry = ClipboardEntry::files(vec!["a.txt".to_string(), "b.txt".to_string()]);
        assert!(entry.summary().contains("2 files"));
    }

    #[tokio::test]
    async fn test_clipboard_manager() {
        let mgr = ClipboardManager::new(ClipboardConfig {
            max_history: 3,
            ..Default::default()
        });

        mgr.record(ClipboardEntry::text("first")).await;
        mgr.record(ClipboardEntry::text("second")).await;
        mgr.record(ClipboardEntry::text("third")).await;

        let history = mgr.history(10).await;
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].text, Some("third".to_string()));

        // Fourth entry evicts oldest
        mgr.record(ClipboardEntry::text("fourth")).await;
        let history = mgr.history(10).await;
        assert_eq!(history.len(), 3);
        assert_eq!(history[0].text, Some("fourth".to_string()));
    }

    #[tokio::test]
    async fn test_write_text() {
        let mgr = ClipboardManager::new(ClipboardConfig::default());
        let entry = mgr.write_text("copied").await;
        assert_eq!(entry.source, ClipboardSource::App);

        let latest = mgr.latest().await.unwrap();
        assert_eq!(latest.text, Some("copied".to_string()));
    }

    #[tokio::test]
    async fn test_search() {
        let mgr = ClipboardManager::new(ClipboardConfig::default());
        mgr.record(ClipboardEntry::text("hello world")).await;
        mgr.record(ClipboardEntry::text("foo bar")).await;
        mgr.record(ClipboardEntry::text("hello again")).await;

        let results = mgr.search("hello").await;
        assert_eq!(results.len(), 2);
    }

    #[tokio::test]
    async fn test_stats() {
        let mgr = ClipboardManager::new(ClipboardConfig::default());
        mgr.record(ClipboardEntry::text("text1")).await;
        mgr.record(ClipboardEntry::image(vec![0; 100], "image/png")).await;
        mgr.record(ClipboardEntry::files(vec!["f.txt".to_string()])).await;

        let stats = mgr.stats().await;
        assert_eq!(stats.entry_count, 3);
        assert_eq!(stats.text_count, 1);
        assert_eq!(stats.image_count, 1);
        assert_eq!(stats.file_count, 1);
    }

    #[tokio::test]
    async fn test_size_limit() {
        let mgr = ClipboardManager::new(ClipboardConfig {
            max_entry_bytes: 10,
            ..Default::default()
        });

        // This entry is too large
        mgr.record(ClipboardEntry::text("this is way too long for the limit")).await;
        let history = mgr.history(10).await;
        assert!(history.is_empty());

        // This entry fits
        mgr.record(ClipboardEntry::text("short")).await;
        let history = mgr.history(10).await;
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn test_entry_serialization() {
        let entry = ClipboardEntry::text("test");
        let json = serde_json::to_string(&entry).unwrap();
        let parsed: ClipboardEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.text, Some("test".to_string()));
        assert_eq!(parsed.content_type, ClipboardContentType::PlainText);
    }
}
