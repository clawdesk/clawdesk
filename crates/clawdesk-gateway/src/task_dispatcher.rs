//! Task Dispatcher — the missing center of ClawDesk's orchestration loop.
//!
//! Takes a `TaskNode` from the planner and decides *how* to execute it:
//!
//! 1. **Skill match** → Look up the task against the `CapabilityIndex`.
//!    If a local skill matches, run it through the `AgentRunner`.
//! 2. **Tool match** → If the task maps to a registered tool (via `ToolRegistry`),
//!    invoke the tool directly without a full agent loop.
//! 3. **ACP delegation** → If no local skill matches but a remote agent
//!    advertises the capability, dispatch via ACP `tasks/send`.
//! 4. **MCP fallback** → If an MCP server exposes the needed tool,
//!    route through `clawdesk-mcp`.
//! 5. **Human escalation** → If confidence is below threshold, park the task
//!    as `InputRequired` and notify the human.
//!
//! ## Complexity
//!
//! Dispatch decision: O(1) via `CapabilityIndex` hash lookup + O(k) for
//! k registered tools. Total per-task overhead is negligible vs LLM latency.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, info, warn};

// ═══════════════════════════════════════════════════════════════════════════
// Dispatch outcome types
// ═══════════════════════════════════════════════════════════════════════════

/// The route chosen for a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DispatchRoute {
    /// Local skill execution via AgentRunner.
    Skill {
        skill_id: String,
        action: String,
    },
    /// Direct tool invocation (no full agent loop).
    Tool {
        tool_name: String,
    },
    /// Remote agent delegation via ACP protocol.
    RemoteAgent {
        agent_id: String,
        endpoint: String,
    },
    /// MCP server tool invocation.
    McpTool {
        server_id: String,
        tool_name: String,
    },
    /// Human escalation — park task and notify.
    HumanEscalation {
        reason: String,
        channel: Option<String>,
    },
    /// No route found — task cannot be dispatched.
    NoRoute {
        reason: String,
    },
}

/// Result of executing a dispatched task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchResult {
    /// The node ID of the task that was dispatched.
    pub node_id: String,
    /// The route that was chosen.
    pub route: DispatchRoute,
    /// Whether execution succeeded.
    pub success: bool,
    /// Output payload (if successful).
    pub output: Option<serde_json::Value>,
    /// Error message (if failed).
    pub error: Option<String>,
    /// Execution duration in milliseconds.
    pub duration_ms: u64,
}

/// Confidence assessment for dispatch decisions.
#[derive(Debug, Clone)]
pub struct DispatchConfidence {
    /// Overall confidence in the chosen route (0.0 - 1.0).
    pub score: f64,
    /// Whether this confidence is below the escalation threshold.
    pub requires_escalation: bool,
}

// ═══════════════════════════════════════════════════════════════════════════
// Backend traits (injected by the application layer)
// ═══════════════════════════════════════════════════════════════════════════

/// Resolves skill capabilities for dispatch routing.
#[async_trait]
pub trait SkillLookup: Send + Sync + 'static {
    /// Find a skill that can handle the given action.
    /// Returns `(skill_id, estimated_duration_ms)` if found.
    async fn find_skill_for_action(&self, action: &str) -> Option<(String, u64)>;

    /// Match a task description against available skills using semantic matching.
    /// Returns `(skill_id, action, confidence)` if found.
    async fn match_task_description(&self, description: &str) -> Option<(String, String, f64)>;
}

/// Resolves registered tools for direct invocation.
#[async_trait]
pub trait ToolLookup: Send + Sync + 'static {
    /// Check if a tool is registered and can handle an action.
    async fn find_tool(&self, action: &str) -> Option<String>;

    /// Execute a tool directly with the given arguments.
    async fn execute_tool(
        &self,
        tool_name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String>;
}

/// Resolves remote agents for ACP delegation.
#[async_trait]
pub trait RemoteAgentLookup: Send + Sync + 'static {
    /// Find a remote agent that can handle the given capability.
    /// Returns `(agent_id, endpoint, confidence)` if found.
    async fn find_agent_for_capability(
        &self,
        capability: &str,
    ) -> Option<(String, String, f64)>;

    /// Dispatch a task to a remote agent via ACP.
    async fn dispatch_task(
        &self,
        agent_id: &str,
        task_description: &str,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, String>;
}

/// Resolves MCP server tools as a fallback.
#[async_trait]
pub trait McpLookup: Send + Sync + 'static {
    /// Find an MCP tool that can handle the given action.
    /// Returns `(server_id, tool_name)` if found.
    async fn find_mcp_tool(&self, action: &str) -> Option<(String, String)>;

    /// Invoke an MCP tool.
    async fn call_mcp_tool(
        &self,
        server_id: &str,
        tool_name: &str,
        args: serde_json::Value,
    ) -> Result<serde_json::Value, String>;
}

