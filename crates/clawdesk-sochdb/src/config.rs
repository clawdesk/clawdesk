//! SochDB config store implementation.

use async_trait::async_trait;
use clawdesk_storage::ConfigStore;
use clawdesk_types::{config::ClawDeskConfig, error::StorageError};

use crate::SochStore;

#[async_trait]
impl ConfigStore for SochStore {
    async fn load_config(&self) -> Result<ClawDeskConfig, StorageError> {
        match self.db.get(b"config/main") {
            Ok(Some(bytes)) => {
                serde_json::from_slice(&bytes).map_err(|e| StorageError::SerializationFailed {
                    detail: e.to_string(),
                })
            }
            Ok(None) => Ok(ClawDeskConfig::default()),
            Err(e) => Err(StorageError::OpenFailed {
                detail: e.to_string(),
            }),
        }
    }

    async fn save_config(&self, config: &ClawDeskConfig) -> Result<(), StorageError> {
        let bytes = serde_json::to_vec(config).map_err(|e| StorageError::SerializationFailed {
            detail: e.to_string(),
        })?;

        self.db
            .put(b"config/main", &bytes)
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        // Increment version
        let version = self.config_version().await.unwrap_or(0) + 1;
        self.db
            .put(b"config/version", &version.to_le_bytes())
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        Ok(())
    }

    async fn config_version(&self) -> Result<u64, StorageError> {
        match self.db.get(b"config/version") {
            Ok(Some(bytes)) => {
                if bytes.len() >= 8 {
                    let arr: [u8; 8] = bytes[..8].try_into().unwrap();
                    Ok(u64::from_le_bytes(arr))
                } else {
                    Ok(0)
                }
            }
            Ok(None) => Ok(0),
            Err(e) => Err(StorageError::OpenFailed {
                detail: e.to_string(),
            }),
        }
    }

    async fn get_value(&self, path: &str) -> Result<Option<serde_json::Value>, StorageError> {
        let key = format!("config/values/{}", path);
        match self.db.get(key.as_bytes()) {
            Ok(Some(bytes)) => {
                let value = serde_json::from_slice(&bytes)
                    .map_err(|e| StorageError::SerializationFailed {
                        detail: e.to_string(),
                    })?;
                Ok(Some(value))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(StorageError::OpenFailed {
                detail: e.to_string(),
            }),
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

        self.db
            .put(key.as_bytes(), &bytes)
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        Ok(())
    }
}
