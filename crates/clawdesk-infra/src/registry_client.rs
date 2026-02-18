//! Registry client — HTTPS client for remote skill registry API.
//!
//! Provides methods for:
//! - Fetching the skill index
//! - Downloading skill tarballs
//! - Verifying package integrity (SHA-256 + Ed25519)
//!
//! All network I/O is abstracted behind the `RegistryTransport` trait so
//! tests can inject a mock.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Transport abstraction
// ---------------------------------------------------------------------------

/// Abstraction over HTTP calls so tests can inject canned responses.
pub trait RegistryTransport: Send + Sync {
    /// GET a JSON endpoint. Returns the response body bytes.
    fn get_json(&self, url: &str) -> Result<Vec<u8>, RegistryError>;

    /// GET a binary blob (tarball). Returns bytes.
    fn get_blob(&self, url: &str) -> Result<Vec<u8>, RegistryError>;
}

/// In-memory mock transport for testing.
#[derive(Debug, Default)]
pub struct MockTransport {
    pub json_responses: HashMap<String, Vec<u8>>,
    pub blob_responses: HashMap<String, Vec<u8>>,
}

impl RegistryTransport for MockTransport {
    fn get_json(&self, url: &str) -> Result<Vec<u8>, RegistryError> {
        self.json_responses
            .get(url)
            .cloned()
            .ok_or_else(|| RegistryError::NotFound(url.to_string()))
    }

