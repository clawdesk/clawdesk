//! Agent pipeline — composable DAG of agent steps.
//!
//! ## Design rationale
//!
//! OpenClaw's multi-agent is config-driven channel routing: each agent is
//! an isolated island with no inter-agent communication. If "home" needs
//! "work" to do something, the user switches channels manually.
//!
//! ClawDesk's A2A protocol (`AgentCard`, `A2AMessageKind`) already defines
//! inter-agent communication. This module builds on that foundation to
//! enable **composable computation graphs over agents**:
//!
//! - **Sequential**: A → B → C (output of A feeds into B)
//! - **Parallel**: A + B → merge (fan-out, collect results)
//! - **Conditional**: router picks branch based on previous output
//! - **Human-in-the-loop**: gate pauses for approval before proceeding
//!
//! ## Example
//!
//! ```rust,ignore
//! let pipeline = PipelineBuilder::new()
//!     .parallel(vec![
//!         Step::agent("researcher", Some("core/web-search")),
//!         Step::agent("analyst", Some("core/data-analysis")),
//!     ], MergeStrategy::Structured)
//!     .gate("Review research before drafting?", Duration::from_secs(300))
//!     .agent("writer", Some("core/document-gen"))
//!     .build();
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Pipeline types
// ---------------------------------------------------------------------------

/// A directed acyclic graph of agent computation steps.
///
/// Steps are executed in topological order. Edges define data flow:
/// the output of step `i` is available as input to step `j` when
/// there exists an edge `(i, j)`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPipeline {
    /// Steps in the pipeline, indexed by position.
    pub steps: Vec<PipelineStep>,
    /// Directed edges: `(from_step_index, to_step_index)`.
    pub edges: Vec<(usize, usize)>,
    /// What to do when a step fails.
    pub error_policy: ErrorPolicy,
    /// Pipeline-level metadata.
    pub metadata: PipelineMetadata,
}

/// Metadata about the pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineMetadata {
    /// Human-readable name for this pipeline.
    pub name: String,
    /// Description of what the pipeline does.
    pub description: Option<String>,
    /// Version string.
    pub version: String,
    /// Who created this pipeline.
    pub author: Option<String>,
}

/// A single step in the pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PipelineStep {
    /// Single agent invocation with optional skill routing.
    Agent {
        /// Agent identifier (must be registered in the agent registry).
        agent_id: String,
        /// Optional skill to activate for this step.
        skill_id: Option<String>,
        /// Optional input transformation (JSONPath or template).
        input_transform: Option<String>,
        /// Timeout for this step.
        #[serde(default = "default_step_timeout_secs")]
        timeout_secs: u64,
    },

    /// Fan-out: run multiple sub-steps in parallel, collect/merge results.
    Parallel {
        /// Branches to execute concurrently.
        branches: Vec<PipelineStep>,
        /// How to merge branch results.
        merge: MergeStrategy,
    },

    /// Conditional routing based on previous step output.
    Router {
        /// Condition to evaluate against the previous step's output.
        condition: RoutingCondition,
        /// Named routes: `(route_name, step)`.
        routes: Vec<(String, PipelineStep)>,
        /// Default route if no condition matches.
        default_route: Option<Box<PipelineStep>>,
    },

    /// Human-in-the-loop approval gate.
    Gate {
        /// Prompt shown to the human reviewer.
        prompt: String,
        /// How long to wait for approval before using the default.
        #[serde(default = "default_gate_timeout_secs")]
        timeout_secs: u64,
        /// What happens if the timeout expires.
        default_action: GateDefault,
    },

    /// Transform the previous step's output without calling an agent.
    Transform {
        /// JSONPath expression or template to apply.
        expression: String,
        /// Description for debugging.
        description: Option<String>,
    },
}

/// How to merge results from parallel branches.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MergeStrategy {
    /// Concatenate all results into a single string.
    Concat,
    /// Return structured map of `branch_index → result`.
    Structured,
    /// Pick the first successful result (fail-fast).
    FirstSuccess,
    /// Pick the result with the highest confidence/quality score.
    Best {
        /// Field name in the result to use as score.
        score_field: String,
    },
}

/// Condition for routing decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RoutingCondition {
    /// Route based on a keyword in the previous output.
    ContainsKeyword { keywords: Vec<String> },
    /// Route based on a JSONPath expression evaluating to true/false.
    JsonPath { expression: String },
    /// Route based on output length (short vs long responses).
    OutputLength { threshold: usize },
    /// Always route to the first matching route.
    Always,
}

