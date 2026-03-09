//! Unified sandbox dispatcher — O(1) dispatch to the appropriate sandbox runtime.
//!
//! Selects sandbox via `IsolationLevel` enum index with fallback cascade:
//! if the requested level is unavailable, falls back to the next lower level.

use crate::capability_gate::{CapabilityGate, CapabilitySet, GateVerdict};
use crate::{IsolationLevel, Sandbox, SandboxError, SandboxRequest, SandboxResult};
use async_trait::async_trait;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Number of isolation levels in the enum
const NUM_LEVELS: usize = 4;

/// Unified sandbox dispatcher.
///
/// Dispatches to the correct sandbox runtime by indexing into an array
/// using the `IsolationLevel` enum. Supports fallback cascade: if the
/// requested level is unavailable, try the next lower level.
#[derive(Debug)]
pub struct SandboxDispatcher {
    /// Sandboxes indexed by IsolationLevel
    /// [None, PathScope, ProcessIsolation, FullSandbox]
    sandboxes: [Option<Arc<dyn Sandbox>>; NUM_LEVELS],
    /// Optional capability gate for pre-execution permission checks.
    capability_gate: Option<CapabilityGate>,
    /// Default agent capability grant when no per-agent grant is provided.
    default_grant: CapabilitySet,
}

impl SandboxDispatcher {
    /// Create an empty dispatcher.
    pub fn new() -> Self {
        Self {
            sandboxes: Default::default(),
            capability_gate: None,
            default_grant: CapabilitySet::FULL,
        }
    }

    /// Attach a capability gate for pre-execution permission checks.
    pub fn with_capability_gate(mut self, gate: CapabilityGate, default_grant: CapabilitySet) -> Self {
        self.capability_gate = Some(gate);
        self.default_grant = default_grant;
        self
    }

    /// Check capabilities for a tool before execution.
    ///
    /// Returns `Ok(())` when permitted, or a `SandboxError::PermissionDenied` when denied.
    pub fn check_capabilities(
        &self,
        agent_grant: Option<CapabilitySet>,
        tool_name: &str,
    ) -> Result<(), SandboxError> {
        if let Some(ref gate) = self.capability_gate {
            let grant = agent_grant.unwrap_or(self.default_grant);
            match gate.check(grant, tool_name) {
                GateVerdict::Permit => Ok(()),
                GateVerdict::Deny { required, granted, missing } => {
                    warn!(
                        tool = tool_name,
                        ?required,
                        ?granted,
                        ?missing,
                        "capability gate denied tool execution"
                    );
                    Err(SandboxError::ExecutionFailed(format!(
                        "capability denied for '{}': missing {}",
                        tool_name, missing
                    )))
                }
            }
        } else {
            Ok(())
        }
    }

    /// Create a dispatcher with default sandbox configuration.
    ///
    /// Always registers: WorkspaceSandbox (PathScope), SubprocessSandbox (ProcessIso).
    /// Optionally registers: DockerSandbox (if feature enabled).
    pub fn with_defaults() -> Self {
        let mut dispatcher = Self::new();
        // Capability gate wired but with default-full grant (permissive by default).
        // Callers can narrow the grant via `with_capability_gate()`.
        use crate::capability_gate::ToolCapabilityMap;
        let tool_map = ToolCapabilityMap::new(CapabilitySet::EMPTY);
        dispatcher.capability_gate = Some(CapabilityGate::new(tool_map));
        dispatcher.default_grant = CapabilitySet::FULL;

        // Always available
        dispatcher.register(Arc::new(crate::WorkspaceSandbox::new()));
        dispatcher.register(Arc::new(crate::SubprocessSandbox::new()));

        // Docker (if enabled)
        #[cfg(feature = "sandbox-docker")]
        {
            dispatcher.register(Arc::new(crate::DockerSandbox::new()));
        }

        info!(
            levels = ?dispatcher.available_levels(),
            "sandbox dispatcher initialized"
        );

        dispatcher
    }

    /// Register a sandbox implementation.
    pub fn register(&mut self, sandbox: Arc<dyn Sandbox>) {
        let idx = sandbox.isolation_level() as usize;
        debug!(
            name = sandbox.name(),
            level = ?sandbox.isolation_level(),
            "registering sandbox"
        );
        self.sandboxes[idx] = Some(sandbox);
    }

