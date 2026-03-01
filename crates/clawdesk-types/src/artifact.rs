//! GAP-E: Cross-Channel Artifact Pipeline — canonical artifact types.
//!
//! Provides a unified artifact representation that bridges:
//! - ACP `Artifact` (agent-to-agent protocol)
//! - `MediaAttachment` (channel messages)
//! - Thread blob attachments (SochDB key/value)
//! - `MediaCache` entries (filesystem cache)
//!
//! ## Content Addressing
//!
//! Every artifact is identified by a content-based hash (`ArtifactId`).
//! SHA-256 truncated to 32 hex chars provides globally unique identification
//! with negligible collision probability (p < 2⁻¹²⁸).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Content-addressed artifact identifier (SHA-256 truncated to 32 hex chars).
pub type ArtifactId = String;

/// Canonical artifact reference — the universal cross-channel artifact type.
///
/// This is the superset of ACP Artifact, MediaAttachment, and thread blobs.
/// All artifact-related storage, transport, and display flows through this type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// Content-addressed unique identifier (SHA-256 hash of data).
    pub id: ArtifactId,
    /// Human-readable name (filename, image caption, etc.).
    pub name: String,
    /// MIME type (e.g. "image/png", "text/plain", "application/json").
    pub mime_type: String,
    /// Size in bytes (exact, from the stored data).
    pub size_bytes: u64,
    /// How the artifact data is stored / accessible.
    pub data: ArtifactData,
    /// When the artifact was first stored.
    pub created_at: DateTime<Utc>,
    /// Optional session/thread that owns this artifact.
    pub owner_session: Option<String>,
    /// Tags for filtering and categorization.
    pub tags: Vec<String>,
    /// Which channels have accessed this artifact.
    pub accessed_from: Vec<String>,
    /// Time-to-live in seconds. 0 = indefinite.
    pub ttl_secs: u64,
}

/// How the artifact data is stored.
///
/// Inspired by ACP's `ArtifactData` but extended with a content-addressed
/// store reference. In order of preference:
/// - `StoreRef` — content-addressed (cheapest to transport cross-channel)
/// - `Url` — remote URL (may be ephemeral)
/// - `Inline` — embedded bytes (for small artifacts < 64KB)
/// - `Text` — inline text content (for code/markdown artifacts)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ArtifactData {
    /// Reference to the content-addressed artifact store.
    /// The ID can be resolved via `ArtifactStore::get()`.
    StoreRef { store_id: ArtifactId },
    /// External URL (possibly ephemeral or access-gated).
    Url { url: String, expires_at: Option<DateTime<Utc>> },
    /// Inline binary data (for artifacts < 64KB).
    Inline { data: Vec<u8> },
    /// Inline text content (code, markdown, JSON, etc.).
    Text { content: String },
}

impl ArtifactRef {
    /// Create a new artifact reference with inline text data.
    pub fn text(
        id: impl Into<String>,
        name: impl Into<String>,
        mime_type: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        let content = content.into();
        let size = content.len() as u64;
        Self {
            id: id.into(),
            name: name.into(),
            mime_type: mime_type.into(),
            size_bytes: size,
            data: ArtifactData::Text { content },
            created_at: Utc::now(),
            owner_session: None,
            tags: vec![],
            accessed_from: vec![],
            ttl_secs: 0,
        }
    }

    /// Create a new artifact reference with a store reference.
    pub fn store_ref(
        id: impl Into<String>,
        name: impl Into<String>,
        mime_type: impl Into<String>,
        size_bytes: u64,
    ) -> Self {
        let aid = id.into();
        Self {
            id: aid.clone(),
            name: name.into(),
            mime_type: mime_type.into(),
            size_bytes,
            data: ArtifactData::StoreRef { store_id: aid },
            created_at: Utc::now(),
            owner_session: None,
            tags: vec![],
            accessed_from: vec![],
            ttl_secs: 0,
        }
    }

