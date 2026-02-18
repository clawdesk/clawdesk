//! Multi-agent fan-out gateway.
//!
//! Supports four strategies:
//!
//! | Strategy     | Behaviour                                          |
//! |-------------|-----------------------------------------------------|
//! | `parallel`  | Run agents concurrently, merge outputs              |
//! | `sequential`| Run agents in order, pipe output → next input       |
//! | `merge`     | Run agents, feed all outputs to a merge agent       |
//! | `vote`      | Run agents, select majority/best output             |
//!
//! Configuration is TOML-based:
//!
//! ```toml
//! [fanout]
//! strategy = "parallel"
//! agents = ["researcher", "designer", "builder"]
//! merge_agent = "synthesiser"
//! timeout_secs = 120
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Fanout configuration
// ---------------------------------------------------------------------------

/// Fan-out execution strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FanoutStrategy {
    /// Run all agents in parallel, merge outputs.
    Parallel,
    /// Run agents sequentially, chaining output → input.
    Sequential,
    /// Run agents in parallel, then pass all outputs to a merge agent.
    Merge,
    /// Run agents in parallel, pick the best/majority output.
    Vote,
}

/// Fan-out configuration (parsed from TOML).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanoutConfig {
    /// Execution strategy.
    pub strategy: FanoutStrategy,
    /// Agent IDs to fan out to.
    pub agents: Vec<String>,
    /// Agent that synthesises the merged output (required for `merge` mode).
    pub merge_agent: Option<String>,
    /// Global timeout in seconds.
    #[serde(default = "default_fanout_timeout")]
    pub timeout_secs: u64,
    /// Maximum concurrent agents (for `parallel`/`merge`/`vote`).
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent: usize,
    /// For `vote`, minimum agreement threshold (0.0–1.0).
    #[serde(default = "default_vote_threshold")]
    pub vote_threshold: f64,
}

fn default_fanout_timeout() -> u64 { 120 }
fn default_max_concurrent() -> usize { 10 }
fn default_vote_threshold() -> f64 { 0.5 }

// ---------------------------------------------------------------------------
// Fanout execution plan
// ---------------------------------------------------------------------------

/// A planned fan-out execution (pre-run).
#[derive(Debug, Clone)]
pub struct FanoutPlan {
    pub config: FanoutConfig,
    pub input: String,
    /// Computed execution phases.
    pub phases: Vec<FanoutPhase>,
}

/// A phase in the fan-out plan.
#[derive(Debug, Clone)]
pub enum FanoutPhase {
    /// Run these agents concurrently.
    Concurrent(Vec<String>),
    /// Run this single agent with accumulated outputs.
    MergeStep(String),
    /// Vote on outputs.
    VoteStep { threshold: f64 },
}

/// Build an execution plan from config + input.
pub fn plan_fanout(config: &FanoutConfig, input: &str) -> Result<FanoutPlan, String> {
    if config.agents.is_empty() {
        return Err("Fan-out requires at least one agent".into());
    }

    let mut phases = Vec::new();

    match config.strategy {
        FanoutStrategy::Parallel => {
            phases.push(FanoutPhase::Concurrent(config.agents.clone()));
        }
        FanoutStrategy::Sequential => {
            // Each agent gets its own sequential phase.
            for agent in &config.agents {
                phases.push(FanoutPhase::Concurrent(vec![agent.clone()]));
            }
        }
        FanoutStrategy::Merge => {
            phases.push(FanoutPhase::Concurrent(config.agents.clone()));
            let merge = config.merge_agent.as_ref().ok_or(
                "Merge strategy requires a merge_agent".to_string(),
            )?;
            phases.push(FanoutPhase::MergeStep(merge.clone()));
        }
        FanoutStrategy::Vote => {
            phases.push(FanoutPhase::Concurrent(config.agents.clone()));
            phases.push(FanoutPhase::VoteStep {
                threshold: config.vote_threshold,
            });
        }
    }

    Ok(FanoutPlan {
        config: config.clone(),
        input: input.to_string(),
        phases,
    })
}

