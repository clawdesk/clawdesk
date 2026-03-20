//! # clawdesk-sandbox
//!
//! Multi-modal sandbox isolation for ClawDesk agent tool execution.
//!
//! Provides sandbox runtimes unified behind a single `Sandbox` trait:
//! - **Docker**: OS-level namespace isolation for shell/binary execution
//! - **Subprocess**: Environment-sanitized process spawning (no Docker required)
//! - **Workspace**: Filesystem path confinement with symlink escape prevention
//!
//! ## Architecture
//!
//! ```text
//! IsolationLevel::FullSandbox   → DockerSandbox (if feature "sandbox-docker") or SubprocessSandbox fallback
//! IsolationLevel::ProcessIso    → DockerSandbox (if feature "sandbox-docker") or SubprocessSandbox
//! IsolationLevel::PathScope     → WorkspaceSandbox
//! IsolationLevel::None          → pass-through (no isolation)
//! ```
//!
//! The `SandboxDispatcher` selects the appropriate runtime via O(1) enum dispatch.

pub mod attestation;
pub mod capability_gate;
pub mod confinement;
pub mod dispatch;
#[cfg(feature = "sandbox-docker")]
pub mod docker;
pub mod pty;
pub mod seccomp;
pub mod session;
pub mod subprocess;
#[cfg(feature = "sandbox-wasm")]
pub mod wasm;
pub mod wasm_runtime;
pub mod workspace;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

// Re-exports
pub use capability_gate::{
    CapabilityGate, CapabilitySet, CachedGrant, EffectivePermission, GateVerdict,
    PermissionGrantCache, ToolCapabilityMap, caps, profiles,
};
pub use dispatch::SandboxDispatcher;
pub use subprocess::SubprocessSandbox;
#[cfg(feature = "sandbox-wasm")]
pub use wasm::{WasmConfig, WasmModuleInfo, WasmModuleRegistry, WasmSandbox};
pub use workspace::WorkspaceSandbox;

#[cfg(feature = "sandbox-docker")]
pub use docker::DockerSandbox;

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// Canonical isolation level from clawdesk-types.
pub use clawdesk_types::IsolationLevel;

/// Resource limits for sandboxed execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    /// Maximum CPU time in seconds
    pub cpu_time_secs: u64,
    /// Maximum wall-clock time in seconds
    pub wall_time_secs: u64,
    /// Maximum memory in bytes
    pub memory_bytes: u64,
    /// Maximum number of open file descriptors
    pub max_fds: u32,
    /// Maximum output size in bytes
    pub max_output_bytes: u64,
    /// Maximum number of processes/threads
    pub max_processes: u32,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            cpu_time_secs: 30,
            wall_time_secs: 60,
            memory_bytes: 256 * 1024 * 1024, // 256 MiB
            max_fds: 64,
            max_output_bytes: 10 * 1024 * 1024, // 10 MiB
            max_processes: 10,
        }
    }
}

/// Request to execute code in a sandbox
#[derive(Debug, Clone)]
pub struct SandboxRequest {
    /// Unique execution ID
    pub execution_id: String,
    /// Name of the tool being executed (used for capability gate checks)
    pub tool_name: String,
    /// The command or code to execute
    pub command: SandboxCommand,
    /// Resource limits
    pub limits: ResourceLimits,
    /// Working directory (within workspace)
    pub working_dir: Option<PathBuf>,
    /// Environment variables to pass through
    pub env: HashMap<String, String>,
    /// Whether network access is allowed
    pub network_allowed: bool,
    /// Workspace root for path confinement
    pub workspace_root: PathBuf,
}

/// What to execute in the sandbox
#[derive(Debug, Clone)]
pub enum SandboxCommand {
    /// Execute a shell command
    Shell {
        command: String,
        args: Vec<String>,
    },
    /// Execute in a Docker container
    #[cfg(feature = "sandbox-docker")]
    Docker {
        image: String,
        command: String,
        args: Vec<String>,
    },
    /// Read/write a file (workspace-confined)
    FileOperation {
        operation: FileOp,
        path: PathBuf,
        content: Option<Vec<u8>>,
    },
}

/// File operations for workspace sandbox
#[derive(Debug, Clone)]
pub enum FileOp {
    Read,
    Write,
    List,
    Delete,
    Exists,
}

/// Result of sandbox execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxResult {
    /// Exit code (0 = success)
    pub exit_code: i32,
    /// Standard output
    pub stdout: String,
    /// Standard error
    pub stderr: String,
    /// Execution duration
    pub duration: Duration,
    /// Resource usage metrics
    pub resource_usage: ResourceUsage,
}

/// Measured resource usage after execution
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResourceUsage {
    pub cpu_time_ms: u64,
    pub wall_time_ms: u64,
    pub peak_memory_bytes: u64,
    pub output_bytes: u64,
}

/// Errors from sandbox execution
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("sandbox not available: {0}")]
    NotAvailable(String),

    #[error("resource limit exceeded: {0}")]
    ResourceLimitExceeded(String),

    #[error("execution timeout after {0:?}")]
    Timeout(Duration),

    #[error("path escape attempt: {path} resolves outside workspace")]
    PathEscape { path: String },

    #[error("command injection detected: {pattern}")]
    CommandInjection { pattern: String },

    #[error("sandbox execution failed: {0}")]
    ExecutionFailed(String),

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

// ---------------------------------------------------------------------------
// Sandbox trait — unified interface for all runtimes
// ---------------------------------------------------------------------------

/// Unified sandbox execution trait.
///
/// Each sandbox runtime implements this trait. The `SandboxDispatcher`
/// selects the appropriate implementation based on `IsolationLevel`.
#[async_trait]
pub trait Sandbox: Send + Sync + std::fmt::Debug {
    /// Human-readable name of this sandbox runtime
    fn name(&self) -> &str;

    /// The isolation level this sandbox provides
    fn isolation_level(&self) -> IsolationLevel;

    /// Check if this sandbox is available on the current platform
    async fn is_available(&self) -> bool;

    /// Execute a request in this sandbox
    async fn execute(&self, request: SandboxRequest) -> Result<SandboxResult, SandboxError>;

    /// Clean up any resources (containers, temp files, etc.)
    async fn cleanup(&self) -> Result<(), SandboxError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolation_level_ordering() {
        assert!(IsolationLevel::None < IsolationLevel::PathScope);
        assert!(IsolationLevel::PathScope < IsolationLevel::ProcessIsolation);
        assert!(IsolationLevel::ProcessIsolation < IsolationLevel::FullSandbox);
    }

    #[test]
    fn default_resource_limits() {
        let limits = ResourceLimits::default();
        assert_eq!(limits.cpu_time_secs, 30);
        assert_eq!(limits.memory_bytes, 256 * 1024 * 1024);
    }
}
