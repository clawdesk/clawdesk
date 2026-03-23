//! Intelligent Agent Selector — LinUCB contextual bandit for agent routing.
//!
//! Replaces static Aho-Corasick keyword matching with learned performance-based
//! agent selection. Uses the same LinUCB algorithm as `task_router.rs` but with
//! agent IDs as arms and agent-specific features.
//!
//! The selector learns from execution feedback: "for this type of task, from
//! this type of user, which agent succeeds most often?"
//!
//! ## Features Vector (D=8)
//!
//! 1. `is_coding` — task involves code
//! 2. `is_research` — task involves research/analysis
//! 3. `is_ops` — task involves operations/infrastructure  
//! 4. `is_creative` — task involves writing/design
//! 5. `complexity` — estimated from token count
//! 6. `user_expertise` — from user model (0.0 = novice, 1.0 = expert)
//! 7. `urgency` — from user frustration level
//! 8. `bias` — constant 1.0 term

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::debug;

use crate::workspace::{CognitiveEvent, GlobalWorkspace};

/// Feature dimension for agent selection.
const AGENT_D: usize = 8;

/// Features extracted for agent selection decisions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentFeatures {
    pub is_coding: bool,
    pub is_research: bool,
    pub is_ops: bool,
    pub is_creative: bool,
    pub complexity: f64,
    pub user_expertise: f64,
    pub urgency: f64,
}

impl AgentFeatures {
    fn to_vector(&self) -> [f64; AGENT_D] {
        [
            boolf(self.is_coding),
            boolf(self.is_research),
            boolf(self.is_ops),
            boolf(self.is_creative),
            self.complexity.clamp(0.0, 1.0),
            self.user_expertise.clamp(0.0, 1.0),
            self.urgency.clamp(0.0, 1.0),
            1.0, // bias
        ]
    }
}

fn boolf(v: bool) -> f64 {
    if v { 1.0 } else { 0.0 }
}

/// Agent capability profile (loaded from TOML configs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCapabilities {
    pub agent_id: String,
    pub domains: Vec<String>,
    pub tools_allowed: Vec<String>,
    pub min_complexity: f64,
    pub max_complexity: f64,
}

/// Candidate agent with selection score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCandidate {
    pub agent_id: String,
    pub score: f64,
    pub predicted_reward: f64,
    pub exploration_bonus: f64,
}

/// Performance tracking per agent.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentPerformance {
    pub total_tasks: u32,
    pub successes: u32,
    pub avg_turns: f64,
    pub avg_cost: f64,
    pub veto_rate: f64,
}

impl AgentPerformance {
    pub fn success_rate(&self) -> f64 {
        if self.total_tasks == 0 {
            return 0.5; // uninformative prior
        }
        self.successes as f64 / self.total_tasks as f64
    }
}

/// LinUCB arm for one agent.
#[derive(Debug, Clone)]
struct LinUcbArm {
    a: [f64; AGENT_D * AGENT_D],
    a_inv: [f64; AGENT_D * AGENT_D],
    b: [f64; AGENT_D],
}

impl LinUcbArm {
    fn new() -> Self {
        let mut a = [0.0; AGENT_D * AGENT_D];
        let mut a_inv = [0.0; AGENT_D * AGENT_D];
        for i in 0..AGENT_D {
            a[i * AGENT_D + i] = 1.0;
            a_inv[i * AGENT_D + i] = 1.0;
        }
        Self {
            a,
            a_inv,
            b: [0.0; AGENT_D],
        }
    }

    /// Sherman-Morrison rank-1 update: O(D²).
    fn update(&mut self, x: &[f64; AGENT_D], reward: f64) {
        // b += reward * x
        for i in 0..AGENT_D {
            self.b[i] += reward * x[i];
        }
        // A += x * x^T
        for i in 0..AGENT_D {
            for j in 0..AGENT_D {
                self.a[i * AGENT_D + j] += x[i] * x[j];
            }
        }
        // Sherman-Morrison: A_inv -= (A_inv * x)(x^T * A_inv) / (1 + x^T * A_inv * x)
        let u = mat_vec(&self.a_inv, x);
        let denom = 1.0 + dot(x, &u);
        if denom.abs() < 1e-12 {
            return;
        }
        let inv_denom = 1.0 / denom;
        for i in 0..AGENT_D {
            for j in 0..AGENT_D {
                self.a_inv[i * AGENT_D + j] -= u[i] * u[j] * inv_denom;
            }
        }
    }

    /// Predict reward and UCB bonus.
    fn predict_and_bonus(&self, x: &[f64; AGENT_D], alpha: f64) -> (f64, f64) {
        let theta = mat_vec(&self.a_inv, &self.b);
        let pred = dot(&theta, x);
        let z = mat_vec(&self.a_inv, x);
        let quad = dot(x, &z).max(0.0);
        let bonus = alpha * quad.sqrt();
        (pred, bonus)
    }
}

