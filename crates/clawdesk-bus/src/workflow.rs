//! # Workflow Materialization — Declared composable pipelines on the event bus.
//!
//! Replaces implicit "bag of capabilities" behavior with declared, composable
//! workflows: `message.incoming → retrieve_context → browse_if_needed →
//! summarize → artifact_emit → deliver`.
//!
//! Workflows are bounded DAGs executed atop the bus's WFQ queues. Each stage
//! is an explicit node that emits events to the bus, enabling testing,
//! benchmarking, and reasoning about behavior.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A declared workflow definition that can be materialized on the event bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowDefinition {
    /// Unique workflow identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Description of what this workflow does.
    pub description: String,
    /// Ordered stages in the workflow.
    pub stages: Vec<WorkflowStage>,
    /// Trigger topic pattern (e.g., "channel.inbound.*").
    pub trigger_topic: String,
    /// Whether the workflow is enabled.
    pub enabled: bool,
}

/// A single stage in a workflow pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowStage {
    /// Stage identifier.
    pub id: String,
    /// Stage type.
    pub stage_type: StageType,
    /// Configuration for this stage.
    pub config: HashMap<String, serde_json::Value>,
    /// IDs of stages that must complete before this one.
    pub depends_on: Vec<String>,
    /// Whether this stage is optional (workflow continues if it fails).
    pub optional: bool,
    /// Timeout in seconds.
    pub timeout_secs: u64,
}

/// Types of workflow stages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StageType {
    /// Retrieve context via memory/vector search.
    RetrieveContext,
    /// Run browser automation if needed.
    BrowseIfNeeded,
    /// Call the LLM for summarization/response.
    LlmCall,
    /// Emit an artifact.
    ArtifactEmit,
    /// Deliver results via announce/channel/webhook.
    Deliver,
    /// Run a custom pipeline step.
    Custom { handler: String },
    /// Gate — wait for human approval.
    Gate { prompt: String },
    /// Fan-out to parallel branches.
    Parallel { branch_count: usize },
}

/// The information-assistant workflow — the canonical pipeline.
pub fn information_workflow() -> WorkflowDefinition {
    WorkflowDefinition {
        id: "info-assistant".into(),
        name: "Information Assistant Workflow".into(),
        description: "message → retrieve → browse_if_needed → summarize → artifact → deliver".into(),
        stages: vec![
            WorkflowStage {
                id: "retrieve".into(),
                stage_type: StageType::RetrieveContext,
                config: HashMap::new(),
                depends_on: Vec::new(),
                optional: false,
                timeout_secs: 10,
            },
            WorkflowStage {
                id: "browse".into(),
                stage_type: StageType::BrowseIfNeeded,
                config: HashMap::new(),
                depends_on: vec!["retrieve".into()],
                optional: true,
                timeout_secs: 30,
            },
            WorkflowStage {
                id: "summarize".into(),
                stage_type: StageType::LlmCall,
                config: HashMap::new(),
                depends_on: vec!["retrieve".into(), "browse".into()],
                optional: false,
                timeout_secs: 60,
            },
            WorkflowStage {
                id: "artifact".into(),
                stage_type: StageType::ArtifactEmit,
                config: HashMap::new(),
                depends_on: vec!["summarize".into()],
                optional: true,
                timeout_secs: 5,
            },
            WorkflowStage {
                id: "deliver".into(),
                stage_type: StageType::Deliver,
                config: HashMap::new(),
                depends_on: vec!["summarize".into(), "artifact".into()],
                optional: false,
                timeout_secs: 10,
            },
        ],
        trigger_topic: "channel.inbound.*".into(),
        enabled: true,
    }
}

/// Validate a workflow definition (check for cycles, missing dependencies).
pub fn validate_workflow(workflow: &WorkflowDefinition) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    let stage_ids: std::collections::HashSet<&str> =
        workflow.stages.iter().map(|s| s.id.as_str()).collect();

    for stage in &workflow.stages {
        for dep in &stage.depends_on {
            if !stage_ids.contains(dep.as_str()) {
                errors.push(format!(
                    "Stage '{}' depends on '{}' which does not exist",
                    stage.id, dep
                ));
            }
            if dep == &stage.id {
                errors.push(format!("Stage '{}' depends on itself", stage.id));
            }
        }
    }

    // Topological sort cycle detection
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    for stage in &workflow.stages {
        in_degree.entry(&stage.id).or_insert(0);
        for dep in &stage.depends_on {
            *in_degree.entry(dep.as_str()).or_insert(0) += 0; // ensure dep exists
        }
    }
    for stage in &workflow.stages {
        for _dep in &stage.depends_on {
            *in_degree.entry(&stage.id).or_insert(0) += 1;
        }
    }

    let mut queue: Vec<&str> = in_degree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&k, _)| k)
        .collect();
    let mut visited = 0;

    while let Some(node) = queue.pop() {
        visited += 1;
        for stage in &workflow.stages {
            if stage.depends_on.iter().any(|d| d == node) {
                let deg = in_degree.get_mut(stage.id.as_str()).unwrap();
                *deg -= 1;
                if *deg == 0 {
                    queue.push(&stage.id);
                }
            }
        }
    }

    if visited < workflow.stages.len() {
        errors.push("Workflow contains a cycle".into());
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_information_workflow_valid() {
        let wf = information_workflow();
        assert!(validate_workflow(&wf).is_ok());
        assert_eq!(wf.stages.len(), 5);
    }

    #[test]
    fn test_workflow_serialization() {
        let wf = information_workflow();
        let json = serde_json::to_string_pretty(&wf).unwrap();
        let parsed: WorkflowDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, "info-assistant");
    }

    #[test]
    fn test_validate_missing_dependency() {
        let wf = WorkflowDefinition {
            id: "bad".into(),
            name: "Bad".into(),
            description: "".into(),
            stages: vec![WorkflowStage {
                id: "s1".into(),
                stage_type: StageType::LlmCall,
                config: HashMap::new(),
                depends_on: vec!["nonexistent".into()],
                optional: false,
                timeout_secs: 10,
            }],
            trigger_topic: "test.*".into(),
            enabled: true,
        };

        let result = validate_workflow(&wf);
        assert!(result.is_err());
        assert!(result.unwrap_err()[0].contains("nonexistent"));
    }

    #[test]
    fn test_validate_self_dependency() {
        let wf = WorkflowDefinition {
            id: "bad".into(),
            name: "Bad".into(),
            description: "".into(),
            stages: vec![WorkflowStage {
                id: "s1".into(),
                stage_type: StageType::LlmCall,
                config: HashMap::new(),
                depends_on: vec!["s1".into()],
                optional: false,
                timeout_secs: 10,
            }],
            trigger_topic: "test.*".into(),
            enabled: true,
        };

        let result = validate_workflow(&wf);
        assert!(result.is_err());
    }
}
