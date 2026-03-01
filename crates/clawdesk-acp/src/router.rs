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

use crate::agent_card::AgentCard;
use crate::capability::CapabilityId;
use rustc_hash::FxHashMap;
use tracing::{debug, info};

/// Agent directory — registry of known agents and their cards.
///
/// Maintains an inverted index `CapabilityId → Vec<AgentId>` for O(C × avg)
/// routing instead of O(A × C) full scan. The index is rebuilt on
/// register/deregister and includes hierarchical closure (an agent with
/// `AudioProcessing` also appears under `MediaProcessing`).
#[derive(Clone)]
pub struct AgentDirectory {
    pub(crate) agents: FxHashMap<String, AgentEntry>,
    /// Inverted index: capability → set of agent IDs that have it (after closure).
    cap_index: FxHashMap<CapabilityId, Vec<String>>,
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
    /// Social/reputation metrics from user feedback (reactions, ratings).
    pub social_metrics: SocialMetrics,
}

/// Bayesian reputation tracker using Beta-distribution parameters.
///
/// Models user satisfaction as a Beta(α, β) distribution where:
/// - α (positive) is incremented for positive feedback (👍, ✅, ⭐)
/// - β (negative) is incremented for negative feedback (👎, ❌)
///
/// The expected reputation is `E[X] = α / (α + β)`, yielding a value in
/// `[0, 1]` that converges to the true satisfaction rate with more data.
///
/// Uses Thompson Sampling–compatible priors: `α₀ = β₀ = 1` (uniform prior).
#[derive(Debug, Clone)]
pub struct SocialMetrics {
    /// Positive feedback count (Beta distribution α parameter).
    pub positive: f64,
    /// Negative feedback count (Beta distribution β parameter).
    pub negative: f64,
    /// Total number of interactions (for decay / recency gating).
    pub total_interactions: u64,
}

impl SocialMetrics {
    /// Uniform prior: no evidence yet.
    pub fn new() -> Self {
        Self {
            positive: 1.0,
            negative: 1.0,
            total_interactions: 0,
        }
    }

    /// Record a positive feedback signal (e.g., thumbs-up reaction).
    pub fn record_positive(&mut self) {
        self.positive += 1.0;
        self.total_interactions += 1;
    }

    /// Record a negative feedback signal (e.g., thumbs-down reaction).
    pub fn record_negative(&mut self) {
        self.negative += 1.0;
        self.total_interactions += 1;
    }

    /// Expected reputation: E[Beta(α, β)] = α / (α + β).
    /// Returns a value in [0, 1]. With the uniform prior (1,1), a fresh
    /// agent starts at 0.5.
    pub fn reputation(&self) -> f64 {
        self.positive / (self.positive + self.negative)
    }

    /// Reputation boost factor for router scoring.
    ///
    /// Maps reputation [0,1] to a multiplicative factor in [0.8, 1.2]:
    /// - reputation 0.5 (neutral) → factor 1.0 (no effect)
    /// - reputation 1.0 (perfect) → factor 1.2 (+20%)
    /// - reputation 0.0 (worst)   → factor 0.8 (−20%)
    ///
    /// The bounded range prevents reputation from dominating capability score.
    pub fn routing_boost(&self) -> f64 {
        0.8 + 0.4 * self.reputation()
    }
}

impl Default for SocialMetrics {
    fn default() -> Self {
        Self::new()
    }
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
        required_capabilities: Vec<CapabilityId>,
    },
}

impl AgentDirectory {
    pub fn new() -> Self {
        Self {
            agents: FxHashMap::default(),
            cap_index: FxHashMap::default(),
        }
    }

    /// Register or update an agent card.
    pub fn register(&mut self, mut card: AgentCard) {
        let id = card.id.clone();
        // Ensure capset with closure is built.
        card.rebuild_capset();
        info!(agent = %id, name = %card.name, caps = card.capabilities.len(), "registered agent");

        // Remove old index entries if re-registering.
        self.remove_from_index(&id);

        // Add to inverted index: every capability in the closed set.
        for &cap in CapabilityId::all() {
            if card.has_capability(cap) {
                self.cap_index
                    .entry(cap)
                    .or_insert_with(Vec::new)
                    .push(id.clone());
            }
        }

        self.agents.insert(
            id,
            AgentEntry {
                card,
                active_tasks: 0,
                last_latency_ms: None,
                is_healthy: true,
                last_health_check: None,
                social_metrics: SocialMetrics::new(),
            },
        );
    }

    /// Remove an agent from the directory.
    pub fn deregister(&mut self, agent_id: &str) -> Option<AgentEntry> {
        self.remove_from_index(agent_id);
        self.agents.remove(agent_id)
    }

    /// Remove agent from all inverted index posting lists.
    fn remove_from_index(&mut self, agent_id: &str) {
        for posting_list in self.cap_index.values_mut() {
            posting_list.retain(|id| id != agent_id);
        }
    }

