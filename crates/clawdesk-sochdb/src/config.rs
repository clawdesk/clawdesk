//! SochDB config store implementation.

use async_trait::async_trait;
use clawdesk_storage::ConfigStore;
use clawdesk_types::{config::ClawDeskConfig, error::StorageError};

use crate::SochStore;

#[async_trait]
impl ConfigStore for SochStore {
    async fn load_config(&self) -> Result<ClawDeskConfig, StorageError> {
        match self.get("config/main") {
            Ok(Some(bytes)) => {
                serde_json::from_slice(&bytes).map_err(|e| StorageError::SerializationFailed {
                    detail: e.to_string(),
                })
            }
            Ok(None) => Ok(ClawDeskConfig::default()),
            Err(e) => Err(e),
        }
    }

    async fn save_config(&self, config: &ClawDeskConfig) -> Result<(), StorageError> {
        let bytes = serde_json::to_vec(config).map_err(|e| StorageError::SerializationFailed {
            detail: e.to_string(),
        })?;

        // GAP-01: Use put_batch() to atomically write both config blob and
        // version counter. Individual put() calls rely on group-commit batching
        // and risk partial writes (config updated but version not, or vice versa).
        let version = self.config_version().await.unwrap_or(0) + 1;
        let version_bytes = version.to_le_bytes();
        self.put_batch(&[
            ("config/main", &bytes),
            ("config/version", &version_bytes),
        ])?;

        Ok(())
    }

    async fn config_version(&self) -> Result<u64, StorageError> {
        match self.get("config/version") {
            Ok(Some(bytes)) => {
                if bytes.len() >= 8 {
                    let arr: [u8; 8] = bytes[..8].try_into().unwrap();
                    Ok(u64::from_le_bytes(arr))
                } else {
                    Ok(0)
                }
            }
            Ok(None) => Ok(0),
            Err(e) => Err(e),
        }
    }

    async fn get_value(&self, path: &str) -> Result<Option<serde_json::Value>, StorageError> {
        let key = format!("config/values/{}", path);
        match self.get(&key) {
            Ok(Some(bytes)) => {
                let value = serde_json::from_slice(&bytes)
                    .map_err(|e| StorageError::SerializationFailed {
                        detail: e.to_string(),
                    })?;
                Ok(Some(value))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn set_value(
        &self,
        path: &str,
        value: serde_json::Value,
    ) -> Result<(), StorageError> {
        let key = format!("config/values/{}", path);
        let bytes = serde_json::to_vec(&value).map_err(|e| StorageError::SerializationFailed {
            detail: e.to_string(),
        })?;

        // GAP-01: Config values are user-facing settings — use durable writes.
        self.put_durable(&key, &bytes)?;

        Ok(())
    }
}