// ---------------------------------------------------------------------------
// Fanout result
// ---------------------------------------------------------------------------

/// Result of a fan-out execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FanoutResult {
    /// Per-agent outputs.
    pub agent_outputs: HashMap<String, AgentOutput>,
    /// Final merged output.
    pub final_output: String,
    /// Strategy used.
    pub strategy: FanoutStrategy,
    /// Total execution time in ms.
    pub total_duration_ms: u64,
}

/// Output from a single agent in a fan-out.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentOutput {
    pub agent_id: String,
    pub output: String,
    pub duration_ms: u64,
    pub success: bool,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Output merging
// ---------------------------------------------------------------------------

/// Merge agent outputs by concatenation.
pub fn merge_concatenate(outputs: &[AgentOutput]) -> String {
    outputs
        .iter()
        .filter(|o| o.success)
        .map(|o| format!("## {}\n\n{}", o.agent_id, o.output))
        .collect::<Vec<_>>()
        .join("\n\n---\n\n")
}

/// Merge by taking the first successful output.
pub fn merge_first_success(outputs: &[AgentOutput]) -> Option<String> {
    outputs
        .iter()
        .find(|o| o.success)
        .map(|o| o.output.clone())
}

/// Simple majority vote: if multiple outputs are identical, pick the most common.
pub fn merge_vote(outputs: &[AgentOutput], threshold: f64) -> Option<String> {
    let successful: Vec<_> = outputs.iter().filter(|o| o.success).collect();
    if successful.is_empty() {
        return None;
    }

    let mut counts: HashMap<&str, usize> = HashMap::new();
    for o in &successful {
        *counts.entry(o.output.as_str()).or_default() += 1;
    }

    let total = successful.len();
    let (best_output, count) = counts.into_iter().max_by_key(|(_, c)| *c)?;

    if (count as f64 / total as f64) >= threshold {
        Some(best_output.to_string())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// TOML parsing
// ---------------------------------------------------------------------------

/// Parse a fanout config from TOML.
pub fn parse_fanout_toml(content: &str) -> Result<FanoutConfig, String> {
    #[derive(Deserialize)]
    struct Wrapper {
        fanout: FanoutConfig,
    }
    let wrapper: Wrapper =
        toml::from_str(content).map_err(|e| format!("Invalid fanout TOML: {e}"))?;
    Ok(wrapper.fanout)
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate a fanout configuration.
pub fn validate_fanout(config: &FanoutConfig) -> Vec<String> {
    let mut errors = Vec::new();

    if config.agents.is_empty() {
        errors.push("No agents specified".into());
    }

    if config.strategy == FanoutStrategy::Merge && config.merge_agent.is_none() {
        errors.push("Merge strategy requires a merge_agent".into());
    }

    if config.vote_threshold < 0.0 || config.vote_threshold > 1.0 {
        errors.push("vote_threshold must be between 0.0 and 1.0".into());
    }

    if config.max_concurrent == 0 {
        errors.push("max_concurrent must be > 0".into());
    }

    // Check for duplicate agents.
    let mut seen = std::collections::HashSet::new();
    for agent in &config.agents {
        if !seen.insert(agent) {
            errors.push(format!("Duplicate agent: {agent}"));
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_fanout_toml() -> &'static str {
        r#"
[fanout]
strategy = "parallel"
agents = ["researcher", "designer", "builder"]
timeout_secs = 120
"#
    }

    #[test]
    fn test_parse_fanout_toml() {
        let config = parse_fanout_toml(sample_fanout_toml()).unwrap();
        assert_eq!(config.strategy, FanoutStrategy::Parallel);
        assert_eq!(config.agents.len(), 3);
        assert_eq!(config.timeout_secs, 120);
    }

    #[test]
    fn test_validate_ok() {
        let config = parse_fanout_toml(sample_fanout_toml()).unwrap();
        let errors = validate_fanout(&config);
        assert!(errors.is_empty());
    }

    #[test]
    fn test_validate_merge_missing_agent() {
        let toml = r#"
[fanout]
strategy = "merge"
agents = ["a", "b"]
"#;
        let config = parse_fanout_toml(toml).unwrap();
        let errors = validate_fanout(&config);
        assert!(errors.iter().any(|e| e.contains("merge_agent")));
    }

    #[test]
    fn test_plan_parallel() {
        let config = parse_fanout_toml(sample_fanout_toml()).unwrap();
        let plan = plan_fanout(&config, "test input").unwrap();
        assert_eq!(plan.phases.len(), 1);
        matches!(&plan.phases[0], FanoutPhase::Concurrent(agents) if agents.len() == 3);
    }

    #[test]
    fn test_plan_sequential() {
        let toml = r#"
[fanout]
strategy = "sequential"
agents = ["a", "b", "c"]
"#;
        let config = parse_fanout_toml(toml).unwrap();
        let plan = plan_fanout(&config, "input").unwrap();
        assert_eq!(plan.phases.len(), 3); // one per agent
    }

    #[test]
    fn test_plan_merge() {
        let toml = r#"
[fanout]
strategy = "merge"
agents = ["a", "b"]
merge_agent = "synthesiser"
"#;
        let config = parse_fanout_toml(toml).unwrap();
        let plan = plan_fanout(&config, "input").unwrap();
        assert_eq!(plan.phases.len(), 2);
    }

    #[test]
    fn test_merge_concatenate() {
        let outputs = vec![
            AgentOutput {
                agent_id: "a".into(),
                output: "Output A".into(),
                duration_ms: 100,
                success: true,
                error: None,
            },
            AgentOutput {
                agent_id: "b".into(),
                output: "Output B".into(),
                duration_ms: 200,
                success: true,
                error: None,
            },
        ];
        let merged = merge_concatenate(&outputs);
        assert!(merged.contains("Output A"));
        assert!(merged.contains("Output B"));
    }

    #[test]
    fn test_merge_first_success() {
        let outputs = vec![
            AgentOutput {
                agent_id: "a".into(),
                output: "fail".into(),
                duration_ms: 100,
                success: false,
                error: Some("err".into()),
            },
            AgentOutput {
                agent_id: "b".into(),
                output: "success".into(),
                duration_ms: 200,
                success: true,
                error: None,
            },
        ];
        assert_eq!(merge_first_success(&outputs).as_deref(), Some("success"));
    }

    #[test]
    fn test_merge_vote() {
        let outputs = vec![
            AgentOutput { agent_id: "a".into(), output: "yes".into(), duration_ms: 0, success: true, error: None },
            AgentOutput { agent_id: "b".into(), output: "yes".into(), duration_ms: 0, success: true, error: None },
            AgentOutput { agent_id: "c".into(), output: "no".into(), duration_ms: 0, success: true, error: None },
        ];
        let result = merge_vote(&outputs, 0.5).unwrap();
        assert_eq!(result, "yes");
    }

    #[test]
    fn test_merge_vote_below_threshold() {
        let outputs = vec![
            AgentOutput { agent_id: "a".into(), output: "x".into(), duration_ms: 0, success: true, error: None },
            AgentOutput { agent_id: "b".into(), output: "y".into(), duration_ms: 0, success: true, error: None },
            AgentOutput { agent_id: "c".into(), output: "z".into(), duration_ms: 0, success: true, error: None },
        ];
        // All different, none exceeds 1.0 threshold.
        let result = merge_vote(&outputs, 1.0);
        assert!(result.is_none());
    }

    #[test]
    fn test_validate_duplicate_agents() {
        let toml = r#"
[fanout]
strategy = "parallel"
agents = ["a", "b", "a"]
"#;
        let config = parse_fanout_toml(toml).unwrap();
        let errors = validate_fanout(&config);
        assert!(errors.iter().any(|e| e.contains("Duplicate")));
    }
}
