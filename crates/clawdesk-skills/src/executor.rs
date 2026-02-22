//! Skill execution engine — dispatches skill actions to the appropriate handler.
//!
//! The executor bridges the gap between skill selection (which skills to activate)
//! and runtime execution (actually invoking tools, augmenting prompts, etc.).
//!
//! ## Design
//!
//! Each skill has a set of `SkillToolBinding`s. When a tool call arrives from the
//! LLM, the executor matches it against active skill bindings and dispatches to
//! the appropriate handler function.
//!
//! ```text
//! LLM tool_call("web_search", {query: "..."})
//!   → SkillExecutor::dispatch()
//!     → match skill_id → handler
//!       → execute handler with params
//!       → return result to LLM
//! ```

use crate::definition::{Skill, SkillId, SkillToolBinding};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Result of a skill execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillExecutionResult {
    /// The skill that was executed.
    pub skill_id: String,
    /// The tool that was called.
    pub tool_name: String,
    /// Whether execution succeeded.
    pub success: bool,
    /// Result text (for returning to the LLM).
    pub output: String,
    /// Execution duration in milliseconds.
    pub duration_ms: u64,
    /// Any structured data returned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// Error from skill execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillExecutionError {
    pub skill_id: String,
    pub tool_name: String,
    pub message: String,
    pub recoverable: bool,
}

impl std::fmt::Display for SkillExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "skill {} / tool {}: {}", self.skill_id, self.tool_name, self.message)
    }
}

impl std::error::Error for SkillExecutionError {}

/// A handler function for a skill tool invocation.
pub type SkillHandler = Arc<
    dyn Fn(&str, serde_json::Value) -> Result<SkillExecutionResult, SkillExecutionError>
        + Send
        + Sync,
>;

/// Skill execution engine.
///
/// Maps tool names to their owning skill and handler, enabling dispatch
/// of LLM tool calls to the correct skill implementation.
pub struct SkillExecutor {
    /// tool_name → (skill_id, handler)
    handlers: HashMap<String, (SkillId, SkillHandler)>,
    /// Execution statistics per skill.
    stats: HashMap<String, ExecutionStats>,
}

/// Per-skill execution statistics.
#[derive(Debug, Clone, Default)]
pub struct ExecutionStats {
    pub total_calls: u64,
    pub successes: u64,
    pub failures: u64,
    pub total_ms: u64,
}

impl ExecutionStats {
    pub fn avg_ms(&self) -> f64 {
        if self.total_calls == 0 {
            0.0
        } else {
            self.total_ms as f64 / self.total_calls as f64
        }
    }

    pub fn success_rate(&self) -> f64 {
        if self.total_calls == 0 {
            0.0
        } else {
            self.successes as f64 / self.total_calls as f64
        }
    }
}

impl SkillExecutor {
    /// Create a new executor.
    pub fn new() -> Self {
        Self {
            handlers: HashMap::new(),
            stats: HashMap::new(),
        }
    }

    /// Register a skill's tools with their handler.
    ///
    /// Registers both `provided_tools` (skill-owned tool definitions) and
    /// `required_tools` (agent-level tools the skill needs). For required_tools,
    /// the handler is registered as a fallback — if the agent's tool registry
    /// already has a handler for that tool, this won't overwrite it.
    pub fn register_skill(&mut self, skill: &Skill, handler: SkillHandler) {
        let skill_id = skill.manifest.id.clone();
        // Register provided tools (skill-owned)
        for tool in &skill.provided_tools {
            self.handlers.insert(
                tool.tool_name.clone(),
                (skill_id.clone(), handler.clone()),
            );
        }
        // Register required tools as fallback (don't overwrite existing handlers)
        for tool_name in &skill.manifest.required_tools {
            self.handlers
                .entry(tool_name.clone())
                .or_insert_with(|| (skill_id.clone(), handler.clone()));
        }
    }

    /// Register a handler for a specific tool name.
    pub fn register_tool(
        &mut self,
        tool_name: &str,
        skill_id: SkillId,
        handler: SkillHandler,
    ) {
        self.handlers
            .insert(tool_name.to_string(), (skill_id, handler));
    }

    /// Dispatch a tool call to the appropriate skill handler.
    pub fn dispatch(
        &mut self,
        tool_name: &str,
        arguments: serde_json::Value,
    ) -> Result<SkillExecutionResult, SkillExecutionError> {
        let (skill_id, handler) = self.handlers.get(tool_name).ok_or_else(|| {
            SkillExecutionError {
                skill_id: "unknown".to_string(),
                tool_name: tool_name.to_string(),
                message: format!("no handler registered for tool '{}'", tool_name),
                recoverable: false,
            }
        })?;

        let skill_id_str = skill_id.as_str().to_string();
        let start = std::time::Instant::now();

        let result = handler(tool_name, arguments);
        let duration_ms = start.elapsed().as_millis() as u64;

        // Update stats
        let stats = self
            .stats
            .entry(skill_id_str.clone())
            .or_insert_with(ExecutionStats::default);
        stats.total_calls += 1;
        stats.total_ms += duration_ms;

        match &result {
            Ok(_) => stats.successes += 1,
            Err(_) => stats.failures += 1,
        }

        result
    }

