//! Agent Router — capability-based discovery and routing.
//!
//! ## Algorithm
//!
//! Given a task with required capabilities C_req, find the best agent:
//!
//! ```text
//! score(agent) = Σ_{c ∈ C_req} w(c) · 𝟙[c ∈ caps(agent)] / |C_req|
//! ```
//!
//! Agents with score < threshold are excluded. Among eligible agents,
//! the one with highest score wins. Ties are broken by:
//! 1. Lowest current task count (load balancing)
//! 2. Lowest latency (if known)
//! 3. Random (to avoid hotspotting)
//!
//! ## Complexity
//!
//! Directory lookup: O(1) by agent ID.
//! Routing (capability matching): O(A × C) where A = |agents|, C = |capabilities|.
//! For typical instances (A < 50, C < 20), this is sub-microsecond.

use crate::agent_card::{AgentCapability, AgentCard};
use rustc_hash::FxHashMap;
use tracing::{debug, info};

/// Agent directory — registry of known agents and their cards.
#[derive(Clone)]
pub struct AgentDirectory {
    pub(crate) agents: FxHashMap<String, AgentEntry>,
}

/// Entry in the agent directory.
#[derive(Debug, Clone)]
pub struct AgentEntry {
    pub card: AgentCard,
    /// Number of currently active tasks delegated to this agent.
    pub active_tasks: u32,
    /// Last known latency in milliseconds.
    pub last_latency_ms: Option<u64>,
    /// Whether this agent is currently reachable.
    pub is_healthy: bool,
    /// Last health check timestamp.
    pub last_health_check: Option<chrono::DateTime<chrono::Utc>>,
}

/// Routing decision result.
#[derive(Debug, Clone)]
pub enum RoutingDecision {
    /// Route to this agent.
    Route {
        agent_id: String,
        score: f64,
        reason: String,
    },
    /// No suitable agent found.
    NoMatch {
        reason: String,
        required_capabilities: Vec<AgentCapability>,
    },
}

impl AgentDirectory {
    pub fn new() -> Self {
        Self {
            agents: FxHashMap::default(),
        }
    }

    /// Register or update an agent card.
    pub fn register(&mut self, card: AgentCard) {
        let id = card.id.clone();
        info!(agent = %id, name = %card.name, caps = card.capabilities.len(), "registered agent");
        self.agents.insert(
            id,
            AgentEntry {
                card,
                active_tasks: 0,
                last_latency_ms: None,
                is_healthy: true,
                last_health_check: None,
            },
        );
    }

    /// Remove an agent from the directory.
    pub fn deregister(&mut self, agent_id: &str) -> Option<AgentEntry> {
        self.agents.remove(agent_id)
    }

    /// Get an agent card by ID.
    pub fn get(&self, agent_id: &str) -> Option<&AgentEntry> {
        self.agents.get(agent_id)
    }

    /// List all registered agents.
    pub fn list(&self) -> Vec<&AgentCard> {
        self.agents.values().map(|e| &e.card).collect()
    }

    /// Record that a task was assigned to an agent.
    pub fn increment_tasks(&mut self, agent_id: &str) {
        if let Some(entry) = self.agents.get_mut(agent_id) {
            entry.active_tasks += 1;
        }
    }

    /// Record that a task on an agent completed.
    pub fn decrement_tasks(&mut self, agent_id: &str) {
        if let Some(entry) = self.agents.get_mut(agent_id) {
            entry.active_tasks = entry.active_tasks.saturating_sub(1);
        }
    }

    /// Update health status for an agent.
    pub fn update_health(&mut self, agent_id: &str, healthy: bool, latency_ms: Option<u64>) {
        if let Some(entry) = self.agents.get_mut(agent_id) {
            entry.is_healthy = healthy;
            entry.last_latency_ms = latency_ms;
            entry.last_health_check = Some(chrono::Utc::now());
        }
    }
}

impl Default for AgentDirectory {
    fn default() -> Self {
        Self::new()
    }
}

/// Agent router — finds the best agent for a given task.
pub struct AgentRouter {
    /// Minimum capability score to consider an agent eligible.
    pub min_score_threshold: f64,
}

impl AgentRouter {
    pub fn new() -> Self {
        Self {
            min_score_threshold: 0.5,
        }
    }