/// Matrix-vector multiply for AGENT_D × AGENT_D matrix.
fn mat_vec(m: &[f64; AGENT_D * AGENT_D], v: &[f64; AGENT_D]) -> [f64; AGENT_D] {
    let mut result = [0.0; AGENT_D];
    for i in 0..AGENT_D {
        let mut sum = 0.0;
        for j in 0..AGENT_D {
            sum += m[i * AGENT_D + j] * v[j];
        }
        result[i] = sum;
    }
    result
}

/// Dot product of two AGENT_D vectors.
fn dot(a: &[f64; AGENT_D], b: &[f64; AGENT_D]) -> f64 {
    let mut sum = 0.0;
    for i in 0..AGENT_D {
        sum += a[i] * b[i];
    }
    sum
}

/// The Agent Selector — picks the best agent for a task using learned performance history.
pub struct AgentSelector {
    /// LinUCB arms, one per agent ID.
    arms: FxHashMap<String, LinUcbArm>,
    /// Agent capability profiles.
    capabilities: FxHashMap<String, AgentCapabilities>,
    /// Performance statistics.
    performance: FxHashMap<String, AgentPerformance>,
    /// Base exploration rate.
    alpha: f64,
    /// Total feedback observations (for Lai-Robbins alpha decay).
    total_feedback: u64,
    /// Minimum samples before trusting the bandit over heuristics.
    min_samples: u32,
    /// Global workspace for events.
    workspace: Option<Arc<GlobalWorkspace>>,
}

impl AgentSelector {
    pub fn new(alpha: f64) -> Self {
        Self {
            arms: FxHashMap::default(),
            capabilities: FxHashMap::default(),
            performance: FxHashMap::default(),
            alpha,
            total_feedback: 0,
            min_samples: 5,
            workspace: None,
        }
    }

    /// Connect to the global workspace.
    pub fn with_global_workspace(mut self, ws: Arc<GlobalWorkspace>) -> Self {
        self.workspace = Some(ws);
        self
    }

    /// Register an agent's capabilities (typically from TOML config).
    pub fn register_agent(&mut self, caps: AgentCapabilities) {
        self.arms.entry(caps.agent_id.clone()).or_insert_with(LinUcbArm::new);
        self.capabilities.insert(caps.agent_id.clone(), caps);
    }

    /// Select the best agent(s) for a task.
    ///
    /// Returns candidates sorted by score (highest first).
    /// The caller should use the top candidate, or fallback to lower ones.
    pub fn select(&self, features: &AgentFeatures) -> Vec<AgentCandidate> {
        let x = features.to_vector();
        let alpha = self.effective_alpha();

        let mut candidates: Vec<AgentCandidate> = self.capabilities.keys()
            .filter(|id| self.can_handle(id, features))
            .map(|id| {
                let (pred, bonus) = self.arms.get(id)
                    .map(|arm| arm.predict_and_bonus(&x, alpha))
                    .unwrap_or((0.5, 0.5)); // uninformative prior for new agents

                AgentCandidate {
                    agent_id: id.clone(),
                    score: pred + bonus,
                    predicted_reward: pred,
                    exploration_bonus: bonus,
                }
            })
            .collect();

        candidates.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        candidates
    }

    /// Record outcome feedback — the critical learning step.
    ///
    /// Reward should be in [0.0, 1.0]:
    /// - 1.0 = task completed successfully, user satisfied
    /// - 0.5 = completed with issues
    /// - 0.0 = failed or user unsatisfied
    pub fn record_outcome(
        &mut self,
        agent_id: &str,
        features: &AgentFeatures,
        reward: f64,
    ) {
        let x = features.to_vector();
        self.total_feedback += 1;

        // Update LinUCB arm
        let arm = self.arms.entry(agent_id.to_string()).or_insert_with(LinUcbArm::new);
        arm.update(&x, reward);

        // Update performance stats
        let perf = self.performance.entry(agent_id.to_string()).or_default();
        perf.total_tasks += 1;
        if reward > 0.5 {
            perf.successes += 1;
        }

        debug!(
            agent = agent_id,
            reward,
            total_feedback = self.total_feedback,
            success_rate = perf.success_rate(),
            "agent selector feedback recorded"
        );

        // Publish to global workspace
        if let Some(ref ws) = self.workspace {
            ws.publish(CognitiveEvent::AgentHandoff {
                from: String::new(),
                to: agent_id.to_string(),
                reason: format!("feedback: reward={reward:.2}, total={}", self.total_feedback),
            });
        }
    }

    /// Get performance stats for an agent.
    pub fn performance(&self, agent_id: &str) -> Option<&AgentPerformance> {
        self.performance.get(agent_id)
    }

    /// Get all registered agent IDs.
    pub fn registered_agents(&self) -> Vec<&str> {
        self.capabilities.keys().map(|s| s.as_str()).collect()
    }

