//! Sandbox commands — multi-modal code/file execution isolation.
//!
//! Exposes clawdesk-sandbox's `SandboxDispatcher` through the Tauri IPC layer
//! so the frontend can inspect available isolation backends, configure resource
//! limits, and execute sandboxed commands.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

// ── Response types ────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct SandboxStatusInfo {
    pub available: bool,
    pub max_isolation: String,
    pub available_levels: Vec<String>,
    pub default_limits: ResourceLimitsInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimitsInfo {
    pub cpu_time_secs: u64,
    pub wall_time_secs: u64,
    pub memory_bytes: u64,
    pub max_fds: u32,
    pub max_output_bytes: u64,
    pub max_processes: u32,
}

#[derive(Debug, Serialize)]
pub struct SandboxExecResult {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u64,
    pub resource_usage: SandboxResourceUsage,
}

#[derive(Debug, Serialize)]
pub struct SandboxResourceUsage {
    pub cpu_time_ms: u64,
    pub wall_time_ms: u64,
    pub peak_memory_bytes: u64,
    pub output_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct SandboxBackendInfo {
    pub name: String,
    pub isolation_level: String,
    pub available: bool,
}

// ── Commands ──────────────────────────────────────────────────

/// Get the current sandbox system status — available backends and max isolation.
#[tauri::command]
pub async fn get_sandbox_status(
    state: State<'_, AppState>,
) -> Result<SandboxStatusInfo, String> {
    let dispatcher = state.sandbox_dispatcher.read().await;
    let levels: Vec<String> = dispatcher
        .available_levels()
        .iter()
        .map(|l| format!("{:?}", l))
        .collect();
    let max = format!("{:?}", dispatcher.max_available());
    let defaults = clawdesk_sandbox::ResourceLimits::default();
    Ok(SandboxStatusInfo {
        available: !levels.is_empty(),
        max_isolation: max,
        available_levels: levels,
        default_limits: ResourceLimitsInfo {
            cpu_time_secs: defaults.cpu_time_secs,
            wall_time_secs: defaults.wall_time_secs,
            memory_bytes: defaults.memory_bytes,
            max_fds: defaults.max_fds,
            max_output_bytes: defaults.max_output_bytes,
            max_processes: defaults.max_processes,
        },
    })
}

/// List all registered sandbox backends with their isolation levels.
#[tauri::command]
pub async fn list_sandbox_backends(
    state: State<'_, AppState>,
) -> Result<Vec<SandboxBackendInfo>, String> {
    let dispatcher = state.sandbox_dispatcher.read().await;
    let mut backends = Vec::new();
    for level in &[
        clawdesk_sandbox::IsolationLevel::None,
        clawdesk_sandbox::IsolationLevel::PathScope,
        clawdesk_sandbox::IsolationLevel::ProcessIsolation,
        clawdesk_sandbox::IsolationLevel::FullSandbox,
    ] {
        if let Some(sb) = dispatcher.get(*level) {
            backends.push(SandboxBackendInfo {
                name: sb.name().to_string(),
                isolation_level: format!("{:?}", level),
                available: true,
            });
        }
    }
    Ok(backends)
}

/// Execute a shell command in the sandbox with the specified isolation level.
#[tauri::command]
pub async fn execute_sandboxed(
    command: String,
    isolation_level: Option<String>,
    limits: Option<ResourceLimitsInfo>,
    state: State<'_, AppState>,
) -> Result<SandboxExecResult, String> {
    let dispatcher = state.sandbox_dispatcher.read().await;
    let workspace = state.workspace_root.clone();

    let level = match isolation_level.as_deref() {
        Some("None") => clawdesk_sandbox::IsolationLevel::None,
        Some("PathScope") => clawdesk_sandbox::IsolationLevel::PathScope,
        Some("ProcessIsolation") => clawdesk_sandbox::IsolationLevel::ProcessIsolation,
        Some("FullSandbox") => clawdesk_sandbox::IsolationLevel::FullSandbox,
        _ => dispatcher.max_available(),
    };

    let resource_limits = limits
        .map(|l| clawdesk_sandbox::ResourceLimits {
            cpu_time_secs: l.cpu_time_secs,
            wall_time_secs: l.wall_time_secs,
            memory_bytes: l.memory_bytes,
            max_fds: l.max_fds,
            max_output_bytes: l.max_output_bytes,
            max_processes: l.max_processes,
        })
        .unwrap_or_default();

    let request = clawdesk_sandbox::SandboxRequest {
        execution_id: uuid::Uuid::new_v4().to_string(),
        command: clawdesk_sandbox::SandboxCommand::Shell { command, args: vec![] },
        limits: resource_limits,
        working_dir: Some(workspace.clone()),
        env: std::collections::HashMap::new(),
        network_allowed: false,
        workspace_root: workspace,
    };

    let result = dispatcher
        .execute(level, request)
        .await
        .map_err(|e| format!("{:?}", e))?;

    Ok(SandboxExecResult {
        exit_code: result.exit_code,
        stdout: result.stdout,
        stderr: result.stderr,
        duration_ms: result.duration.as_millis() as u64,
        resource_usage: SandboxResourceUsage {
            cpu_time_ms: result.resource_usage.cpu_time_ms,
            wall_time_ms: result.resource_usage.wall_time_ms,
            peak_memory_bytes: result.resource_usage.peak_memory_bytes,
            output_bytes: result.resource_usage.output_bytes,
        },
    })
}

/// Update the default resource limits for sandbox execution.
#[tauri::command]
pub async fn get_sandbox_resource_limits(
    state: State<'_, AppState>,
) -> Result<ResourceLimitsInfo, String> {
    let _dispatcher = state.sandbox_dispatcher.read().await;
    let defaults = clawdesk_sandbox::ResourceLimits::default();
    Ok(ResourceLimitsInfo {
        cpu_time_secs: defaults.cpu_time_secs,
        wall_time_secs: defaults.wall_time_secs,
        memory_bytes: defaults.memory_bytes,
        max_fds: defaults.max_fds,
        max_output_bytes: defaults.max_output_bytes,
        max_processes: defaults.max_processes,
    })
}

/// Clean up all sandbox resources (temp dirs, containers, etc.).
#[tauri::command]
pub async fn cleanup_sandboxes(
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let dispatcher = state.sandbox_dispatcher.read().await;
    let errors = dispatcher.cleanup_all().await;
    if !errors.is_empty() {
        return Err(format!("Cleanup had {} error(s): {:?}", errors.len(), errors));
    }
    Ok(true)
}
