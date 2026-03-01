//! Credential types shared across the extension system.

use serde::{Deserialize, Serialize};

/// A stored credential with metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credential {
    /// Integration name this credential belongs to
    pub integration: String,
    /// Credential identifier (e.g., "GITHUB_TOKEN")
    pub name: String,
    /// Display label
    pub label: Option<String>,
    /// When the credential was stored
    pub stored_at: chrono::DateTime<chrono::Utc>,
    /// Optional expiry time
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl Credential {
    pub fn is_expired(&self) -> bool {
        self.expires_at
            .map(|exp| exp < chrono::Utc::now())
            .unwrap_or(false)
    }
}
