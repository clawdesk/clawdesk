//! WASM sandbox runtime — Wasmtime-based memory-safe isolation.
//!
//! Provides capability-based isolation via WebAssembly linear memory.
//! Each module runs in its own 32-bit address space with fuel-based CPU metering.
//! Feature-gated behind `sandbox-wasm`.

use crate::{
    IsolationLevel, ResourceLimits, ResourceUsage, Sandbox, SandboxCommand, SandboxError,
    SandboxRequest, SandboxResult,
};
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Instant;
use tracing::{debug, info, warn};
use wasmtime::*;

/// Estimated WASM instructions per second (conservative).
const INSTRUCTIONS_PER_SEC: u64 = 1_000_000_000;

/// Host state stored in each Wasmtime Store.
struct HostState {
    /// Resource limiter for memory bounds
    limiter: StoreLimits,
    /// Captured stdout
    stdout: Vec<u8>,
    /// Captured stderr
    stderr: Vec<u8>,
}

/// WASM sandbox runtime.
///
/// Uses Wasmtime with fuel-based CPU metering, bounded memory,
/// and explicit host-function capability grants.
#[derive(Debug)]
pub struct WasmSandbox {
    /// Shared Wasmtime engine (compiled once, reused)
    engine: Engine,
    /// Module cache: SHA-256(bytes) → compiled Module
    module_cache: dashmap::DashMap<String, Module>,
}

impl WasmSandbox {
    /// Create a new WASM sandbox with default engine configuration.
    pub fn new() -> Result<Self, SandboxError> {
        let mut config = Config::new();
        config.consume_fuel(true);
        config.wasm_bulk_memory(true);
        config.wasm_multi_value(true);

        let engine = Engine::new(&config)
            .map_err(|e| SandboxError::InvalidConfig(format!("wasmtime engine: {}", e)))?;

        info!("WASM sandbox initialized with fuel-based metering");

        Ok(Self {
            engine,
            module_cache: dashmap::DashMap::new(),
        })
    }

    /// Compute fuel budget from resource limits.
    fn compute_fuel(limits: &ResourceLimits) -> u64 {
        limits.cpu_time_secs.saturating_mul(INSTRUCTIONS_PER_SEC)
    }

    /// Get or compile a WASM module (cached by content hash).
    fn get_or_compile_module(&self, bytes: &[u8]) -> Result<Module, SandboxError> {
        let hash = {
            use sha2::{Digest, Sha256};
            let digest = Sha256::digest(bytes);
            hex::encode(digest)
        };

        if let Some(module) = self.module_cache.get(&hash) {
            debug!(hash = %hash, "using cached WASM module");
            return Ok(module.clone());
        }

        debug!(hash = %hash, size = bytes.len(), "compiling WASM module");
        let module = Module::new(&self.engine, bytes)
            .map_err(|e| SandboxError::ExecutionFailed(format!("WASM compilation: {}", e)))?;

        self.module_cache.insert(hash, module.clone());
        Ok(module)
    }

    /// Create a store with resource limits and fuel budget.
    fn create_store(&self, limits: &ResourceLimits) -> Store<HostState> {
        let store_limits = StoreLimitsBuilder::new()
            .memory_size(limits.memory_bytes as usize)
            .instances(10)
            .tables(10)
            .memories(1)
            .build();

        let host_state = HostState {
            limiter: store_limits,
            stdout: Vec::new(),
            stderr: Vec::new(),
        };

        let mut store = Store::new(&self.engine, host_state);
        store.limiter(|state| &mut state.limiter);

        // Set fuel budget
        let fuel = Self::compute_fuel(limits);
        store.set_fuel(fuel).ok();

        store
    }
}

#[async_trait]
impl Sandbox for WasmSandbox {
    fn name(&self) -> &str {
        "wasm"
    }

    fn isolation_level(&self) -> IsolationLevel {
        IsolationLevel::FullSandbox
    }

    async fn is_available(&self) -> bool {
        true // If compiled with the feature, it's available
    }

