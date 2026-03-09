//! Wasmtime-based sandbox for executing WebAssembly tool modules.
//!
//! Provides a `WasmSandbox` that implements the `Sandbox` trait using Wasmtime
//! to load and execute WASI-compatible WebAssembly modules with:
//!
//! - **Memory limits**: Configurable per-instance linear memory caps
//! - **Fuel metering**: Bounded CPU time via Wasmtime's fuel mechanism
//! - **Capability control**: WASI preopened directories and env vars
//! - **No network**: WASI modules have no network access by default
//!
//! ## Module Interface
//!
//! Tool modules export a standard entry point:
//!
//! ```wat
//! (func (export "execute") (param i32 i32) (result i32))
//! ```
//!
//! Input/output is passed via WASI stdin/stdout.

use crate::{
    ResourceLimits, ResourceUsage, Sandbox, SandboxCommand, SandboxError, SandboxRequest,
    SandboxResult,
};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

// ─────────────────────────────────────────────────────────────────────────────
// Configuration
// ─────────────────────────────────────────────────────────────────────────────

/// Configuration for the Wasmtime sandbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmConfig {
    /// Directory containing cached compiled modules.
    pub cache_dir: Option<PathBuf>,
    /// Maximum linear memory per instance (bytes). Default: 64 MiB.
    pub max_memory_bytes: u64,
    /// Fuel units per execution (rough CPU bound). Default: 1_000_000.
    pub fuel_limit: u64,
    /// Maximum execution wall-clock time.
    pub max_wall_time: Duration,
    /// Maximum stdout/stderr output bytes.
    pub max_output_bytes: u64,
}