    /// Check if an agent can handle a task based on capability matching.
    fn can_handle(&self, agent_id: &str, features: &AgentFeatures) -> bool {
        let Some(caps) = self.capabilities.get(agent_id) else {
            return false;
        };

        // Complexity range check
        if features.complexity < caps.min_complexity || features.complexity > caps.max_complexity {
            return false;
        }

        // Domain match (at least one domain must match)
        if caps.domains.is_empty() {
            return true; // general-purpose agent
        }

        let has_matching_domain = caps.domains.iter().any(|d| match d.as_str() {
            "coding" => features.is_coding,
            "research" => features.is_research,
            "ops" | "devops" | "infrastructure" => features.is_ops,
            "creative" | "writing" | "design" => features.is_creative,
            "general" => true,
            _ => false,
        });

        has_matching_domain
    }

    /// Lai-Robbins alpha decay: alpha / (1 + 0.1 * sqrt(t)).
    fn effective_alpha(&self) -> f64 {
        self.alpha / (1.0 + 0.1 * (self.total_feedback as f64).sqrt())
    }
}

impl Default for AgentSelector {
    fn default() -> Self {
        Self::new(1.0)
    }
}

// Re-export from conscious crate (this module is part of clawdesk-conscious)
// The AgentSelector is part of the cognitive architecture.

#[cfg(test)]
mod tests {
    use super::*;

    fn make_coding_agent() -> AgentCapabilities {
        AgentCapabilities {
            agent_id: "coder".into(),
            domains: vec!["coding".into()],
            tools_allowed: vec!["shell_exec".into(), "file_write".into()],
            min_complexity: 0.0,
            max_complexity: 1.0,
        }
    }

    fn make_research_agent() -> AgentCapabilities {
        AgentCapabilities {
            agent_id: "researcher".into(),
            domains: vec!["research".into()],
            tools_allowed: vec!["search".into(), "http_fetch".into()],
            min_complexity: 0.0,
            max_complexity: 1.0,
        }
    }

    fn make_general_agent() -> AgentCapabilities {
        AgentCapabilities {
            agent_id: "general".into(),
            domains: vec!["general".into()],
            tools_allowed: vec![],
            min_complexity: 0.0,
            max_complexity: 1.0,
        }
    }

    #[test]
    fn selects_coding_agent_for_coding_task() {
        let mut selector = AgentSelector::new(1.0);
        selector.register_agent(make_coding_agent());
        selector.register_agent(make_research_agent());
        selector.register_agent(make_general_agent());

        // Train: coder succeeds at coding tasks
        let coding_features = AgentFeatures {
            is_coding: true,
            is_research: false,
            is_ops: false,
            is_creative: false,
            complexity: 0.5,
            user_expertise: 0.7,
            urgency: 0.3,
        };

        for _ in 0..20 {
            selector.record_outcome("coder", &coding_features, 0.9);
            selector.record_outcome("general", &coding_features, 0.5);
        }

        let candidates = selector.select(&coding_features);
        assert!(!candidates.is_empty());
        assert_eq!(candidates[0].agent_id, "coder");
    }

    #[test]
    fn new_agent_gets_exploration_bonus() {
        let mut selector = AgentSelector::new(2.0);
        selector.register_agent(make_coding_agent());
        selector.register_agent(make_general_agent());

        let features = AgentFeatures {
            is_coding: true,
            is_research: false,
            is_ops: false,
            is_creative: false,
            complexity: 0.5,
            user_expertise: 0.5,
            urgency: 0.0,
        };

        // Both agents have no data — UCB exploration bonus should be positive
        let candidates = selector.select(&features);
        assert!(candidates.len() >= 2);
        assert!(candidates[0].exploration_bonus > 0.0);
    }

    #[test]
    fn complexity_filter_works() {
        let mut selector = AgentSelector::new(1.0);

        let limited = AgentCapabilities {
            agent_id: "simple".into(),
            domains: vec!["general".into()],
            tools_allowed: vec![],
            min_complexity: 0.0,
            max_complexity: 0.3,
        };
        selector.register_agent(limited);

        let complex_task = AgentFeatures {
            is_coding: false,
            is_research: false,
            is_ops: false,
            is_creative: false,
            complexity: 0.8,
            user_expertise: 0.5,
            urgency: 0.0,
        };

        let candidates = selector.select(&complex_task);
        assert!(candidates.is_empty(), "simple agent should be excluded for complex task");
    }

    #[test]
    fn alpha_decays_with_feedback() {
        let mut selector = AgentSelector::new(1.0);
        let alpha_0 = selector.effective_alpha();

        selector.total_feedback = 100;
        let alpha_100 = selector.effective_alpha();

        assert!(alpha_100 < alpha_0, "alpha should decay with more feedback");
    }

    #[test]
    fn performance_tracking() {
        let mut selector = AgentSelector::new(1.0);
        selector.register_agent(make_coding_agent());

        let features = AgentFeatures {
            is_coding: true,
            is_research: false,
            is_ops: false,
            is_creative: false,
            complexity: 0.5,
            user_expertise: 0.5,
            urgency: 0.0,
        };

        selector.record_outcome("coder", &features, 1.0);
        selector.record_outcome("coder", &features, 0.8);
        selector.record_outcome("coder", &features, 0.2);

        let perf = selector.performance("coder").unwrap();
        assert_eq!(perf.total_tasks, 3);
        assert_eq!(perf.successes, 2); // 1.0 and 0.8 > 0.5
    }
}
