//! Plugin SDK — type-safe interface for external plugin authors.
//!
//! Provides the trait definitions, context types, and helper macros that
//! plugin authors use to build ClawDesk extensions. This is the public API
//! surface — internal crates depend on the host/sandbox/resolver modules,
//! but external plugins only interact through this SDK.
//!
//! ## Plugin Lifecycle
//! 1. Plugin implements `ClawDeskPlugin` trait
//! 2. Host discovers and loads the plugin  
//! 3. `on_load()` is called with a `PluginContext`
//! 4. Plugin registers handlers for specific events
//! 5. `on_activate()` is called when the plugin is enabled
//! 6. Plugin receives events via registered handlers
//! 7. `on_deactivate()` / `on_unload()` for cleanup
//!
//! ## Capabilities
//! Plugins declare required capabilities in their manifest. The host validates
//! these against the user's grant configuration before activation.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

// ── Plugin Manifest ───────────────────────────────────────

/// Plugin manifest — declarative metadata for discovery and validation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Unique plugin identifier (reverse domain notation recommended).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Plugin version (semver).
    pub version: String,
    /// Short description.
    pub description: String,
    /// Author name.
    pub author: Option<String>,
    /// License identifier (SPDX).
    pub license: Option<String>,
    /// Minimum ClawDesk version required.
    pub min_clawdesk_version: Option<String>,
    /// Required capabilities.
    pub capabilities: Vec<Capability>,
    /// Event types this plugin wants to receive.
    pub subscriptions: Vec<String>,
    /// Configuration schema (JSON Schema).
    pub config_schema: Option<serde_json::Value>,
}

/// Capability that a plugin can request.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Read messages from channels.
    ReadMessages,
    /// Send messages to channels.
    SendMessages,
    /// Access to file system (scoped to plugin data dir).
    FileSystem,
    /// Make outbound HTTP requests.
    Network,
    /// Access user configuration.
    ReadConfig,
    /// Modify user configuration.
    WriteConfig,
    /// Execute shell commands (requires explicit user approval).
    ExecCommands,
    /// Access agent context (current conversation, history).
    AgentContext,
    /// Register custom tools/skills.
    RegisterTools,
    /// Access memory/embedding store.
    MemoryAccess,
    /// Custom capability with a name.
    Custom(String),
}

// ── Plugin Context ────────────────────────────────────────

/// Context provided to plugins for interacting with ClawDesk.
///
/// This is the plugin's "window" into the host — all interactions go through
/// method calls on this context.
#[derive(Clone)]
pub struct PluginContext {
    pub plugin_id: String,
    pub data_dir: String,
    pub config: serde_json::Value,
    services: Arc<PluginServices>,
}

/// Internal services exposed to plugins.
struct PluginServices {
    kv_store: RwLock<HashMap<String, String>>,
    log_buffer: RwLock<Vec<LogEntry>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub level: LogLevel,
    pub message: String,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl PluginContext {
    /// Create a new plugin context.
    pub fn new(plugin_id: impl Into<String>, data_dir: impl Into<String>) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            data_dir: data_dir.into(),
            config: serde_json::Value::Null,
            services: Arc::new(PluginServices {
                kv_store: RwLock::new(HashMap::new()),
                log_buffer: RwLock::new(Vec::new()),
            }),
        }
    }

    /// Set plugin configuration.
    pub fn with_config(mut self, config: serde_json::Value) -> Self {
        self.config = config;
        self
    }

    /// Log a message from the plugin.
    pub async fn log(&self, level: LogLevel, message: impl Into<String>) {
        let entry = LogEntry {
            level,
            message: message.into(),
            timestamp: chrono::Utc::now(),
        };
        debug!(
            plugin = self.plugin_id.as_str(),
            level = ?level,
            msg = entry.message.as_str(),
            "plugin log"
        );
        self.services.log_buffer.write().await.push(entry);
    }

    /// Get a value from the plugin's key-value store.
    pub async fn kv_get(&self, key: &str) -> Option<String> {
        self.services.kv_store.read().await.get(key).cloned()
    }

    /// Set a value in the plugin's key-value store.
    pub async fn kv_set(&self, key: impl Into<String>, value: impl Into<String>) {
        self.services
            .kv_store
            .write()
            .await
            .insert(key.into(), value.into());
    }

    /// Delete a value from the plugin's key-value store.
    pub async fn kv_delete(&self, key: &str) -> bool {
        self.services.kv_store.write().await.remove(key).is_some()
    }

    /// List all keys in the plugin's key-value store.
    pub async fn kv_keys(&self) -> Vec<String> {
        self.services
            .kv_store
            .read()
            .await
            .keys()
            .cloned()
            .collect()
    }

    /// Get recent log entries.
    pub async fn get_logs(&self) -> Vec<LogEntry> {
        self.services.log_buffer.read().await.clone()
    }

    /// Get a config value by JSON pointer (e.g. "/database/host").
    pub fn config_get(&self, pointer: &str) -> Option<&serde_json::Value> {
        self.config.pointer(pointer)
    }
}