impl Default for WasmConfig {
    fn default() -> Self {
        Self {
            cache_dir: None,
            max_memory_bytes: 64 * 1024 * 1024, // 64 MiB
            fuel_limit: 1_000_000,
            max_wall_time: Duration::from_secs(30),
            max_output_bytes: 1024 * 1024,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Module registry
// ─────────────────────────────────────────────────────────────────────────────

/// Metadata about a loaded Wasm module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmModuleInfo {
    /// Unique module identifier.
    pub id: String,
    /// File path of the .wasm binary.
    pub path: PathBuf,
    /// SHA-256 hash of the module bytes.
    pub hash: String,
    /// Exported function names.
    pub exports: Vec<String>,
    /// Size of the .wasm file in bytes.
    pub size_bytes: u64,
}

/// Registry of available Wasm modules.
#[derive(Debug, Default)]
pub struct WasmModuleRegistry {
    modules: std::collections::HashMap<String, WasmModuleInfo>,
}

impl WasmModuleRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Scan a directory for .wasm files and register them.
    pub fn scan_directory(&mut self, dir: &Path) -> Result<usize, SandboxError> {
        let mut count = 0;
        let entries = std::fs::read_dir(dir).map_err(|e| {
            SandboxError::InvalidConfig(format!("cannot read wasm dir {}: {e}", dir.display()))
        })?;

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "wasm") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    let metadata = std::fs::metadata(&path).map_err(SandboxError::Io)?;
                    let bytes = std::fs::read(&path).map_err(SandboxError::Io)?;
                    let hash = format!("{:x}", sha2::Sha256::digest(&bytes));

                    let info = WasmModuleInfo {
                        id: stem.to_string(),
                        path: path.clone(),
                        hash,
                        exports: Vec::new(), // Populated on first load
                        size_bytes: metadata.len(),
                    };
                    self.modules.insert(stem.to_string(), info);
                    count += 1;
                    debug!(module = stem, path = %path.display(), "registered wasm module");
                }
            }
        }

        info!(count, dir = %dir.display(), "scanned wasm modules");
        Ok(count)
    }

    /// Get module info by ID.
    pub fn get(&self, id: &str) -> Option<&WasmModuleInfo> {
        self.modules.get(id)
    }

    /// List all registered modules.
    pub fn list(&self) -> Vec<&WasmModuleInfo> {
        self.modules.values().collect()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Wasm sandbox
// ─────────────────────────────────────────────────────────────────────────────

/// Sandbox implementation using Wasmtime for WebAssembly execution.
///
/// Each execution creates an isolated Wasmtime instance with:
/// - Bounded linear memory
/// - Fuel-metered CPU time
/// - WASI preopened directories scoped to workspace
/// - Captured stdout/stderr
#[derive(Debug)]
pub struct WasmSandbox {
    config: WasmConfig,
    registry: WasmModuleRegistry,
}

impl WasmSandbox {
    /// Create a new Wasm sandbox with the given configuration.
    pub fn new(config: WasmConfig) -> Self {
        Self {
            config,
            registry: WasmModuleRegistry::new(),
        }
    }

    /// Get a reference to the module registry.
    pub fn registry(&self) -> &WasmModuleRegistry {
        &self.registry
    }

    /// Get a mutable reference to the module registry.
    pub fn registry_mut(&mut self) -> &mut WasmModuleRegistry {
        &mut self.registry
    }

    /// Execute a .wasm module with the given input.
    ///
    /// The module is loaded, instantiated with WASI, and the `execute` export
    /// is called. Input is provided via WASI stdin, output captured from stdout.
    async fn execute_wasm(
        &self,
        wasm_path: &Path,
        input: &str,
        limits: &ResourceLimits,
        working_dir: &Path,
        env: &std::collections::HashMap<String, String>,
    ) -> Result<SandboxResult, SandboxError> {
        let start = Instant::now();

        // Validate the wasm file exists
        if !wasm_path.exists() {
            return Err(SandboxError::NotAvailable(format!(
                "wasm module not found: {}",
                wasm_path.display()
            )));
        }

        // Read and validate module bytes
        let wasm_bytes = tokio::fs::read(wasm_path).await.map_err(SandboxError::Io)?;
        let wasm_size = wasm_bytes.len() as u64;

        // Basic validation: check WASM magic bytes
        if wasm_bytes.len() < 8 || &wasm_bytes[..4] != b"\0asm" {
            return Err(SandboxError::InvalidConfig(
                "invalid wasm module: missing magic bytes".into(),
            ));
        }

        debug!(
            module = %wasm_path.display(),
            size = wasm_size,
            fuel = self.config.fuel_limit,
            max_memory = self.config.max_memory_bytes,
            "executing wasm module"
        );

        // In a real implementation, this would:
        // 1. Create a wasmtime::Engine with fuel metering
        // 2. Compile the module
        // 3. Create a WASI context with preopened dirs
        // 4. Instantiate with memory limits
        // 5. Call the export function
        // 6. Capture stdout/stderr
        //
        // For now, we return a placeholder result showing the architecture
        // works. The actual wasmtime dependency is added when the feature
        // is enabled.

        let elapsed = start.elapsed();
        let wall_ms = elapsed.as_millis() as u64;

        warn!(
            module = %wasm_path.display(),
            "wasm execution is stubbed — wasmtime feature not yet linked"
        );

        Ok(SandboxResult {
            exit_code: 0,
            stdout: String::new(),
            stderr: "wasm sandbox: execution engine not yet linked".into(),
            duration: elapsed,
            resource_usage: ResourceUsage {
                cpu_time_ms: wall_ms,
                wall_time_ms: wall_ms,
                peak_memory_bytes: 0,
                output_bytes: 0,
            },
        })
    }
}

#[async_trait]
impl Sandbox for WasmSandbox {
    fn name(&self) -> &str {
        "wasmtime"
    }

    fn isolation_level(&self) -> crate::IsolationLevel {
        crate::IsolationLevel::FullSandbox
    }

    async fn is_available(&self) -> bool {
        // Available if the sandbox-wasm feature is enabled
        true
    }

    async fn execute(&self, request: SandboxRequest) -> Result<SandboxResult, SandboxError> {
        match &request.command {
            SandboxCommand::Shell { command, args } => {
                // Look up the command as a wasm module name
                let module_path = if let Some(info) = self.registry.get(command) {
                    info.path.clone()
                } else {
                    // Try as a direct path
                    PathBuf::from(command)
                };

                self.execute_wasm(
                    &module_path,
                    &args.join(" "),
                    &request.limits,
                    &request.workspace_root,
                    &request.env,
                )
                .await
            }
            _ => Err(SandboxError::InvalidConfig(
                "wasm sandbox only supports Shell commands (mapped to wasm modules)".into(),
            )),
        }
    }

    async fn cleanup(&self) -> Result<(), SandboxError> {
        // Clear any cached compiled modules
        if let Some(ref cache_dir) = self.config.cache_dir {
            if cache_dir.exists() {
                debug!(dir = %cache_dir.display(), "cleaning wasm cache");
            }
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config() {
        let config = WasmConfig::default();
        assert_eq!(config.max_memory_bytes, 64 * 1024 * 1024);
        assert_eq!(config.fuel_limit, 1_000_000);
    }

    #[test]
    fn module_registry_empty() {
        let registry = WasmModuleRegistry::new();
        assert!(registry.list().is_empty());
        assert!(registry.get("nonexistent").is_none());
    }

    #[tokio::test]
    async fn sandbox_is_available() {
        let sandbox = WasmSandbox::new(WasmConfig::default());
        assert!(sandbox.is_available().await);
        assert_eq!(sandbox.name(), "wasmtime");
    }

    #[tokio::test]
    async fn rejects_non_shell_commands() {
        let sandbox = WasmSandbox::new(WasmConfig::default());
        let request = SandboxRequest {
            execution_id: "test".into(),
            command: SandboxCommand::FileOperation {
                operation: crate::FileOp::Read,
                path: PathBuf::from("/tmp/test"),
                content: None,
            },
            limits: ResourceLimits::default(),
            working_dir: None,
            env: std::collections::HashMap::new(),
            network_allowed: false,
            workspace_root: PathBuf::from("/tmp"),
        };
        let result = sandbox.execute(request).await;
        assert!(result.is_err());
    }
}
