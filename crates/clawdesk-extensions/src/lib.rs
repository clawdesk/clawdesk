//! # clawdesk-extensions
//!
//! Declarative integration framework for ClawDesk.
//!
//! - **Registry**: TOML-driven integration loading (bundled + user)
//! - **Vault**: AES-256-GCM encrypted credential storage
//! - **OAuth**: OAuth2 PKCE authorization code flow
//! - **Health**: Integration health monitoring with auto-reconnect

pub mod credentials;
pub mod health;
pub mod oauth;
pub mod registry;
pub mod vault;

pub use credentials::*;
pub use health::{HealthMonitor, HealthState, HealthStatus};
pub use registry::{
    ConfigField, ConfigFieldOption, ConfigFieldType, Integration, IntegrationCategory,
    IntegrationRegistry,
};
pub use vault::CredentialVault;

use serde::{Deserialize, Serialize};

/// Extension system errors
#[derive(Debug, thiserror::Error)]
pub enum ExtensionError {
    #[error("integration not found: {0}")]
    NotFound(String),

    #[error("credential error: {0}")]
    CredentialError(String),

    #[error("vault error: {0}")]
    VaultError(String),

    #[error("OAuth error: {0}")]
    OAuthError(String),

    #[error("health check failed: {0}")]
    HealthCheckFailed(String),

    #[error("configuration error: {0}")]
    ConfigError(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}