    /// Get candidate agent IDs for a set of required capabilities.
    ///
    /// Returns the *union* of posting lists for all required capabilities.
    /// O(C × avg_posting_list_length) instead of O(A).
    pub fn candidates_for(&self, required: &[CapabilityId]) -> Vec<&str> {
        use std::collections::HashSet;
        let mut seen = HashSet::new();
        for &cap in required {
            if let Some(list) = self.cap_index.get(&cap) {
                for id in list {
                    seen.insert(id.as_str());
                }
            }
            // Also look up the closed required set — if the requirement is
            // MediaProcessing, agents indexed under MediaProcessing are candidates.
            // If requirement is AudioProcessing, the closure adds MediaProcessing
            // but we already indexed agents under their closure, so the union covers it.
        }
        seen.into_iter().collect()
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

    /// Record a user reaction (feedback signal) for an agent.
    ///
    /// Positive reaction emojis (👍, ✅, ⭐, ❤️, etc.) increment the positive
    /// Beta parameter. Negative reactions (👎, ❌) increment the negative
    /// parameter. Neutral/unknown reactions are ignored.
    pub fn record_reaction(&mut self, agent_id: &str, positive: bool) {
        if let Some(entry) = self.agents.get_mut(agent_id) {
            if positive {
                entry.social_metrics.record_positive();
            } else {
                entry.social_metrics.record_negative();
            }
            debug!(
                agent = agent_id,
                positive = positive,
                reputation = format!("{:.3}", entry.social_metrics.reputation()),
                "recorded social feedback"
            );
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
    /// 1. Use inverted index to get candidate agents (O(C × avg)).
    /// 2. Filter to healthy agents with score ≥ threshold.
    /// 3. Among eligible agents, select by: highest score → lowest load → lowest latency.
    ///
    /// Complexity: O(C × avg + K log K) where K = |candidates| ≪ |agents|.
    pub fn route(
        &self,
        directory: &AgentDirectory,
        required_capabilities: &[CapabilityId],
        exclude_agents: &[String],
    ) -> RoutingDecision {
        // Use inverted index for candidate pre-filtering.
        let candidate_ids = directory.candidates_for(required_capabilities);

        let mut candidates: Vec<(&str, f64, u32, u64)> = candidate_ids
            .iter()
            .filter_map(|&id| directory.agents.get(id).map(|entry| (id, entry)))
            .filter(|(id, entry)| {
                entry.is_healthy
                    && !exclude_agents.iter().any(|ex| ex == *id)
                    && entry
                        .card
                        .max_concurrent_tasks
                        .map_or(true, |max| entry.active_tasks < max)
            })
            .map(|(id, entry)| {
                let cap_score = entry.card.capability_score(required_capabilities);
                // Modulate capability score by reputation [0.8x .. 1.2x].
                let score = cap_score * entry.social_metrics.routing_boost();
                let load = entry.active_tasks;
                let latency = entry.last_latency_ms.unwrap_or(u64::MAX);
                (id, score, load, latency)
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
        required_capabilities: &[CapabilityId],
    ) -> Vec<(String, f64)> {
        let candidate_ids = directory.candidates_for(required_capabilities);

        candidate_ids
            .iter()
            .filter_map(|&id| directory.agents.get(id).map(|entry| (id, entry)))
            .filter(|(_, entry)| entry.is_healthy)
            .map(|(id, entry)| {
                let score = entry.card.capability_score(required_capabilities);
                (id.to_string(), score)
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

    fn make_agent(id: &str, caps: Vec<CapabilityId>) -> AgentCard {
        let mut card = AgentCard::new(id, id, format!("http://{}.local", id));
        card.capabilities = caps;
        card.rebuild_capset();
        card
    }

    #[test]
    fn routes_to_best_match() {
        let mut dir = AgentDirectory::new();
        dir.register(make_agent(
            "web-agent",
            vec![CapabilityId::WebSearch, CapabilityId::TextGeneration],
        ));
        dir.register(make_agent(
            "code-agent",
            vec![
                CapabilityId::CodeExecution,
                CapabilityId::TextGeneration,
            ],
        ));

        let router = AgentRouter::new();
        let decision = router.route(
            &dir,
            &[CapabilityId::WebSearch, CapabilityId::TextGeneration],
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
            vec![CapabilityId::TextGeneration],
        ));
        dir.register(make_agent(
            "idle",
            vec![CapabilityId::TextGeneration],
        ));
        dir.increment_tasks("busy");
        dir.increment_tasks("busy");
        dir.increment_tasks("busy");

        let router = AgentRouter::new();
        let decision = router.route(&dir, &[CapabilityId::TextGeneration], &[]);

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
            vec![CapabilityId::TextGeneration],
        ));
        dir.update_health("down", false, None);

        let router = AgentRouter::new();
        let decision = router.route(&dir, &[CapabilityId::TextGeneration], &[]);

        assert!(matches!(decision, RoutingDecision::NoMatch { .. }));
    }

    #[test]
    fn social_metrics_default_reputation() {
        let m = SocialMetrics::new();
        // Uniform prior: 1/(1+1) = 0.5
        assert!((m.reputation() - 0.5).abs() < 1e-10);
        // Neutral boost factor: 0.8 + 0.4 * 0.5 = 1.0
        assert!((m.routing_boost() - 1.0).abs() < 1e-10);
    }

    #[test]
    fn social_metrics_positive_feedback() {
        let mut m = SocialMetrics::new();
        for _ in 0..8 {
            m.record_positive();
        }
        // α=9, β=1 → reputation 0.9
        assert!((m.reputation() - 0.9).abs() < 1e-10);
        assert!(m.routing_boost() > 1.1);
        assert_eq!(m.total_interactions, 8);
    }

    #[test]
    fn social_metrics_affect_routing() {
        let mut dir = AgentDirectory::new();
        dir.register(make_agent(
            "loved",
            vec![CapabilityId::TextGeneration],
        ));
        dir.register(make_agent(
            "disliked",
            vec![CapabilityId::TextGeneration],
        ));

        // Give "loved" positive feedback, "disliked" negative feedback
        for _ in 0..10 {
            dir.record_reaction("loved", true);
            dir.record_reaction("disliked", false);
        }

        let router = AgentRouter::new();
        let decision = router.route(&dir, &[CapabilityId::TextGeneration], &[]);

        match decision {
            RoutingDecision::Route { agent_id, .. } => {
                assert_eq!(agent_id, "loved", "agent with better reputation should be preferred");
            }
            RoutingDecision::NoMatch { .. } => panic!("expected a match"),
        }
    }
}
