//! # clawdesk-sandbox
//!
//! Multi-modal sandbox isolation for ClawDesk agent tool execution.
//!
//! Provides 4 sandbox runtimes unified behind a single `Sandbox` trait:
//! - **WASM** (Wasmtime): Memory-safe, capability-based isolation for untrusted plugins
//! - **Docker**: OS-level namespace isolation for shell/binary execution
//! - **Subprocess**: Environment-sanitized process spawning (no Docker required)
//! - **Workspace**: Filesystem path confinement with symlink escape prevention
//!
//! ## Architecture
//!
//! ```text
//! IsolationLevel::FullSandbox   → WasmSandbox (if feature "sandbox-wasm")
//! IsolationLevel::ProcessIso    → DockerSandbox (if feature "sandbox-docker") or SubprocessSandbox
//! IsolationLevel::PathScope     → WorkspaceSandbox
//! IsolationLevel::None          → pass-through (no isolation)
//! ```
//!
//! The `SandboxDispatcher` selects the appropriate runtime via O(1) enum dispatch.

pub mod dispatch;
#[cfg(feature = "sandbox-docker")]
pub mod docker;
pub mod subprocess;
#[cfg(feature = "sandbox-wasm")]
pub mod wasm;
pub mod workspace;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

// Re-exports
pub use dispatch::SandboxDispatcher;
pub use subprocess::SubprocessSandbox;
pub use workspace::WorkspaceSandbox;

#[cfg(feature = "sandbox-docker")]
pub use docker::DockerSandbox;
#[cfg(feature = "sandbox-wasm")]
pub use wasm::WasmSandbox;

// ---------------------------------------------------------------------------
// Core types
// ---------------------------------------------------------------------------

/// Isolation levels matching the lattice in clawdesk-security/sandbox_policy.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum IsolationLevel {
    /// No isolation — direct execution in host process
    None = 0,
    /// Filesystem path confinement only
    PathScope = 1,
    /// Process-level isolation (subprocess or Docker)
    ProcessIsolation = 2,
    /// Full sandbox (WASM linear memory isolation)
    FullSandbox = 3,
}

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
    /// Execute a WASM module
    #[cfg(feature = "sandbox-wasm")]
    Wasm {
        module_bytes: Vec<u8>,
        function: String,
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
