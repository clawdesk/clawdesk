//! Integration Adapter — standard trait for external integrations.
//!
//! Rather than building individual crates for each integration (IoT/MQTT,
//! Health APIs, Object Storage, GTFS-RT), this trait defines the standard
//! contract that all integrations must implement.
//!
//! ## Design
//!
//! Each `IntegrationAdapter` declares:
//! - Its capabilities (what actions it can perform)
//! - An `execute` method for request-response interactions
//! - A `subscribe` method for push-based/event-driven integrations
//! - A `health` check for monitoring
//!
//! ## Registration
//!
//! Adapters register their capabilities with the skill system and their
//! events with the bus. The `TaskDispatcher` routes tasks to adapters
//! via the capability index.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Health status of an integration adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterHealth {
    /// Whether the adapter is currently healthy.
    pub healthy: bool,
    /// Human-readable status message.
    pub message: String,
    /// Last successful operation timestamp.
    pub last_success: Option<chrono::DateTime<chrono::Utc>>,
    /// Error count since last healthy state.
    pub error_count: u64,
    /// Adapter-specific metrics.
    pub metrics: serde_json::Value,
}

impl AdapterHealth {
    /// Create a healthy status.
    pub fn ok(message: impl Into<String>) -> Self {
        Self {
            healthy: true,
            message: message.into(),
            last_success: Some(chrono::Utc::now()),
            error_count: 0,
            metrics: serde_json::json!({}),
        }
    }

    /// Create an unhealthy status.
    pub fn unhealthy(message: impl Into<String>, error_count: u64) -> Self {
        Self {
            healthy: false,
            message: message.into(),
            last_success: None,
            error_count,
            metrics: serde_json::json!({}),
        }
    }
}

/// Error type for integration adapter operations.
#[derive(Debug, thiserror::Error)]
pub enum IntegrationError {
    #[error("Action not supported: {action}")]
    UnsupportedAction { action: String },

    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Authentication required: {0}")]
    AuthRequired(String),

    #[error("Rate limited: retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },

    #[error("Timeout after {timeout_ms}ms")]
    Timeout { timeout_ms: u64 },

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("External service error: {0}")]
    External(String),
}

/// An event from a push-based integration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntegrationEvent {
    /// Source adapter ID.
    pub adapter_id: String,
    /// Event type (e.g., "mqtt_message", "webhook_received", "health_sync").
    pub event_type: String,
    /// Event payload.
    pub payload: serde_json::Value,
    /// Timestamp of the event.
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Capability declaration for an integration adapter.
///
/// Mirrors the `SkillCapability` contract so adapters automatically
/// register in the capability index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterCapability {
    /// Action this adapter can perform.
    pub action: String,
    /// Human-readable description.
    pub description: String,
    /// Whether this is a push-based (event-driven) capability.
    pub event_driven: bool,
    /// Estimated latency in milliseconds.
    pub estimated_latency_ms: u64,
}

impl AdapterCapability {
    pub fn new(action: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            action: action.into(),
            description: description.into(),
            event_driven: false,
            estimated_latency_ms: 1000,
        }
    }

    pub fn event_driven(mut self) -> Self {
        self.event_driven = true;
        self
    }
}

/// The standard integration adapter trait.
///
/// Implementing this trait is all that's needed to add a new external
/// integration. The adapter registers its capabilities with the skill
/// system and its events with the bus automatically.
///
/// This single trait replaces the need for separate architectural efforts
/// for Pub/Sub bridges (T-01), Object Storage (T-02), IoT/MQTT (T-03),
/// Health APIs (T-04), and Camera capture (T-05). Each becomes an
/// implementation that plugs into the platform.
#[async_trait]
pub trait IntegrationAdapter: Send + Sync + 'static {
    /// Unique adapter identifier (e.g., "mqtt", "oura", "s3", "gmail-pubsub").
    fn id(&self) -> &str;

    /// Human-readable display name.
    fn display_name(&self) -> &str;

    /// Capabilities this adapter provides.
    fn capabilities(&self) -> Vec<AdapterCapability>;

    /// Execute a request-response action.
    ///
    /// The `action` parameter matches one of the declared capabilities.
    /// The `input` is action-specific structured data.
    async fn execute(
        &self,
        action: &str,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, IntegrationError>;

    /// Subscribe to events from this integration (for push-based).
    ///
    /// The adapter sends events to the provided `tx` channel. The caller
    /// (typically the bus bridge) routes these to the EventBus.
    ///
    /// Returns `Ok(())` if subscription was established. The subscription
    /// remains active until the sender is dropped.
    async fn subscribe(
        &self,
        event_type: &str,
        tx: mpsc::Sender<IntegrationEvent>,
    ) -> Result<(), IntegrationError>;

    /// Health check — verify the adapter can connect and operate.
    async fn health(&self) -> AdapterHealth;

    /// Graceful shutdown — clean up resources, close connections.
    async fn shutdown(&self) {}
}

/// Registry for integration adapters.
///
/// Provides O(1) lookup by adapter ID and action name.
pub struct IntegrationAdapterRegistry {
    adapters: std::collections::HashMap<String, Box<dyn IntegrationAdapter>>,
    /// Inverted index: action → adapter ID
    action_index: std::collections::HashMap<String, Vec<String>>,
}

impl IntegrationAdapterRegistry {
    pub fn new() -> Self {
        Self {
            adapters: std::collections::HashMap::new(),
            action_index: std::collections::HashMap::new(),
        }
    }

    /// Register an adapter and index its capabilities.
    pub fn register(&mut self, adapter: Box<dyn IntegrationAdapter>) {
        let id = adapter.id().to_string();
        let capabilities = adapter.capabilities();

        for cap in &capabilities {
            self.action_index
                .entry(cap.action.clone())
                .or_default()
                .push(id.clone());
        }

        self.adapters.insert(id, adapter);
    }

    /// Find an adapter by ID.
    pub fn get(&self, id: &str) -> Option<&dyn IntegrationAdapter> {
        self.adapters.get(id).map(|a| a.as_ref())
    }

    /// Find adapters that can handle a given action.
    pub fn find_by_action(&self, action: &str) -> Vec<&dyn IntegrationAdapter> {
        self.action_index
            .get(action)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| self.adapters.get(id).map(|a| a.as_ref()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// List all registered adapter IDs.
    pub fn list(&self) -> Vec<&str> {
        self.adapters.keys().map(|s| s.as_str()).collect()
    }

    /// Remove and shut down an adapter.
    pub async fn unregister(&mut self, id: &str) {
        if let Some(adapter) = self.adapters.remove(id) {
            adapter.shutdown().await;
            // Remove from action index
            for entries in self.action_index.values_mut() {
                entries.retain(|a| a != id);
            }
            self.action_index.retain(|_, v| !v.is_empty());
        }
    }

    /// Run health checks on all adapters.
    pub async fn health_all(&self) -> Vec<(String, AdapterHealth)> {
        let mut results = Vec::with_capacity(self.adapters.len());
        for (id, adapter) in &self.adapters {
            let health = adapter.health().await;
            results.push((id.clone(), health));
        }
        results
    }
}

impl Default for IntegrationAdapterRegistry {
    fn default() -> Self {
        Self::new()
    }
}
