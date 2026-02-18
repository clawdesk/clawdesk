//! Configuration storage with versioning and hot-reload.

use async_trait::async_trait;
use clawdesk_types::{config::ClawDeskConfig, error::StorageError};

/// Port: configuration storage.
#[async_trait]
pub trait ConfigStore: Send + Sync + 'static {
    /// Load the current configuration.
    async fn load_config(&self) -> Result<ClawDeskConfig, StorageError>;

    /// Save configuration atomically.
    async fn save_config(&self, config: &ClawDeskConfig) -> Result<(), StorageError>;

    /// Get the current config version number.
    async fn config_version(&self) -> Result<u64, StorageError>;

    /// Get a specific config value by path (e.g., "providers.anthropic.api_key").
    async fn get_value(&self, path: &str) -> Result<Option<serde_json::Value>, StorageError>;

    /// Set a specific config value by path.
    async fn set_value(
        &self,
        path: &str,
        value: serde_json::Value,
    ) -> Result<(), StorageError>;
}