    async fn execute(&self, request: SandboxRequest) -> Result<SandboxResult, SandboxError> {
        let start = Instant::now();

        let (module_bytes, function, _args) = match &request.command {
            SandboxCommand::Wasm {
                module_bytes,
                function,
                args,
            } => (module_bytes, function, args),
            _ => {
                return Err(SandboxError::InvalidConfig(
                    "WASM sandbox only handles WASM commands".into(),
                ))
            }
        };

        // Compile or retrieve cached module
        let module = self.get_or_compile_module(module_bytes)?;

        // Create store with limits
        let mut store = self.create_store(&request.limits);

        // Create linker with host functions
        let mut linker = Linker::new(&self.engine);

        // Host function: fd_write (basic stdout capture)
        linker
            .func_wrap(
                "wasi_snapshot_preview1",
                "fd_write",
                |mut caller: Caller<'_, HostState>,
                 fd: i32,
                 iovs_ptr: i32,
                 iovs_len: i32,
                 nwritten_ptr: i32|
                 -> i32 {
                    let memory = match caller.get_export("memory") {
                        Some(Extern::Memory(m)) => m,
                        _ => return -1,
                    };

                    let mut total_written = 0u32;
                    for i in 0..iovs_len {
                        let iov_offset = (iovs_ptr + i * 8) as usize;
                        let data = memory.data(&caller);

                        if iov_offset + 8 > data.len() {
                            return -1;
                        }

                        let buf_ptr =
                            u32::from_le_bytes(data[iov_offset..iov_offset + 4].try_into().unwrap())
                                as usize;
                        let buf_len = u32::from_le_bytes(
                            data[iov_offset + 4..iov_offset + 8].try_into().unwrap(),
                        ) as usize;

                        if buf_ptr + buf_len > data.len() {
                            return -1;
                        }

                        let bytes = data[buf_ptr..buf_ptr + buf_len].to_vec();
                        total_written += buf_len as u32;

                        let state = caller.data_mut();
                        match fd {
                            1 => state.stdout.extend_from_slice(&bytes),
                            2 => state.stderr.extend_from_slice(&bytes),
                            _ => {}
                        }
                    }

                    // Write nwritten
                    let data = memory.data_mut(&mut caller);
                    let nw_offset = nwritten_ptr as usize;
                    if nw_offset + 4 <= data.len() {
                        data[nw_offset..nw_offset + 4]
                            .copy_from_slice(&total_written.to_le_bytes());
                    }

                    0 // success
                },
            )
            .map_err(|e| SandboxError::ExecutionFailed(format!("linker fd_write: {}", e)))?;

        // Stub out other WASI functions
        for func_name in &[
            "args_get",
            "args_sizes_get",
            "environ_get",
            "environ_sizes_get",
            "clock_time_get",
            "proc_exit",
            "random_get",
        ] {
            let name = *func_name;
            linker
                .func_wrap("wasi_snapshot_preview1", name, || -> i32 { 0 })
                .ok(); // Ignore if already defined
        }

        // Instantiate
        let instance = linker.instantiate(&mut store, &module).map_err(|e| {
            SandboxError::ExecutionFailed(format!("WASM instantiation: {}", e))
        })?;

        // Find and call the target function
        let func = instance.get_func(&mut store, function).ok_or_else(|| {
            SandboxError::ExecutionFailed(format!(
                "function '{}' not found in WASM module",
                function
            ))
        })?;

        // Execute with wall-clock timeout
        let timeout = std::time::Duration::from_secs(request.limits.wall_time_secs);
        let exec_result = tokio::time::timeout(timeout, async {
            // WASM execution is synchronous — run in blocking thread
            tokio::task::spawn_blocking({
                let func_type = func.ty(&store);
                move || {
                    let mut results = vec![Val::I32(0); func_type.results().len()];
                    let params: Vec<Val> = func_type
                        .params()
                        .map(|_| Val::I32(0))
                        .collect();

                    match func.call(&mut store, &params, &mut results) {
                        Ok(()) => {
                            let exit_code = results
                                .first()
                                .and_then(|v| v.i32())
                                .unwrap_or(0);
                            let state = store.data();
                            Ok(SandboxResult {
                                exit_code,
                                stdout: String::from_utf8_lossy(&state.stdout).to_string(),
                                stderr: String::from_utf8_lossy(&state.stderr).to_string(),
                                duration: start.elapsed(),
                                resource_usage: ResourceUsage {
                                    wall_time_ms: start.elapsed().as_millis() as u64,
                                    output_bytes: state.stdout.len() as u64,
                                    ..Default::default()
                                },
                            })
                        }
                        Err(e) => {
                            if e.to_string().contains("fuel") {
                                Err(SandboxError::ResourceLimitExceeded(
                                    "CPU fuel exhausted".into(),
                                ))
                            } else {
                                Err(SandboxError::ExecutionFailed(format!("WASM trap: {}", e)))
                            }
                        }
                    }
                }
            })
            .await
            .map_err(|e| SandboxError::ExecutionFailed(format!("task join: {}", e)))?
        })
        .await;

        match exec_result {
            Ok(result) => result,
            Err(_) => Err(SandboxError::Timeout(timeout)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuel_computation() {
        let limits = ResourceLimits {
            cpu_time_secs: 5,
            ..Default::default()
        };
        assert_eq!(WasmSandbox::compute_fuel(&limits), 5_000_000_000);
    }

    #[test]
    fn wasm_sandbox_creation() {
        let sandbox = WasmSandbox::new();
        assert!(sandbox.is_ok());
    }
}
