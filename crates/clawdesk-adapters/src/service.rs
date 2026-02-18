//! Unified ServiceAdapter trait for external API interactions.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Errors from service adapter operations.
#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("Rate limited: retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
    #[error("Circuit open: backing off for {backoff_secs}s")]
    CircuitOpen { backoff_secs: u64 },
    #[error("Authentication failed: {0}")]
    AuthFailed(String),
    #[error("API error ({status}): {message}")]
    ApiError { status: u16, message: String },
    #[error("Network error: {0}")]
    Network(String),
    #[error("Configuration error: {0}")]
    Config(String),
}

/// Data fetched from an external service during a poll cycle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollResult {
    /// Service identifier
    pub service_id: String,
    /// Items fetched (arbitrary structured data)
    pub items: Vec<serde_json::Value>,
    /// Cursor/pagination token for the next poll
    pub next_cursor: Option<String>,
    /// Timestamp of this poll
    pub polled_at: chrono::DateTime<chrono::Utc>,
}

/// Result of pushing data to an external service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushResult {
    pub service_id: String,
    pub success: bool,
    pub external_id: Option<String>,
    pub message: Option<String>,
}

/// Configuration for a service adapter instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterConfig {
    /// Service identifier (e.g., "todoist", "fathom", "gmail")
    pub service_id: String,
    /// Display name
    pub name: String,
    /// OAuth2 client ID (if applicable)
    pub client_id: Option<String>,
    /// OAuth2 client secret (if applicable)
    pub client_secret: Option<String>,
    /// API base URL
    pub base_url: String,
    /// Rate limit: requests per minute
    pub rate_limit_rpm: u32,
    /// Circuit breaker failure threshold (0.0 - 1.0)
    pub circuit_break_threshold: f64,
    /// Custom configuration parameters
    pub params: HashMap<String, String>,
}

/// The unified external service adapter trait.
///
/// Implementing this trait is the only requirement for adding a new
/// external service. Token refresh, retry, and rate limiting are
/// handled by the framework wrapping this trait.
#[async_trait]
pub trait ServiceAdapter: Send + Sync {
    /// Unique service identifier.
    fn service_id(&self) -> &str;

    /// Poll the external service for new data.
    ///
    /// Called periodically by the scheduler. The `cursor` parameter
    /// enables incremental polling (only fetch data since last poll).
    async fn poll(&self, cursor: Option<&str>) -> Result<PollResult, AdapterError>;

    /// Push data to the external service.
    ///
    /// Used for write operations (create Todoist task, send email, etc.)
    async fn push(&self, data: serde_json::Value) -> Result<PushResult, AdapterError>;

    /// Validate that the adapter is properly configured and can connect.
    async fn health_check(&self) -> Result<bool, AdapterError>;
}
