//! Declarative pipeline definition in TOML with CLI execution and monitoring.
//!
//! Bridges ClawDesk's existing `AgentPipeline` DAG with a user-facing TOML
//! definition format and CLI surface:
//!
//! ```text
//! clawdesk pipeline run <name> --input "..."
//! clawdesk pipeline status
//! clawdesk pipeline logs <name>
//! ```
//!
//! Also exposes pipelines via chat: "run the design-and-build pipeline for a settings page".

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

// ---------------------------------------------------------------------------
// TOML pipeline schema
// ---------------------------------------------------------------------------

/// Top-level pipeline definition (parsed from TOML).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineDefinition {
    pub pipeline: PipelineSection,
}

/// Main [pipeline] section.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineSection {
    /// Pipeline display name.
    pub name: String,
    /// Description of what this pipeline does.
    #[serde(default)]
    pub description: String,
    /// Ordered steps.
    pub steps: Vec<PipelineStepDef>,
    /// Global timeout in seconds (default: none).
    pub timeout_secs: Option<u64>,
}

/// A step in a pipeline definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PipelineStepDef {
    /// Single agent invocation.
    Agent {
        agent_id: String,
        task: String,
        #[serde(default)]
        timeout_secs: Option<u64>,
    },
    /// Parallel fan-out with merge.
    Parallel {
        branches: Vec<PipelineStepDef>,
        #[serde(default = "default_merge_strategy")]
        merge_strategy: String,
    },
    /// Gate (human-in-the-loop approval).
    Gate {
        prompt: String,
        #[serde(default)]
        timeout_secs: Option<u64>,
        #[serde(default = "default_gate_action")]
        default_action: String,
    },
    /// Transform step (applies a template to previous output).
    Transform {
        template: String,
    },
    /// Router (conditional branching).
    Router {
        condition: String,
        branches: HashMap<String, PipelineStepDef>,
    },
}

fn default_merge_strategy() -> String { "concatenate".to_string() }
fn default_gate_action() -> String { "abort".to_string() }

// ---------------------------------------------------------------------------
// Pipeline resolution: TOML → runtime DAG
// ---------------------------------------------------------------------------

/// Validate a pipeline definition.
pub fn validate_pipeline(def: &PipelineDefinition) -> Vec<PipelineValidationError> {
    let mut errors = Vec::new();

    if def.pipeline.name.is_empty() {
        errors.push(PipelineValidationError {
            message: "Pipeline name is empty".into(),
            step_index: None,
        });
    }

    if def.pipeline.steps.is_empty() {
        errors.push(PipelineValidationError {
            message: "Pipeline has no steps".into(),
            step_index: None,
        });
    }

    for (i, step) in def.pipeline.steps.iter().enumerate() {
        validate_step(step, i, &mut errors);
    }

    errors
}

fn validate_step(step: &PipelineStepDef, index: usize, errors: &mut Vec<PipelineValidationError>) {
    match step {
        PipelineStepDef::Agent { agent_id, task, .. } => {
            if agent_id.is_empty() {
                errors.push(PipelineValidationError {
                    message: format!("Step #{index}: agent_id is empty"),
                    step_index: Some(index),
                });
            }
            if task.is_empty() {
                errors.push(PipelineValidationError {
                    message: format!("Step #{index}: task is empty"),
                    step_index: Some(index),
                });
            }
        }
        PipelineStepDef::Parallel { branches, .. } => {
            if branches.is_empty() {
                errors.push(PipelineValidationError {
                    message: format!("Step #{index}: parallel step has no branches"),
                    step_index: Some(index),
                });
            }
            for (j, branch) in branches.iter().enumerate() {
                validate_step(branch, index * 100 + j, errors);
            }
        }
        PipelineStepDef::Gate { prompt, .. } => {
            if prompt.is_empty() {
                errors.push(PipelineValidationError {
                    message: format!("Step #{index}: gate prompt is empty"),
                    step_index: Some(index),
                });
            }
        }
        PipelineStepDef::Transform { template } => {
            if template.is_empty() {
                errors.push(PipelineValidationError {
                    message: format!("Step #{index}: transform template is empty"),
                    step_index: Some(index),
                });
            }
        }
        PipelineStepDef::Router { condition, branches } => {
            if condition.is_empty() {
                errors.push(PipelineValidationError {
                    message: format!("Step #{index}: router condition is empty"),
                    step_index: Some(index),
                });
            }
            if branches.is_empty() {
                errors.push(PipelineValidationError {
                    message: format!("Step #{index}: router has no branches"),
                    step_index: Some(index),
                });
            }
        }
    }
}

/// A pipeline validation error.
#[derive(Debug, Clone)]
pub struct PipelineValidationError {
    pub message: String,
    pub step_index: Option<usize>,
}

// ---------------------------------------------------------------------------
// Pipeline variable interpolation
// ---------------------------------------------------------------------------

/// Interpolate `{{input}}` and `{{step.N.output}}` placeholders in a task string.
///
/// `step_outputs` maps step index → output string.
pub fn interpolate_task(task: &str, input: &str, step_outputs: &HashMap<usize, String>) -> String {
    let mut result = task.replace("{{input}}", input);

    // Replace {{step.N.output}} patterns
    for (idx, output) in step_outputs {
        let placeholder = format!("{{{{step.{idx}.output}}}}");
        result = result.replace(&placeholder, output);
    }

    result
}

// ---------------------------------------------------------------------------
// Pipeline execution state
// ---------------------------------------------------------------------------