    /// Check if a tool has a registered handler.
    pub fn has_handler(&self, tool_name: &str) -> bool {
        self.handlers.contains_key(tool_name)
    }

    /// Get execution statistics for a skill.
    pub fn get_stats(&self, skill_id: &str) -> Option<&ExecutionStats> {
        self.stats.get(skill_id)
    }

    /// Get all registered tool names.
    pub fn registered_tools(&self) -> Vec<&str> {
        self.handlers.keys().map(|s| s.as_str()).collect()
    }

    /// Build a composite system prompt from active skills.
    ///
    /// Concatenates prompt fragments from the given skills, respecting
    /// the token budget. Returns the combined prompt and the list of
    /// tool definitions for the LLM.
    pub fn build_prompt(
        skills: &[&Skill],
        token_budget: usize,
    ) -> (String, Vec<SkillToolBinding>) {
        let mut prompt = String::new();
        let mut tools = Vec::new();
        let mut used_tokens = 0;

        for skill in skills {
            let cost = skill.token_cost();
            if used_tokens + cost > token_budget {
                break;
            }
            prompt.push_str(&skill.prompt_fragment);
            prompt.push_str("\n\n");
            tools.extend(skill.provided_tools.iter().cloned());
            used_tokens += cost;
        }

        (prompt, tools)
    }
}

impl Default for SkillExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::*;

    fn test_skill() -> Skill {
        Skill {
            manifest: SkillManifest {
                id: SkillId::from("test/echo"),
                display_name: "Echo".into(),
                description: "Echoes input".into(),
                version: "0.1.0".into(),
                author: None,
                dependencies: vec![],
                required_tools: vec![],
                parameters: vec![],
                triggers: vec![SkillTrigger::Always],
                estimated_tokens: 50,
                priority_weight: 1.0,
                tags: vec![],
                signature: None,
                publisher_key: None,
                content_hash: None,
                schema_version: 1,
            },
            prompt_fragment: "You can echo back user input.".into(),
            provided_tools: vec![SkillToolBinding {
                tool_name: "echo".into(),
                description: "Echo text back".into(),
                parameters_schema: serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
            }],
            parameter_values: serde_json::Value::Null,
            source_path: None,
        }
    }

    #[test]
    fn register_and_dispatch() {
        let mut executor = SkillExecutor::new();
        let skill = test_skill();

        let handler: SkillHandler = Arc::new(|tool_name, args| {
            let text = args["text"].as_str().unwrap_or("").to_string();
            Ok(SkillExecutionResult {
                skill_id: "test/echo".into(),
                tool_name: tool_name.to_string(),
                success: true,
                output: format!("Echo: {}", text),
                duration_ms: 1,
                data: None,
            })
        });

        executor.register_skill(&skill, handler);
        assert!(executor.has_handler("echo"));

        let result = executor
            .dispatch("echo", serde_json::json!({"text": "hello"}))
            .unwrap();
        assert!(result.success);
        assert_eq!(result.output, "Echo: hello");
    }

    #[test]
    fn dispatch_unknown_tool_fails() {
        let mut executor = SkillExecutor::new();
        let result = executor.dispatch("nonexistent", serde_json::json!({}));
        assert!(result.is_err());
        assert!(!result.unwrap_err().recoverable);
    }

    #[test]
    fn execution_stats_tracked() {
        let mut executor = SkillExecutor::new();
        let skill = test_skill();

        let handler: SkillHandler = Arc::new(|tool_name, _| {
            Ok(SkillExecutionResult {
                skill_id: "test/echo".into(),
                tool_name: tool_name.to_string(),
                success: true,
                output: "ok".into(),
                duration_ms: 5,
                data: None,
            })
        });

        executor.register_skill(&skill, handler);
        executor.dispatch("echo", serde_json::json!({})).unwrap();
        executor.dispatch("echo", serde_json::json!({})).unwrap();

        let stats = executor.get_stats("test/echo").unwrap();
        assert_eq!(stats.total_calls, 2);
        assert_eq!(stats.successes, 2);
        assert!(stats.success_rate() > 0.99);
    }

    #[test]
    fn build_prompt_respects_budget() {
        let skill1 = test_skill();
        let skills = vec![&skill1];
        let (prompt, tools) = SkillExecutor::build_prompt(&skills, 10000);
        assert!(prompt.contains("echo"));
        assert_eq!(tools.len(), 1);

        // Zero budget should produce empty
        let (prompt, tools) = SkillExecutor::build_prompt(&skills, 0);
        assert!(prompt.is_empty());
        assert!(tools.is_empty());
    }
}