// ── Plugin Event ──────────────────────────────────────────

/// Event delivered to plugins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginEvent {
    /// Event type identifier.
    pub event_type: String,
    /// Event payload.
    pub data: serde_json::Value,
    /// Source plugin or system component.
    pub source: String,
    /// Event timestamp.
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Result returned by plugin event handlers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginResponse {
    /// Whether the event was handled.
    pub handled: bool,
    /// Optional response data.
    pub data: Option<serde_json::Value>,
    /// Optional error message.
    pub error: Option<String>,
}

impl PluginResponse {
    pub fn handled() -> Self {
        Self {
            handled: true,
            data: None,
            error: None,
        }
    }

    pub fn handled_with_data(data: serde_json::Value) -> Self {
        Self {
            handled: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn not_handled() -> Self {
        Self {
            handled: false,
            data: None,
            error: None,
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            handled: false,
            data: None,
            error: Some(msg.into()),
        }
    }
}

// ── Plugin Trait ──────────────────────────────────────────

/// The main trait that plugins must implement.
///
/// Lifecycle: `on_load` → `on_activate` → handle events → `on_deactivate` → `on_unload`
#[async_trait]
pub trait ClawDeskPlugin: Send + Sync {
    /// Return the plugin's manifest.
    fn manifest(&self) -> &PluginManifest;

    /// Called when the plugin is loaded. Initialize resources here.
    async fn on_load(&mut self, ctx: PluginContext) -> Result<(), PluginSdkError>;

    /// Called when the plugin is activated (enabled by user).
    async fn on_activate(&mut self) -> Result<(), PluginSdkError> {
        Ok(())
    }

    /// Handle an incoming event.
    async fn on_event(&self, event: PluginEvent) -> PluginResponse {
        let _ = event;
        PluginResponse::not_handled()
    }

    /// Called when the plugin is deactivated (disabled by user).
    async fn on_deactivate(&mut self) -> Result<(), PluginSdkError> {
        Ok(())
    }

    /// Called when the plugin is being unloaded. Cleanup resources here.
    async fn on_unload(&mut self) -> Result<(), PluginSdkError> {
        Ok(())
    }

    /// Called when plugin configuration changes at runtime.
    async fn on_config_change(
        &mut self,
        _new_config: serde_json::Value,
    ) -> Result<(), PluginSdkError> {
        Ok(())
    }
}

/// SDK-level error type for plugin operations.
#[derive(Debug, thiserror::Error)]
pub enum PluginSdkError {
    #[error("initialization failed: {0}")]
    InitFailed(String),
    #[error("missing capability: {0:?}")]
    MissingCapability(Capability),
    #[error("configuration error: {0}")]
    ConfigError(String),
    #[error("runtime error: {0}")]
    RuntimeError(String),
    #[error("version mismatch: plugin requires {required}, got {actual}")]
    VersionMismatch { required: String, actual: String },
}

// ── Plugin Registry Interface ─────────────────────────────

/// Information about a registered plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub state: PluginState,
    pub capabilities: Vec<Capability>,
}

/// Plugin lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginState {
    Discovered,
    Loaded,
    Active,
    Inactive,
    Error,
}

/// Trait for plugin discovery and management.
#[async_trait]
pub trait PluginManager: Send + Sync {
    /// List all known plugins.
    async fn list_plugins(&self) -> Vec<PluginInfo>;