    /// Find the best agent for a set of required capabilities.
    ///
    /// Algorithm:
    /// 1. Filter to healthy agents with score ≥ threshold.
    /// 2. Among eligible agents, select by: highest score → lowest load → lowest latency.
    ///
    /// Complexity: O(A × C) where A = |agents|, C = |required_capabilities|.
    pub fn route(
        &self,
        directory: &AgentDirectory,
        required_capabilities: &[AgentCapability],
        exclude_agents: &[String],
    ) -> RoutingDecision {
        let mut candidates: Vec<(&str, f64, u32, u64)> = directory
            .agents
            .iter()
            .filter(|(id, entry)| {
                entry.is_healthy
                    && !exclude_agents.contains(id)
                    && entry
                        .card
                        .max_concurrent_tasks
                        .map_or(true, |max| entry.active_tasks < max)
            })
            .map(|(id, entry)| {
                let score = entry.card.capability_score(required_capabilities);
                let load = entry.active_tasks;
                let latency = entry.last_latency_ms.unwrap_or(u64::MAX);
                (id.as_str(), score, load, latency)
            })
            .filter(|(_, score, _, _)| *score >= self.min_score_threshold)
            .collect();

        if candidates.is_empty() {
            return RoutingDecision::NoMatch {
                reason: "no healthy agent with required capabilities found".into(),
                required_capabilities: required_capabilities.to_vec(),
            };
        }

        // Sort: highest score → lowest load → lowest latency.
        candidates.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.2.cmp(&b.2))
                .then(a.3.cmp(&b.3))
        });

        let (agent_id, score, load, _latency) = candidates[0];

        debug!(
            agent = agent_id,
            score = score,
            load = load,
            candidates = candidates.len(),
            "routed task to agent"
        );

        RoutingDecision::Route {
            agent_id: agent_id.to_string(),
            score,
            reason: format!(
                "best match (score={:.2}, load={}, {} candidates)",
                score,
                load,
                candidates.len()
            ),
        }
    }

    /// Find all agents matching a set of capabilities (for broadcast/parallel execution).
    pub fn find_all(
        &self,
        directory: &AgentDirectory,
        required_capabilities: &[AgentCapability],
    ) -> Vec<(String, f64)> {
        directory
            .agents
            .iter()
            .filter(|(_, entry)| entry.is_healthy)
            .map(|(id, entry)| {
                let score = entry.card.capability_score(required_capabilities);
                (id.clone(), score)
            })
            .filter(|(_, score)| *score >= self.min_score_threshold)
            .collect()
    }
}

impl Default for AgentRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_agent(id: &str, caps: Vec<AgentCapability>) -> AgentCard {
        let mut card = AgentCard::new(id, id, format!("http://{}.local", id));
        card.capabilities = caps;
        card
    }

    #[test]
    fn routes_to_best_match() {
        let mut dir = AgentDirectory::new();
        dir.register(make_agent(
            "web-agent",
            vec![AgentCapability::WebSearch, AgentCapability::TextGeneration],
        ));
        dir.register(make_agent(
            "code-agent",
            vec![
                AgentCapability::CodeExecution,
                AgentCapability::TextGeneration,
            ],
        ));

        let router = AgentRouter::new();
        let decision = router.route(
            &dir,
            &[AgentCapability::WebSearch, AgentCapability::TextGeneration],
            &[],
        );

        match decision {
            RoutingDecision::Route { agent_id, score, .. } => {
                assert_eq!(agent_id, "web-agent");
                assert_eq!(score, 1.0);
            }
            RoutingDecision::NoMatch { .. } => panic!("expected a match"),
        }
    }

    #[test]
    fn prefers_lower_load() {
        let mut dir = AgentDirectory::new();
        dir.register(make_agent(
            "busy",
            vec![AgentCapability::TextGeneration],
        ));
        dir.register(make_agent(
            "idle",
            vec![AgentCapability::TextGeneration],
        ));
        dir.increment_tasks("busy");
        dir.increment_tasks("busy");
        dir.increment_tasks("busy");

        let router = AgentRouter::new();
        let decision = router.route(&dir, &[AgentCapability::TextGeneration], &[]);

        match decision {
            RoutingDecision::Route { agent_id, .. } => {
                assert_eq!(agent_id, "idle");
            }
            RoutingDecision::NoMatch { .. } => panic!("expected a match"),
        }
    }

    #[test]
    fn excludes_unhealthy() {
        let mut dir = AgentDirectory::new();
        dir.register(make_agent(
            "down",
            vec![AgentCapability::TextGeneration],
        ));
        dir.update_health("down", false, None);

        let router = AgentRouter::new();
        let decision = router.route(&dir, &[AgentCapability::TextGeneration], &[]);

        assert!(matches!(decision, RoutingDecision::NoMatch { .. }));
    }
}