/// Runtime state of a pipeline execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineExecution {
    /// Unique execution ID.
    pub id: String,
    /// Pipeline name.
    pub pipeline_name: String,
    /// Input that started the pipeline.
    pub input: String,
    /// Current state.
    pub state: PipelineState,
    /// Per-step results.
    pub step_results: Vec<StepExecutionResult>,
    /// Start time (ISO 8601).
    pub started_at: String,
    /// End time, if completed.
    pub completed_at: Option<String>,
}

/// Pipeline execution state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineState {
    Running,
    WaitingForGate,
    Completed,
    Failed,
    Cancelled,
}

/// Result of a single step execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepExecutionResult {
    pub step_index: usize,
    pub step_type: String,
    pub output: Option<String>,
    pub error: Option<String>,
    pub duration_ms: u64,
    pub state: StepState,
}

/// Step state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
}

// ---------------------------------------------------------------------------
// Pipeline loading
// ---------------------------------------------------------------------------

/// Parse a pipeline TOML file.
pub fn parse_pipeline_toml(content: &str) -> Result<PipelineDefinition, String> {
    toml::from_str(content).map_err(|e| format!("Invalid pipeline TOML: {e}"))
}

/// Load all pipeline definitions from a directory.
pub fn load_all_pipelines(pipelines_dir: &Path) -> Result<Vec<(String, PipelineDefinition)>, String> {
    let mut pipelines = Vec::new();

    if !pipelines_dir.exists() {
        return Ok(pipelines);
    }

    let entries = std::fs::read_dir(pipelines_dir)
        .map_err(|e| format!("Failed to read pipelines directory: {e}"))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }

        let name = path
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;

        let def = parse_pipeline_toml(&content)?;
        pipelines.push((name, def));
    }

    Ok(pipelines)
}

/// Collect all referenced agent IDs from a pipeline definition.
pub fn referenced_agents(def: &PipelineDefinition) -> Vec<String> {
    let mut agents = Vec::new();
    for step in &def.pipeline.steps {
        collect_agents_from_step(step, &mut agents);
    }
    agents.sort();
    agents.dedup();
    agents
}

fn collect_agents_from_step(step: &PipelineStepDef, agents: &mut Vec<String>) {
    match step {
        PipelineStepDef::Agent { agent_id, .. } => {
            agents.push(agent_id.clone());
        }
        PipelineStepDef::Parallel { branches, .. } => {
            for branch in branches {
                collect_agents_from_step(branch, agents);
            }
        }
        PipelineStepDef::Router { branches, .. } => {
            for branch in branches.values() {
                collect_agents_from_step(branch, agents);
            }
        }
        PipelineStepDef::Gate { .. } | PipelineStepDef::Transform { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_pipeline_toml() -> &'static str {
        r#"
[pipeline]
name = "Design & Build"
description = "Full design-to-implementation pipeline"

[[pipeline.steps]]
type = "agent"
agent_id = "researcher"
task = "Find 3 competitor references for: {{input}}"
timeout_secs = 120

[[pipeline.steps]]
type = "parallel"
merge_strategy = "concatenate"

  [[pipeline.steps.branches]]
  type = "agent"
  agent_id = "designer"
  task = "Create UX spec based on: {{step.0.output}}"

  [[pipeline.steps.branches]]
  type = "agent"
  agent_id = "builder"
  task = "Identify technical constraints for: {{step.0.output}}"

[[pipeline.steps]]
type = "gate"
prompt = "Review design spec before implementation?"
timeout_secs = 300
default_action = "abort"

[[pipeline.steps]]
type = "agent"
agent_id = "builder"
task = "Convert approved design to implementation plan: {{step.1.output}}"
"#
    }

    #[test]
    fn test_parse_pipeline_toml() {
        let def = parse_pipeline_toml(sample_pipeline_toml()).unwrap();
        assert_eq!(def.pipeline.name, "Design & Build");
        assert_eq!(def.pipeline.steps.len(), 4);
    }

    #[test]
    fn test_validate_pipeline_ok() {
        let def = parse_pipeline_toml(sample_pipeline_toml()).unwrap();
        let errors = validate_pipeline(&def);
        assert!(errors.is_empty(), "Expected no errors, got: {:?}", errors);
    }

    #[test]
    fn test_validate_empty_pipeline() {
        let toml = r#"
[pipeline]
name = ""
steps = []
"#;
        let def = parse_pipeline_toml(toml).unwrap();
        let errors = validate_pipeline(&def);
        assert!(errors.len() >= 2); // empty name + empty steps
    }

    #[test]
    fn test_referenced_agents() {
        let def = parse_pipeline_toml(sample_pipeline_toml()).unwrap();
        let agents = referenced_agents(&def);
        assert!(agents.contains(&"researcher".to_string()));
        assert!(agents.contains(&"designer".to_string()));
        assert!(agents.contains(&"builder".to_string()));
    }

    #[test]
    fn test_interpolate_task() {
        let mut outputs = HashMap::new();
        outputs.insert(0, "research results here".to_string());

        let result = interpolate_task(
            "Build UX spec for {{input}} based on {{step.0.output}}",
            "settings page",
            &outputs,
        );
        assert_eq!(result, "Build UX spec for settings page based on research results here");
    }

    #[test]
    fn test_pipeline_state_serialization() {
        let state = PipelineState::WaitingForGate;
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, "\"waiting_for_gate\"");
    }

    #[test]
    fn test_validate_empty_agent_id() {
        let toml = r#"
[pipeline]
name = "Test"
[[pipeline.steps]]
type = "agent"
agent_id = ""
task = "Do something"
"#;
        let def = parse_pipeline_toml(toml).unwrap();
        let errors = validate_pipeline(&def);
        assert!(errors.iter().any(|e| e.message.contains("agent_id is empty")));
    }
}