/// What happens when a gate times out.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateDefault {
    /// Continue with the pipeline.
    Proceed,
    /// Abort the pipeline.
    Abort,
    /// Skip this step and continue.
    Skip,
}

/// What happens when a step fails.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorPolicy {
    /// Fail the entire pipeline on any step failure.
    FailFast,
    /// Continue with remaining steps, collect errors.
    ContinueOnError,
    /// Retry the failed step up to N times.
    Retry { max_attempts: usize, backoff_ms: u64 },
}

impl Default for ErrorPolicy {
    fn default() -> Self {
        Self::FailFast
    }
}

fn default_step_timeout_secs() -> u64 {
    60
}

fn default_gate_timeout_secs() -> u64 {
    300
}

// ---------------------------------------------------------------------------
// Pipeline execution result
// ---------------------------------------------------------------------------

/// Result of a pipeline step execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepResult {
    /// Index of the step in the pipeline.
    pub step_index: usize,
    /// Whether the step succeeded.
    pub success: bool,
    /// Output value (typically JSON or text).
    pub output: serde_json::Value,
    /// Duration of execution.
    pub duration_ms: u64,
    /// Error message if failed.
    pub error: Option<String>,
    /// Nested results for Parallel steps.
    pub sub_results: Vec<StepResult>,
}

/// Result of an entire pipeline execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineResult {
    /// Pipeline name.
    pub pipeline_name: String,
    /// Overall success.
    pub success: bool,
    /// Per-step results.
    pub steps: Vec<StepResult>,
    /// Final output (output of the last step).
    pub final_output: serde_json::Value,
    /// Total duration of the pipeline.
    pub total_duration_ms: u64,
    /// Errors encountered during execution.
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------------------
// Builder API
// ---------------------------------------------------------------------------

/// Fluent builder for constructing pipelines.
///
/// ```rust,ignore
/// let pipeline = PipelineBuilder::new("Research & Draft")
///     .parallel(vec![
///         PipelineStep::Agent {
///             agent_id: "researcher".into(),
///             skill_id: Some("core/web-search".into()),
///             input_transform: None,
///             timeout_secs: 60,
///         },
///         PipelineStep::Agent {
///             agent_id: "analyst".into(),
///             skill_id: Some("core/data-analysis".into()),
///             input_transform: None,
///             timeout_secs: 60,
///         },
///     ], MergeStrategy::Structured)
///     .gate("Review before drafting?", Duration::from_secs(300))
///     .agent("writer", Some("core/document-gen"))
///     .build();
/// ```
pub struct PipelineBuilder {
    steps: Vec<PipelineStep>,
    name: String,
    description: Option<String>,
    error_policy: ErrorPolicy,
}

impl PipelineBuilder {
    /// Create a new pipeline builder with a name.
    pub fn new(name: &str) -> Self {
        Self {
            steps: Vec::new(),
            name: name.to_string(),
            description: None,
            error_policy: ErrorPolicy::FailFast,
        }
    }

    /// Set the pipeline description.
    pub fn description(mut self, desc: &str) -> Self {
        self.description = Some(desc.to_string());
        self
    }

    /// Set the error policy.
    pub fn error_policy(mut self, policy: ErrorPolicy) -> Self {
        self.error_policy = policy;
        self
    }

    /// Add a single agent step.
    pub fn agent(mut self, agent_id: &str, skill_id: Option<&str>) -> Self {
        self.steps.push(PipelineStep::Agent {
            agent_id: agent_id.to_string(),
            skill_id: skill_id.map(|s| s.to_string()),
            input_transform: None,
            timeout_secs: default_step_timeout_secs(),
        });
        self
    }

    /// Add an agent step with a custom input transform.
    pub fn agent_with_transform(
        mut self,
        agent_id: &str,
        skill_id: Option<&str>,
        transform: &str,
    ) -> Self {
        self.steps.push(PipelineStep::Agent {
            agent_id: agent_id.to_string(),
            skill_id: skill_id.map(|s| s.to_string()),
            input_transform: Some(transform.to_string()),
            timeout_secs: default_step_timeout_secs(),
        });
        self
    }

    /// Add a parallel fan-out step.
    pub fn parallel(mut self, branches: Vec<PipelineStep>, merge: MergeStrategy) -> Self {
        self.steps.push(PipelineStep::Parallel { branches, merge });
        self
    }

    /// Add a human-in-the-loop gate.
    pub fn gate(mut self, prompt: &str, timeout: Duration) -> Self {
        self.steps.push(PipelineStep::Gate {
            prompt: prompt.to_string(),
            timeout_secs: timeout.as_secs(),
            default_action: GateDefault::Proceed,
        });
        self
    }