    /// Get info for a specific plugin.
    async fn get_plugin(&self, id: &str) -> Option<PluginInfo>;

    /// Enable a plugin.
    async fn enable_plugin(&self, id: &str) -> Result<(), PluginSdkError>;

    /// Disable a plugin.
    async fn disable_plugin(&self, id: &str) -> Result<(), PluginSdkError>;

    /// Reload a plugin (disable + unload + load + enable).
    async fn reload_plugin(&self, id: &str) -> Result<(), PluginSdkError>;

    /// Install a plugin from a path or URL.
    async fn install_plugin(&self, source: &str) -> Result<PluginInfo, PluginSdkError>;

    /// Uninstall a plugin.
    async fn uninstall_plugin(&self, id: &str) -> Result<(), PluginSdkError>;
}

// ── Helper Types ──────────────────────────────────────────

/// Tool definition that plugins can register.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub returns: Option<String>,
}

/// Tool execution result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: serde_json::Value,
    pub error: Option<String>,
}

// ── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manifest_serialization() {
        let manifest = PluginManifest {
            id: "com.example.test".to_string(),
            name: "Test Plugin".to_string(),
            version: "1.0.0".to_string(),
            description: "A test plugin".to_string(),
            author: Some("Test Author".to_string()),
            license: Some("MIT".to_string()),
            min_clawdesk_version: Some("0.1.0".to_string()),
            capabilities: vec![Capability::ReadMessages, Capability::SendMessages],
            subscriptions: vec!["message.received".to_string()],
            config_schema: None,
        };

        let json = serde_json::to_string(&manifest).unwrap();
        let parsed: PluginManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "com.example.test");
        assert_eq!(parsed.capabilities.len(), 2);
    }

    #[test]
    fn test_plugin_response() {
        let r = PluginResponse::handled();
        assert!(r.handled);
        assert!(r.data.is_none());

        let r = PluginResponse::error("bad");
        assert!(!r.handled);
        assert_eq!(r.error, Some("bad".to_string()));

        let r = PluginResponse::handled_with_data(serde_json::json!({"key": "val"}));
        assert!(r.handled);
        assert!(r.data.is_some());
    }

    #[tokio::test]
    async fn test_plugin_context_kv() {
        let ctx = PluginContext::new("test", "/tmp/test");

        ctx.kv_set("key1", "value1").await;
        assert_eq!(ctx.kv_get("key1").await, Some("value1".to_string()));
        assert_eq!(ctx.kv_get("missing").await, None);

        let keys = ctx.kv_keys().await;
        assert_eq!(keys.len(), 1);

        assert!(ctx.kv_delete("key1").await);
        assert_eq!(ctx.kv_get("key1").await, None);
    }

    #[tokio::test]
    async fn test_plugin_context_logging() {
        let ctx = PluginContext::new("test", "/tmp/test");
        ctx.log(LogLevel::Info, "hello").await;
        ctx.log(LogLevel::Error, "oops").await;

        let logs = ctx.get_logs().await;
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].level, LogLevel::Info);
        assert_eq!(logs[1].message, "oops");
    }

    #[test]
    fn test_config_get() {
        let ctx = PluginContext::new("test", "/tmp/test")
            .with_config(serde_json::json!({
                "database": {
                    "host": "localhost",
                    "port": 5432
                }
            }));

        assert_eq!(
            ctx.config_get("/database/host"),
            Some(&serde_json::json!("localhost"))
        );
        assert_eq!(
            ctx.config_get("/database/port"),
            Some(&serde_json::json!(5432))
        );
        assert_eq!(ctx.config_get("/missing"), None);
    }

    #[test]
    fn test_capability_serde() {
        let cap = Capability::ReadMessages;
        let json = serde_json::to_string(&cap).unwrap();
        assert_eq!(json, "\"read_messages\"");

        let custom = Capability::Custom("gpu_access".to_string());
        let json = serde_json::to_string(&custom).unwrap();
        let parsed: Capability = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, Capability::Custom("gpu_access".to_string()));
    }

    #[test]
    fn test_plugin_state_serde() {
        let state = PluginState::Active;
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, "\"active\"");
    }
}