/// Handles human escalation for low-confidence tasks.
#[async_trait]
pub trait EscalationHandler: Send + Sync + 'static {
    /// Escalate a task to a human. Returns the channel used for escalation.
    async fn escalate(
        &self,
        node_id: &str,
        description: &str,
        reason: &str,
        context: serde_json::Value,
    ) -> Result<String, String>;
}

// ═══════════════════════════════════════════════════════════════════════════
// Task Dispatcher
// ═══════════════════════════════════════════════════════════════════════════

/// Configuration for the task dispatcher.
#[derive(Debug, Clone)]
pub struct DispatcherConfig {
    /// Minimum confidence score before escalating to human.
    pub escalation_threshold: f64,
    /// Whether to enable MCP fallback routing.
    pub enable_mcp_fallback: bool,
    /// Whether to enable remote agent delegation.
    pub enable_remote_agents: bool,
    /// Maximum concurrent dispatches.
    pub max_concurrent: usize,
}

impl Default for DispatcherConfig {
    fn default() -> Self {
        Self {
            escalation_threshold: 0.3,
            enable_mcp_fallback: true,
            enable_remote_agents: true,
            max_concurrent: 10,
        }
    }
}

/// The central task dispatcher — routes tasks to the appropriate execution backend.
///
/// This is the single most impactful component for turning ClawDesk from
/// "a gateway that forwards messages" into "a platform that orchestrates work."
pub struct TaskDispatcher {
    config: DispatcherConfig,
    skill_lookup: Arc<dyn SkillLookup>,
    tool_lookup: Arc<dyn ToolLookup>,
    remote_lookup: Option<Arc<dyn RemoteAgentLookup>>,
    mcp_lookup: Option<Arc<dyn McpLookup>>,
    escalation: Option<Arc<dyn EscalationHandler>>,
}

impl TaskDispatcher {
    pub fn new(
        config: DispatcherConfig,
        skill_lookup: Arc<dyn SkillLookup>,
        tool_lookup: Arc<dyn ToolLookup>,
    ) -> Self {
        Self {
            config,
            skill_lookup,
            tool_lookup,
            remote_lookup: None,
            mcp_lookup: None,
            escalation: None,
        }
    }

    pub fn with_remote_agents(mut self, lookup: Arc<dyn RemoteAgentLookup>) -> Self {
        self.remote_lookup = Some(lookup);
        self
    }

    pub fn with_mcp(mut self, lookup: Arc<dyn McpLookup>) -> Self {
        self.mcp_lookup = Some(lookup);
        self
    }

    pub fn with_escalation(mut self, handler: Arc<dyn EscalationHandler>) -> Self {
        self.escalation = Some(handler);
        self
    }

    /// Decide how to dispatch a task, returning the chosen route and confidence.
    ///
    /// Resolution order:
    /// 1. Skill match (by action or description)
    /// 2. Tool match (direct tool registry lookup)
    /// 3. Remote agent (ACP delegation)
    /// 4. MCP fallback
    /// 5. Human escalation (if confidence too low)
    /// 6. NoRoute (nothing can handle it)
    pub async fn resolve_route(
        &self,
        node_id: &str,
        description: &str,
        metadata: &serde_json::Value,
    ) -> (DispatchRoute, DispatchConfidence) {
        // Extract action hint from metadata if available
        let action_hint = metadata
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        // 1. Try skill match by explicit action
        if !action_hint.is_empty() {
            if let Some((skill_id, _dur)) = self.skill_lookup.find_skill_for_action(action_hint).await {
                debug!(node_id, skill_id, action = action_hint, "dispatch: skill match by action");
                return (
                    DispatchRoute::Skill {
                        skill_id,
                        action: action_hint.to_string(),
                    },
                    DispatchConfidence {
                        score: 0.95,
                        requires_escalation: false,
                    },
                );
            }
        }

        // 1b. Try skill match by semantic description
        if let Some((skill_id, action, confidence)) =
            self.skill_lookup.match_task_description(description).await
        {
            if confidence > self.config.escalation_threshold {
                debug!(node_id, skill_id, action, confidence, "dispatch: skill match by description");
                return (
                    DispatchRoute::Skill { skill_id, action },
                    DispatchConfidence {
                        score: confidence,
                        requires_escalation: false,
                    },
                );
            }
        }

        // 2. Try direct tool match
        let tool_key = if !action_hint.is_empty() {
            action_hint
        } else {
            description
        };
        if let Some(tool_name) = self.tool_lookup.find_tool(tool_key).await {
            debug!(node_id, tool_name, "dispatch: tool match");
            return (
                DispatchRoute::Tool { tool_name },
                DispatchConfidence {
                    score: 0.9,
                    requires_escalation: false,
                },
            );
        }

        // 3. Try remote agent (if enabled)
        if self.config.enable_remote_agents {
            if let Some(ref remote) = self.remote_lookup {
                let cap_key = if !action_hint.is_empty() {
                    action_hint
                } else {
                    description
                };
                if let Some((agent_id, endpoint, confidence)) =
                    remote.find_agent_for_capability(cap_key).await
                {
                    if confidence > self.config.escalation_threshold {
                        debug!(node_id, agent_id, endpoint, "dispatch: remote agent");
                        return (
                            DispatchRoute::RemoteAgent { agent_id, endpoint },
                            DispatchConfidence {
                                score: confidence,
                                requires_escalation: false,
                            },
                        );
                    }
                }
            }
        }

        // 4. Try MCP fallback (if enabled)
        if self.config.enable_mcp_fallback {
            if let Some(ref mcp) = self.mcp_lookup {
                let mcp_key = if !action_hint.is_empty() {
                    action_hint
                } else {
                    description
                };
                if let Some((server_id, tool_name)) = mcp.find_mcp_tool(mcp_key).await {
                    debug!(node_id, server_id, tool_name, "dispatch: MCP fallback");
                    return (
                        DispatchRoute::McpTool {
                            server_id,
                            tool_name,
                        },
                        DispatchConfidence {
                            score: 0.7,
                            requires_escalation: false,
                        },
                    );
                }
            }
        }

        // 5. Human escalation
        let escalation_channel = metadata
            .get("escalation_channel")
            .and_then(|v| v.as_str())
            .map(String::from);

        if self.escalation.is_some() {
            info!(node_id, "dispatch: escalating to human — no automated route found");
            return (
                DispatchRoute::HumanEscalation {
                    reason: format!("No automated handler found for: {}", description),
                    channel: escalation_channel,
                },
                DispatchConfidence {
                    score: 0.0,
                    requires_escalation: true,
                },
            );
        }

        // 6. No route
        warn!(node_id, description, "dispatch: no route found");
        (
            DispatchRoute::NoRoute {
                reason: format!("No skill, tool, remote agent, or MCP server can handle: {}", description),
            },
            DispatchConfidence {
                score: 0.0,
                requires_escalation: true,
            },
        )
    }