    /// Add a conditional router.
    pub fn router(
        mut self,
        condition: RoutingCondition,
        routes: Vec<(&str, PipelineStep)>,
    ) -> Self {
        self.steps.push(PipelineStep::Router {
            condition,
            routes: routes
                .into_iter()
                .map(|(name, step)| (name.to_string(), step))
                .collect(),
            default_route: None,
        });
        self
    }

    /// Add a transform step.
    pub fn transform(mut self, expression: &str) -> Self {
        self.steps.push(PipelineStep::Transform {
            expression: expression.to_string(),
            description: None,
        });
        self
    }

    /// Build the pipeline.
    ///
    /// Edges are inferred as a linear chain: step 0 → step 1 → step 2 → ...
    /// For more complex DAGs, construct `AgentPipeline` directly.
    pub fn build(self) -> AgentPipeline {
        let edges: Vec<(usize, usize)> = (0..self.steps.len().saturating_sub(1))
            .map(|i| (i, i + 1))
            .collect();

        AgentPipeline {
            steps: self.steps,
            edges,
            error_policy: self.error_policy,
            metadata: PipelineMetadata {
                name: self.name,
                description: self.description,
                version: "0.1.0".to_string(),
                author: None,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Pipeline validation
// ---------------------------------------------------------------------------

impl AgentPipeline {
    /// Validate the pipeline structure.
    ///
    /// Checks:
    /// - At least one step exists.
    /// - All edges reference valid step indices.
    /// - No cycles (DAG property).
    /// - Agent IDs are non-empty.
    pub fn validate(&self) -> Result<(), Vec<String>> {
        let mut errors = Vec::new();

        if self.steps.is_empty() {
            errors.push("pipeline has no steps".into());
        }

        // Check edge validity.
        for (from, to) in &self.edges {
            if *from >= self.steps.len() {
                errors.push(format!("edge source {} out of bounds", from));
            }
            if *to >= self.steps.len() {
                errors.push(format!("edge target {} out of bounds", to));
            }
            if from >= to {
                errors.push(format!("edge ({}, {}) creates a cycle or self-loop", from, to));
            }
        }

        // Check agent IDs.
        for (i, step) in self.steps.iter().enumerate() {
            self.validate_step(step, i, &mut errors);
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn validate_step(&self, step: &PipelineStep, index: usize, errors: &mut Vec<String>) {
        match step {
            PipelineStep::Agent { agent_id, .. } => {
                if agent_id.is_empty() {
                    errors.push(format!("step {} has empty agent_id", index));
                }
            }
            PipelineStep::Parallel { branches, .. } => {
                if branches.is_empty() {
                    errors.push(format!("step {} (Parallel) has no branches", index));
                }
                for (bi, branch) in branches.iter().enumerate() {
                    self.validate_step(branch, index * 100 + bi, errors);
                }
            }
            PipelineStep::Router { routes, .. } => {
                if routes.is_empty() {
                    errors.push(format!("step {} (Router) has no routes", index));
                }
            }
            PipelineStep::Gate { prompt, .. } => {
                if prompt.is_empty() {
                    errors.push(format!("step {} (Gate) has empty prompt", index));
                }
            }
            PipelineStep::Transform { expression, .. } => {
                if expression.is_empty() {
                    errors.push(format!("step {} (Transform) has empty expression", index));
                }
            }
        }
    }

    /// Get agent IDs referenced by this pipeline (for pre-validation).
    pub fn referenced_agents(&self) -> Vec<String> {
        let mut agents = Vec::new();
        for step in &self.steps {
            Self::collect_agents(step, &mut agents);
        }
        agents.sort();
        agents.dedup();
        agents
    }

    fn collect_agents(step: &PipelineStep, agents: &mut Vec<String>) {
        match step {
            PipelineStep::Agent { agent_id, .. } => {
                agents.push(agent_id.clone());
            }
            PipelineStep::Parallel { branches, .. } => {
                for branch in branches {
                    Self::collect_agents(branch, agents);
                }
            }
            PipelineStep::Router { routes, default_route, .. } => {
                for (_, route_step) in routes {
                    Self::collect_agents(route_step, agents);
                }
                if let Some(default) = default_route {
                    Self::collect_agents(default, agents);
                }
            }
            _ => {}
        }
    }

    /// Compute the topological order of steps.
    ///
    /// For a linear pipeline this is trivial (0, 1, 2, ...),
    /// but for complex DAGs this ensures correct execution order.
    pub fn topological_order(&self) -> Result<Vec<usize>, String> {
        let n = self.steps.len();
        let mut in_degree = vec![0usize; n];
        let mut adj: HashMap<usize, Vec<usize>> = HashMap::new();

        for &(from, to) in &self.edges {
            in_degree[to] += 1;
            adj.entry(from).or_default().push(to);
        }

        // Kahn's algorithm.
        let mut queue: Vec<usize> = (0..n)
            .filter(|&i| in_degree[i] == 0)
            .collect();
        let mut order = Vec::with_capacity(n);

        while let Some(node) = queue.pop() {
            order.push(node);
            if let Some(neighbors) = adj.get(&node) {
                for &next in neighbors {
                    in_degree[next] -= 1;
                    if in_degree[next] == 0 {
                        queue.push(next);
                    }
                }
            }
        }

        if order.len() != n {
            Err("pipeline contains a cycle".into())
        } else {
            Ok(order)
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_creates_linear_pipeline() {
        let pipeline = PipelineBuilder::new("Test Pipeline")
            .agent("researcher", Some("core/web-search"))
            .agent("writer", Some("core/document-gen"))
            .build();

        assert_eq!(pipeline.steps.len(), 2);
        assert_eq!(pipeline.edges, vec![(0, 1)]);
        assert!(pipeline.validate().is_ok());
    }

    #[test]
    fn builder_with_parallel_step() {
        let pipeline = PipelineBuilder::new("Parallel Test")
            .parallel(
                vec![
                    PipelineStep::Agent {
                        agent_id: "researcher".into(),
                        skill_id: None,
                        input_transform: None,
                        timeout_secs: 60,
                    },
                    PipelineStep::Agent {
                        agent_id: "analyst".into(),
                        skill_id: None,
                        input_transform: None,
                        timeout_secs: 60,
                    },
                ],
                MergeStrategy::Structured,
            )
            .agent("writer", None)
            .build();

        assert_eq!(pipeline.steps.len(), 2);
        assert!(pipeline.validate().is_ok());
    }

    #[test]
    fn builder_with_gate() {
        let pipeline = PipelineBuilder::new("Gate Test")
            .agent("researcher", None)
            .gate("Review results?", Duration::from_secs(300))
            .agent("writer", None)
            .build();

        assert_eq!(pipeline.steps.len(), 3);
        assert_eq!(pipeline.edges, vec![(0, 1), (1, 2)]);
        assert!(pipeline.validate().is_ok());
    }

    #[test]
    fn validation_catches_empty_agent_id() {
        let pipeline = PipelineBuilder::new("Bad")
            .agent("", None)
            .build();

        let errors = pipeline.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("empty agent_id")));
    }

    #[test]
    fn validation_catches_empty_parallel() {
        let pipeline = AgentPipeline {
            steps: vec![PipelineStep::Parallel {
                branches: vec![],
                merge: MergeStrategy::Concat,
            }],
            edges: vec![],
            error_policy: ErrorPolicy::FailFast,
            metadata: PipelineMetadata {
                name: "Bad".into(),
                description: None,
                version: "0.1.0".into(),
                author: None,
            },
        };

        let errors = pipeline.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("no branches")));
    }

    #[test]
    fn topological_order_linear() {
        let pipeline = PipelineBuilder::new("Linear")
            .agent("a", None)
            .agent("b", None)
            .agent("c", None)
            .build();

        let order = pipeline.topological_order().unwrap();
        assert_eq!(order, vec![0, 1, 2]);
    }

    #[test]
    fn referenced_agents() {
        let pipeline = PipelineBuilder::new("Multi-Agent")
            .parallel(
                vec![
                    PipelineStep::Agent {
                        agent_id: "researcher".into(),
                        skill_id: None,
                        input_transform: None,
                        timeout_secs: 60,
                    },
                    PipelineStep::Agent {
                        agent_id: "analyst".into(),
                        skill_id: None,
                        input_transform: None,
                        timeout_secs: 60,
                    },
                ],
                MergeStrategy::Structured,
            )
            .agent("writer", None)
            .build();

        let agents = pipeline.referenced_agents();
        assert_eq!(agents, vec!["analyst", "researcher", "writer"]);
    }

    #[test]
    fn pipeline_serializes_to_json() {
        let pipeline = PipelineBuilder::new("Serializable")
            .agent("worker", Some("core/task"))
            .build();

        let json = serde_json::to_string(&pipeline).unwrap();
        let restored: AgentPipeline = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.metadata.name, "Serializable");
        assert_eq!(restored.steps.len(), 1);
    }
}