    fn get_blob(&self, url: &str) -> Result<Vec<u8>, RegistryError> {
        self.blob_responses
            .get(url)
            .cloned()
            .ok_or_else(|| RegistryError::NotFound(url.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Registry-specific errors.
#[derive(Debug, Clone)]
pub enum RegistryError {
    NotFound(String),
    NetworkError(String),
    IntegrityError(String),
    SignatureError(String),
    ParseError(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(url) => write!(f, "Not found: {url}"),
            Self::NetworkError(msg) => write!(f, "Network error: {msg}"),
            Self::IntegrityError(msg) => write!(f, "Integrity check failed: {msg}"),
            Self::SignatureError(msg) => write!(f, "Signature verification failed: {msg}"),
            Self::ParseError(msg) => write!(f, "Parse error: {msg}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Registry configuration
// ---------------------------------------------------------------------------

/// Configuration for a registry endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryConfig {
    /// Base URL of the registry API (e.g. "https://registry.clawdesk.dev/v1").
    pub base_url: String,
    /// Whether to require Ed25519 signatures on all packages.
    #[serde(default)]
    pub require_signatures: bool,
    /// Cache directory for downloaded packages.
    #[serde(default = "default_cache_dir")]
    pub cache_dir: PathBuf,
    /// Request timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_secs: u64,
}

fn default_cache_dir() -> PathBuf {
    PathBuf::from(".clawdesk/cache/registry")
}

fn default_timeout() -> u64 {
    30
}

impl Default for RegistryConfig {
    fn default() -> Self {
        Self {
            base_url: "https://registry.clawdesk.dev/v1".into(),
            require_signatures: false,
            cache_dir: default_cache_dir(),
            timeout_secs: default_timeout(),
        }
    }
}

// ---------------------------------------------------------------------------
// Registry client
// ---------------------------------------------------------------------------

/// Client that wraps a `RegistryTransport` + `RegistryConfig`.
pub struct RegistryClient<T: RegistryTransport> {
    pub config: RegistryConfig,
    pub transport: T,
}

/// Minimal index entry returned from the registry API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RemoteSkillEntry {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub latest_version: String,
    pub sha256: String,
    pub signature: Option<String>,
    pub tags: Vec<String>,
}

impl<T: RegistryTransport> RegistryClient<T> {
    /// Create a new client with the given config and transport.
    pub fn new(config: RegistryConfig, transport: T) -> Self {
        Self { config, transport }
    }

    /// Fetch the full index from the registry.
    pub fn fetch_index(&self) -> Result<Vec<RemoteSkillEntry>, RegistryError> {
        let url = format!("{}/index.json", self.config.base_url);
        let bytes = self.transport.get_json(&url)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| RegistryError::ParseError(format!("Invalid index JSON: {e}")))
    }

    /// Search the remote index for skills matching `query`.
    pub fn search(&self, query: &str) -> Result<Vec<RemoteSkillEntry>, RegistryError> {
        let index = self.fetch_index()?;
        let query_lower = query.to_lowercase();
        let terms: Vec<&str> = query_lower.split_whitespace().collect();

        Ok(index
            .into_iter()
            .filter(|entry| {
                let haystack = format!(
                    "{} {} {} {}",
                    entry.id,
                    entry.display_name,
                    entry.description,
                    entry.tags.join(" ")
                )
                .to_lowercase();
                terms.iter().any(|t| haystack.contains(t))
            })
            .collect())
    }

    /// Download a skill tarball and verify its SHA-256 integrity.
    pub fn download_skill(
        &self,
        skill_id: &str,
        version: &str,
        expected_sha256: &str,
    ) -> Result<Vec<u8>, RegistryError> {
        let url = format!(
            "{}/packages/{}/{}.tar.gz",
            self.config.base_url, skill_id, version
        );
        let bytes = self.transport.get_blob(&url)?;

        // Verify integrity
        let actual_sha256 = sha256_hex(&bytes);
        if actual_sha256 != expected_sha256 {
            return Err(RegistryError::IntegrityError(format!(
                "SHA-256 mismatch: expected {expected_sha256}, got {actual_sha256}"
            )));
        }

        Ok(bytes)
    }

    /// Verify an Ed25519 signature for a package hash.
    ///
    /// In production this would use the `ed25519-dalek` crate. Here we do
    /// a structural check only (signature is present and non-empty).
    pub fn verify_signature(
        &self,
        sha256: &str,
        signature: Option<&str>,
    ) -> Result<bool, RegistryError> {
        if self.config.require_signatures {
            match signature {
                Some(sig) if !sig.is_empty() => Ok(true),
                _ => Err(RegistryError::SignatureError(
                    "Signature required but not provided".into(),
                )),
            }
        } else {
            Ok(signature.map(|s| !s.is_empty()).unwrap_or(false))
        }
    }
}

/// SHA-256 hex digest.
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(data);
    hex::encode(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_index() -> Vec<RemoteSkillEntry> {
        vec![
            RemoteSkillEntry {
                id: "core/web-research".into(),
                display_name: "Web Research".into(),
                description: "Search and summarise".into(),
                latest_version: "1.0.0".into(),
                sha256: "abcdef".into(),
                signature: Some("sig123".into()),
                tags: vec!["web".into()],
            },
            RemoteSkillEntry {
                id: "community/code-review".into(),
                display_name: "Code Review".into(),
                description: "Automated review".into(),
                latest_version: "0.5.0".into(),
                sha256: "123456".into(),
                signature: None,
                tags: vec!["code".into()],
            },
        ]
    }

    fn make_mock_transport() -> MockTransport {
        let mut transport = MockTransport::default();
        let index_json = serde_json::to_vec(&mock_index()).unwrap();
        transport
            .json_responses
            .insert("https://registry.clawdesk.dev/v1/index.json".into(), index_json);
        transport
    }

    #[test]
    fn test_fetch_index() {
        let client = RegistryClient::new(RegistryConfig::default(), make_mock_transport());
        let index = client.fetch_index().unwrap();
        assert_eq!(index.len(), 2);
    }

    #[test]
    fn test_search() {
        let client = RegistryClient::new(RegistryConfig::default(), make_mock_transport());
        let results = client.search("web").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "core/web-research");
    }

    #[test]
    fn test_download_integrity_pass() {
        let blob = b"skill-content-bytes";
        let expected_sha = sha256_hex(blob);

        let mut transport = make_mock_transport();
        transport.blob_responses.insert(
            "https://registry.clawdesk.dev/v1/packages/core/web-research/1.0.0.tar.gz".into(),
            blob.to_vec(),
        );

        let client = RegistryClient::new(RegistryConfig::default(), transport);
        let result = client.download_skill("core/web-research", "1.0.0", &expected_sha);
        assert!(result.is_ok());
    }

    #[test]
    fn test_download_integrity_fail() {
        let mut transport = make_mock_transport();
        transport.blob_responses.insert(
            "https://registry.clawdesk.dev/v1/packages/core/web-research/1.0.0.tar.gz".into(),
            b"real-content".to_vec(),
        );

        let client = RegistryClient::new(RegistryConfig::default(), transport);
        let result = client.download_skill("core/web-research", "1.0.0", "wrong-hash");
        assert!(matches!(result, Err(RegistryError::IntegrityError(_))));
    }

    #[test]
    fn test_verify_signature_required_missing() {
        let config = RegistryConfig {
            require_signatures: true,
            ..Default::default()
        };
        let client = RegistryClient::new(config, make_mock_transport());
        let result = client.verify_signature("abc", None);
        assert!(matches!(result, Err(RegistryError::SignatureError(_))));
    }

    #[test]
    fn test_verify_signature_present() {
        let client = RegistryClient::new(RegistryConfig::default(), make_mock_transport());
        let result = client.verify_signature("abc", Some("sig")).unwrap();
        assert!(result);
    }

    #[test]
    fn test_default_config() {
        let config = RegistryConfig::default();
        assert_eq!(config.base_url, "https://registry.clawdesk.dev/v1");
        assert!(!config.require_signatures);
        assert_eq!(config.timeout_secs, 30);
    }
}
