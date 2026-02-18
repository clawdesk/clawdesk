//! Plugin system types — lifecycle, capabilities, and SDK contracts.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// Plugin manifest declaring identity and capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub author: String,
    /// Minimum SDK version required.
    pub min_sdk_version: String,
    /// Declared dependencies (plugin names).
    pub dependencies: Vec<String>,
    /// Capabilities this plugin provides.
    pub capabilities: PluginCapabilities,
}

/// What a plugin can register.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginCapabilities {
    pub tools: Vec<String>,
    pub hooks: Vec<String>,
    pub channels: Vec<String>,
    pub providers: Vec<String>,
    pub commands: Vec<String>,
    pub http_routes: Vec<String>,
    pub gateway_methods: Vec<String>,
    /// Optional slot this plugin occupies.
    ///
    /// Slots enforce mutual exclusion: only one plugin can occupy a given slot
    /// at a time (e.g., `"tts"`, `"stt"`, `"search"`). When a new plugin claims
    /// a slot, the previous occupant is deactivated (compare-and-swap semantics).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot: Option<String>,
}

/// Plugin lifecycle state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginState {
    /// Discovered but not loaded.
    Discovered,
    /// Manifest loaded and validated.
    Loaded,
    /// Dependencies resolved, ready to activate.
    Resolved,
    /// Plugin is running.
    Active,
    /// Plugin encountered an error.
    Failed,
    /// Plugin was explicitly disabled.
    Disabled,
    /// Plugin is being unloaded.
    Stopping,
}

/// Plugin source — where was it discovered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PluginSource {
    /// Bundled with ClawDesk binary.
    Bundled,
    /// Installed globally.
    Global { path: String },
    /// Workspace-local plugin.
    Workspace { path: String },
    /// Loaded from config reference.
    Config { path: String },
}

/// Security capability grant for a plugin.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PluginCapabilityGrant {
    /// Read files from specified paths.
    FileRead(Vec<String>),
    /// Write files to specified paths.
    FileWrite(Vec<String>),
    /// Network access to specified hosts.
    Network(Vec<String>),
    /// Access to config keys.
    ConfigRead(Vec<String>),
    /// Execute shell commands.
    ShellExec,
    /// Full access (trusted plugins only).
    Full,
}

/// Resource limits for plugin execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginResourceLimits {
    /// Maximum heap memory in bytes.
    pub max_memory_bytes: u64,
    /// Maximum CPU time per operation in milliseconds.
    pub max_cpu_ms: u64,
    /// Maximum concurrent async tasks.
    pub max_tasks: u32,
    /// Maximum file descriptors.
    pub max_fds: u32,
}

impl Default for PluginResourceLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: 128 * 1024 * 1024, // 128MB
            max_cpu_ms: 30_000,                   // 30s
            max_tasks: 64,
            max_fds: 256,
        }
    }
}

/// Runtime information about a loaded plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInfo {
    pub manifest: PluginManifest,
    pub source: PluginSource,
    pub state: PluginState,
    pub grants: HashSet<PluginCapabilityGrant>,
    pub resource_limits: PluginResourceLimits,
    pub load_time_ms: u64,
    pub error: Option<String>,
}