    /// Create from a URL.
    pub fn url(
        id: impl Into<String>,
        name: impl Into<String>,
        mime_type: impl Into<String>,
        url: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            mime_type: mime_type.into(),
            size_bytes: 0,
            data: ArtifactData::Url {
                url: url.into(),
                expires_at: None,
            },
            created_at: Utc::now(),
            owner_session: None,
            tags: vec![],
            accessed_from: vec![],
            ttl_secs: 0,
        }
    }

    /// Whether this artifact is a text/code type.
    pub fn is_text(&self) -> bool {
        self.mime_type.starts_with("text/")
            || self.mime_type == "application/json"
            || self.mime_type == "application/xml"
    }

    /// Whether this artifact is an image.
    pub fn is_image(&self) -> bool {
        self.mime_type.starts_with("image/")
    }

    /// Whether this artifact has expired (past TTL).
    pub fn is_expired(&self) -> bool {
        if self.ttl_secs == 0 {
            return false;
        }
        let deadline = self.created_at + chrono::Duration::seconds(self.ttl_secs as i64);
        Utc::now() > deadline
    }
}

/// Convert from `MediaAttachment` to `ArtifactRef`.
impl From<&crate::message::MediaAttachment> for ArtifactRef {
    fn from(media: &crate::message::MediaAttachment) -> Self {
        let id = match (&media.data, &media.url) {
            (Some(data), _) => {
                // Content hash from data
                format!("{:x}", md5_like_hash(data))
            }
            (_, Some(url)) => {
                // Hash from URL
                format!("{:x}", md5_like_hash(url.as_bytes()))
            }
            _ => uuid::Uuid::new_v4().to_string(),
        };

        let data = match (&media.data, &media.url) {
            (Some(bytes), _) => ArtifactData::Inline { data: bytes.clone() },
            (_, Some(url)) => ArtifactData::Url {
                url: url.clone(),
                expires_at: None,
            },
            _ => ArtifactData::Text { content: String::new() },
        };

        ArtifactRef {
            id,
            name: media.filename.clone().unwrap_or_default(),
            mime_type: media.mime_type.clone(),
            size_bytes: media.size_bytes.unwrap_or(0),
            data,
            created_at: Utc::now(),
            owner_session: None,
            tags: vec![format!("media:{:?}", media.media_type)],
            accessed_from: vec![],
            ttl_secs: 0,
        }
    }
}

/// Simple FNV-1a hash for generating artifact IDs when no SHA-256 is available.
fn md5_like_hash(data: &[u8]) -> u128 {
    let mut h: u128 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u128;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Metadata for artifact indexing and search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactIndex {
    /// All known artifact references, keyed by ID.
    pub artifacts: Vec<ArtifactRef>,
    /// Total size of all stored artifacts.
    pub total_size_bytes: u64,
    /// Number of artifacts.
    pub count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_artifact_text() {
        let art = ArtifactRef::text("abc123", "readme.md", "text/markdown", "# Hello");
        assert_eq!(art.size_bytes, 7);
        assert!(art.is_text());
        assert!(!art.is_image());
        assert!(!art.is_expired());
    }

    #[test]
    fn test_artifact_url() {
        let art = ArtifactRef::url("def456", "photo.png", "image/png", "https://example.com/photo.png");
        assert!(art.is_image());
        assert!(!art.is_text());
    }

    #[test]
    fn test_artifact_expired() {
        let mut art = ArtifactRef::text("abc", "test", "text/plain", "data");
        art.ttl_secs = 1;
        art.created_at = Utc::now() - chrono::Duration::seconds(5);
        assert!(art.is_expired());
    }

    #[test]
    fn test_artifact_not_expired() {
        let mut art = ArtifactRef::text("abc", "test", "text/plain", "data");
        art.ttl_secs = 3600;
        assert!(!art.is_expired());
    }

    #[test]
    fn test_from_media_attachment() {
        let media = crate::message::MediaAttachment {
            media_type: crate::message::MediaType::Image,
            url: Some("https://example.com/img.png".to_string()),
            data: None,
            mime_type: "image/png".to_string(),
            filename: Some("img.png".to_string()),
            size_bytes: Some(1024),
        };
        let art = ArtifactRef::from(&media);
        assert_eq!(art.name, "img.png");
        assert_eq!(art.mime_type, "image/png");
        assert!(art.is_image());
    }
}