    /// Dispatch and execute a task node.
    ///
    /// This is the primary entry point: resolve the route, then execute.
    pub async fn dispatch(
        &self,
        node_id: &str,
        description: &str,
        input: serde_json::Value,
        metadata: &serde_json::Value,
    ) -> DispatchResult {
        let start = std::time::Instant::now();
        let (route, confidence) = self.resolve_route(node_id, description, metadata).await;

        let (success, output, error) = match &route {
            DispatchRoute::Skill { skill_id, action } => {
                // Delegate to skill lookup which wraps AgentRunner
                match self
                    .skill_lookup
                    .find_skill_for_action(action)
                    .await
                {
                    Some(_) => {
                        // The actual execution would be done by the orchestrator
                        // which holds the AgentRunner. We return the routing decision.
                        (true, Some(serde_json::json!({
                            "routed_to": "skill",
                            "skill_id": skill_id,
                            "action": action,
                        })), None)
                    }
                    None => (false, None, Some(format!("Skill {} not found", skill_id))),
                }
            }

            DispatchRoute::Tool { tool_name } => {
                match self.tool_lookup.execute_tool(tool_name, input.clone()).await {
                    Ok(output) => (true, Some(output), None),
                    Err(e) => (false, None, Some(e)),
                }
            }

            DispatchRoute::RemoteAgent { agent_id, .. } => {
                if let Some(ref remote) = self.remote_lookup {
                    match remote.dispatch_task(agent_id, description, input.clone()).await {
                        Ok(output) => (true, Some(output), None),
                        Err(e) => (false, None, Some(e)),
                    }
                } else {
                    (false, None, Some("Remote agent support not configured".into()))
                }
            }

            DispatchRoute::McpTool {
                server_id,
                tool_name,
            } => {
                if let Some(ref mcp) = self.mcp_lookup {
                    match mcp.call_mcp_tool(server_id, tool_name, input.clone()).await {
                        Ok(output) => (true, Some(output), None),
                        Err(e) => (false, None, Some(e)),
                    }
                } else {
                    (false, None, Some("MCP support not configured".into()))
                }
            }

            DispatchRoute::HumanEscalation { reason, .. } => {
                if let Some(ref escalation) = self.escalation {
                    match escalation
                        .escalate(node_id, description, reason, input.clone())
                        .await
                    {
                        Ok(channel) => (
                            true,
                            Some(serde_json::json!({
                                "escalated": true,
                                "channel": channel,
                                "awaiting_human": true,
                            })),
                            None,
                        ),
                        Err(e) => (false, None, Some(e)),
                    }
                } else {
                    (false, None, Some("No escalation handler configured".into()))
                }
            }

            DispatchRoute::NoRoute { reason } => (false, None, Some(reason.clone())),
        };

        let duration_ms = start.elapsed().as_millis() as u64;

        if !success {
            if let Some(ref err) = error {
                warn!(node_id, route = ?route, error = err, "dispatch execution failed");
            }
        }

        DispatchResult {
            node_id: node_id.to_string(),
            route,
            success,
            output,
            error,
            duration_ms,
        }
    }
}