    /// Get the sandbox for a given isolation level, with fallback cascade.
    pub fn get(&self, level: IsolationLevel) -> Option<&Arc<dyn Sandbox>> {
        let idx = level as usize;

        // Try the requested level first
        if let Some(ref sandbox) = self.sandboxes[idx] {
            return Some(sandbox);
        }

        // Fallback cascade: try lower levels
        for i in (0..idx).rev() {
            if let Some(ref sandbox) = self.sandboxes[i] {
                warn!(
                    requested = ?level,
                    fallback = ?sandbox.isolation_level(),
                    "sandbox level unavailable, using fallback"
                );
                return Some(sandbox);
            }
        }

        None
    }

    /// Get the maximum available isolation level.
    pub fn max_available(&self) -> IsolationLevel {
        for i in (0..NUM_LEVELS).rev() {
            if self.sandboxes[i].is_some() {
                return match i {
                    0 => IsolationLevel::None,
                    1 => IsolationLevel::PathScope,
                    2 => IsolationLevel::ProcessIsolation,
                    3 => IsolationLevel::FullSandbox,
                    _ => IsolationLevel::None,
                };
            }
        }
        IsolationLevel::None
    }

    /// List all available isolation levels.
    pub fn available_levels(&self) -> Vec<IsolationLevel> {
        let mut levels = Vec::new();
        for i in 0..NUM_LEVELS {
            if self.sandboxes[i].is_some() {
                levels.push(match i {
                    0 => IsolationLevel::None,
                    1 => IsolationLevel::PathScope,
                    2 => IsolationLevel::ProcessIsolation,
                    3 => IsolationLevel::FullSandbox,
                    _ => continue,
                });
            }
        }
        levels
    }

    /// Execute a request at the specified isolation level (with fallback).
    ///
    /// If a capability gate is attached, checks permission for the tool
    /// before dispatching. Uses `tool_name` from the request for the check.
    pub async fn execute(
        &self,
        level: IsolationLevel,
        request: SandboxRequest,
    ) -> Result<SandboxResult, SandboxError> {
        // Capability gate check
        self.check_capabilities(None, &request.tool_name)?;

        let sandbox = self.get(level).ok_or_else(|| {
            SandboxError::NotAvailable(format!(
                "no sandbox available for {:?} (available: {:?})",
                level,
                self.available_levels()
            ))
        })?;

        debug!(
            sandbox = sandbox.name(),
            level = ?sandbox.isolation_level(),
            execution_id = %request.execution_id,
            "dispatching to sandbox"
        );

        sandbox.execute(request).await
    }

    /// Clean up all registered sandboxes.
    pub async fn cleanup_all(&self) -> Vec<SandboxError> {
        let mut errors = Vec::new();
        for sandbox in self.sandboxes.iter().flatten() {
            if let Err(e) = sandbox.cleanup().await {
                errors.push(e);
            }
        }
        errors
    }
}

impl Default for SandboxDispatcher {
    fn default() -> Self {
        Self::with_defaults()
    }
}

#[async_trait]
impl Sandbox for SandboxDispatcher {
    fn name(&self) -> &str {
        "dispatcher"
    }

    fn isolation_level(&self) -> IsolationLevel {
        self.max_available()
    }

    async fn is_available(&self) -> bool {
        self.sandboxes.iter().any(|s| s.is_some())
    }

    async fn execute(&self, request: SandboxRequest) -> Result<SandboxResult, SandboxError> {
        // Default to ProcessIsolation if no level specified
        self.execute(IsolationLevel::ProcessIsolation, request).await
    }

    async fn cleanup(&self) -> Result<(), SandboxError> {
        let errors = self.cleanup_all().await;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(SandboxError::ExecutionFailed(format!(
                "{} cleanup errors",
                errors.len()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatcher_default_has_workspace_and_subprocess() {
        let dispatcher = SandboxDispatcher::with_defaults();
        let levels = dispatcher.available_levels();
        assert!(levels.contains(&IsolationLevel::PathScope));
        assert!(levels.contains(&IsolationLevel::ProcessIsolation));
    }

    #[test]
    fn fallback_cascade() {
        let dispatcher = SandboxDispatcher::with_defaults();
        // FullSandbox not available without wasm feature, should fallback
        let sandbox = dispatcher.get(IsolationLevel::FullSandbox);
        assert!(sandbox.is_some());
        assert!(sandbox.unwrap().isolation_level() <= IsolationLevel::FullSandbox);
    }

    #[test]
    fn max_available_level() {
        let dispatcher = SandboxDispatcher::with_defaults();
        let max = dispatcher.max_available();
        assert!(max >= IsolationLevel::ProcessIsolation);
    }
}
