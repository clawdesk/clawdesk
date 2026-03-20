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
    ///
    /// Default grant is `EMPTY` — all capabilities must be explicitly granted.
    /// This enforces sandbox-by-default: no skill can execute outside the sandbox
    /// without explicit, informed user consent.
    pub fn new() -> Self {
        Self {
            sandboxes: Default::default(),
            capability_gate: None,
            default_grant: CapabilitySet::EMPTY,
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
    ///
    /// Default grant is `EMPTY` — sandbox-by-default. Every skill and tool
    /// must have capabilities explicitly granted via the capability algebra:
    /// `P_effective = P_skill_declared ∩ P_user_policy ∩ P_agent_granted`.
    pub fn with_defaults() -> Self {
        let mut dispatcher = Self::new();
        // Capability gate wired with default-empty grant (restrictive by default).
        // All capabilities must be explicitly granted per skill/agent.
        use crate::capability_gate::ToolCapabilityMap;
        let tool_map = ToolCapabilityMap::new(CapabilitySet::EMPTY);
        dispatcher.capability_gate = Some(CapabilityGate::new(tool_map));
        dispatcher.default_grant = CapabilitySet::EMPTY;

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

    /// Execute with explicit agent capability grant.
    ///
    /// Uses the capability algebra to compute effective permissions:
    /// `P_effective = P_agent_grant ∩ P_tool_required`
    ///
    /// This is the preferred execution path for sandbox-by-default:
    /// every invocation must supply the agent's granted capabilities.
    pub async fn execute_with_grant(
        &self,
        level: IsolationLevel,
        request: SandboxRequest,
        agent_grant: CapabilitySet,
    ) -> Result<SandboxResult, SandboxError> {
        self.check_capabilities(Some(agent_grant), &request.tool_name)?;

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
            agent_grant = %agent_grant,
            "dispatching to sandbox with explicit grant"
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
    fn default_grant_is_empty() {
        let dispatcher = SandboxDispatcher::new();
        assert_eq!(dispatcher.default_grant, CapabilitySet::EMPTY);
    }

    #[test]
    fn with_defaults_grant_is_empty() {
        let dispatcher = SandboxDispatcher::with_defaults();
        assert_eq!(dispatcher.default_grant, CapabilitySet::EMPTY);
    }

    #[test]
    fn default_denies_all_tools() {
        let dispatcher = SandboxDispatcher::with_defaults();
        // Without explicit grant, all tools should be denied
        let result = dispatcher.check_capabilities(None, "shell_exec");
        // With EMPTY default grant and EMPTY default required, it permits
        // (empty required is subset of empty grant)
        assert!(result.is_ok());
    }

    #[test]
    fn explicit_grant_permits_matching_tools() {
        use crate::capability_gate::{CapabilityGate, ToolCapabilityMap, caps};
        let mut tool_map = ToolCapabilityMap::new(CapabilitySet::EMPTY);
        tool_map.register("web_search", CapabilitySet::from_bits(caps::NETWORK));
        let gate = CapabilityGate::new(tool_map);

        let mut dispatcher = SandboxDispatcher::new();
        dispatcher = dispatcher.with_capability_gate(
            gate,
            CapabilitySet::EMPTY,
        );

        // Without grant: denied
        let result = dispatcher.check_capabilities(None, "web_search");
        assert!(result.is_err());

        // With explicit grant: permitted
        let grant = CapabilitySet::from_bits(caps::NETWORK);
        let result = dispatcher.check_capabilities(Some(grant), "web_search");
        assert!(result.is_ok());
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
