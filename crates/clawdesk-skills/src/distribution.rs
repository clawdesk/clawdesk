//! A4: Skill Distribution Protocol.
//!
//! Defines the protocol for distributing skill packages between
//! registries and ClawDesk instances.
//!
//! ## Protocol Overview
//!
//! ```text
//! Publisher → Package(.clawpkg) → Sign → Upload → Registry
//! Registry → Index(catalog.json) → ClawDesk Store → Install
//! ```
//!
//! ## Wire Format
//!
//! Catalog synchronization uses a simple JSON-over-HTTPS protocol:
//! - `GET /catalog.json` — full catalog (for first sync)
//! - `GET /catalog/since/{timestamp}` — incremental updates
//! - `GET /packages/{skill_id}/{version}.clawpkg` — download package
//! - `GET /packages/{skill_id}/latest` — redirect to latest version
//!
//! All responses are signed with the registry's Ed25519 key.

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

// ═══════════════════════════════════════════════════════════════════════════
// Registry endpoint configuration
// ═══════════════════════════════════════════════════════════════════════════

/// Configuration for a remote skill registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEndpoint {
    /// Display name.
    pub name: String,
    /// Base URL (e.g., "https://skills.clawdesk.io").
    pub url: String,
    /// Registry's Ed25519 public key for response verification.
    pub public_key: Option<String>,
    /// Whether this registry is enabled.
    pub enabled: bool,
    /// Priority (lower = checked first).
    pub priority: u32,
    /// When this registry was last synced.
    pub last_sync: Option<String>,
}

impl RegistryEndpoint {
    /// Default official ClawDesk registry.
    pub fn official() -> Self {
        Self {
            name: "ClawDesk Official".into(),
            url: "https://skills.clawdesk.io".into(),
            public_key: None,
            enabled: true,
            priority: 0,
            last_sync: None,
        }
    }

    /// Community registry.
    pub fn community(name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            url: url.into(),
            public_key: None,
            enabled: true,
            priority: 10,
            last_sync: None,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Catalog sync protocol
// ═══════════════════════════════════════════════════════════════════════════

/// A catalog response from a registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogResponse {
    /// Schema version.
    pub version: u32,
    /// Registry name.
    pub registry: String,
    /// Timestamp of this catalog snapshot.
    pub timestamp: String,
    /// Catalog entries.
    pub entries: Vec<CatalogEntry>,
    /// Ed25519 signature of the entries JSON.
    pub signature: Option<String>,
}

/// A catalog entry — minimal metadata for the store listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogEntry {
    pub skill_id: String,
    pub display_name: String,
    pub version: String,
    pub description: String,
    pub category: String,
    pub author: String,
    pub tags: Vec<String>,
    pub checksum: String,
    pub size_bytes: u64,
    pub install_count: u64,
    pub rating: f32,
    pub verified: bool,
    pub updated_at: String,
    pub download_url: String,
}

/// Incremental update — only entries changed since a timestamp.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CatalogDelta {
    /// Base timestamp this delta applies to.
    pub since: String,
    /// Current timestamp.
    pub until: String,
    /// New or updated entries.
    pub added: Vec<CatalogEntry>,
    /// Removed skill IDs.
    pub removed: Vec<String>,
}

// ═══════════════════════════════════════════════════════════════════════════
// Download tracking
// ═══════════════════════════════════════════════════════════════════════════

/// State of a package download.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadProgress {
    pub skill_id: String,
    pub version: String,
    pub total_bytes: u64,
    pub downloaded_bytes: u64,
    pub state: DownloadState,
}

/// Download state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    /// Queued for download.
    Pending,
    /// Currently downloading.
    Downloading,
    /// Verifying checksum and signature.
    Verifying,
    /// Extracting package contents.
    Extracting,
    /// Download complete.
    Complete,
    /// Download failed.
    Failed,
}

impl DownloadProgress {
    /// Percentage complete (0-100).
    pub fn percent(&self) -> u8 {
        if self.total_bytes == 0 {
            return 0;
        }
        ((self.downloaded_bytes as f64 / self.total_bytes as f64) * 100.0) as u8
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_registry() {
        let r = RegistryEndpoint::official();
        assert_eq!(r.priority, 0);
        assert!(r.enabled);
    }

    #[test]
    fn download_progress_percent() {
        let p = DownloadProgress {
            skill_id: "test/skill".into(),
            version: "1.0.0".into(),
            total_bytes: 1000,
            downloaded_bytes: 500,
            state: DownloadState::Downloading,
        };
        assert_eq!(p.percent(), 50);
    }

    #[test]
    fn download_progress_zero_total() {
        let p = DownloadProgress {
            skill_id: "test/skill".into(),
            version: "1.0.0".into(),
            total_bytes: 0,
            downloaded_bytes: 0,
            state: DownloadState::Pending,
        };
        assert_eq!(p.percent(), 0);
    }

    #[test]
    fn catalog_roundtrip() {
        let catalog = CatalogResponse {
            version: 1,
            registry: "test".into(),
            timestamp: "2026-01-01T00:00:00Z".into(),
            entries: vec![CatalogEntry {
                skill_id: "test/hello".into(),
                display_name: "Hello".into(),
                version: "1.0.0".into(),
                description: "A hello skill".into(),
                category: "productivity".into(),
                author: "tester".into(),
                tags: vec!["hello".into()],
                checksum: "abc123".into(),
                size_bytes: 1024,
                install_count: 42,
                rating: 4.5,
                verified: true,
                updated_at: "2026-01-01".into(),
                download_url: "https://example.com/hello-1.0.0.clawpkg".into(),
            }],
            signature: None,
        };

        let json = serde_json::to_string(&catalog).unwrap();
        let parsed: CatalogResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.entries.len(), 1);
    }
}
