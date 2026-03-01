//! ACP Capability Algebra — lattice-theoretic agent matching.
//!
//! Extends the existing `CapSet` bitfield with proficiency levels,
//! taxonomy-based similarity, and lattice meet/join operations for
//! formal agent-to-task matching.
//!
//! ## Algebra
//!
//! A **graded capability** is `(CapabilityId, proficiency ∈ [0, 1])`.
//! The quality function for request R matched against agent A:
//!
//!     Q(R, A) = Σ_{c ∈ R} w(c) · min(prof_A(c), threshold_R(c)) / max(threshold_R(c), ε)
//!
//! ## Taxonomy similarity
//!
//!     sim(a, b) = depth(LCA(a, b)) / max(depth(a), depth(b))
//!
//! where LCA = lowest common ancestor in the capability hierarchy.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::capability::CapabilityId;

// ─── Graded Capability ─────────────────────────────────────────────────────

/// A capability with an associated proficiency level ∈ [0, 1].
///
/// - 0.0 = no proficiency (effectively absent)
/// - 0.5 = basic competence
/// - 0.8 = strong proficiency
/// - 1.0 = expert level
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GradedCapability {
    pub capability: CapabilityId,
    pub proficiency: f64,
}

impl GradedCapability {
    pub fn new(capability: CapabilityId, proficiency: f64) -> Self {
        Self {
            capability,
            proficiency: proficiency.clamp(0.0, 1.0),
        }
    }

    pub fn expert(capability: CapabilityId) -> Self {
        Self::new(capability, 1.0)
    }

    pub fn basic(capability: CapabilityId) -> Self {
        Self::new(capability, 0.5)
    }
}

// ─── Graded Capability Set ──────────────────────────────────────────────────

/// A set of capabilities with proficiency levels.
///
/// Unlike `CapSet` (Boolean membership), this tracks how *well* an agent
/// can perform each capability.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GradedCapSet {
    levels: HashMap<CapabilityId, f64>,
}

impl GradedCapSet {
    pub fn new() -> Self {
        Self {
            levels: HashMap::new(),
        }
    }

    /// Insert or update a capability's proficiency.
    pub fn insert(&mut self, cap: CapabilityId, proficiency: f64) {
        self.levels.insert(cap, proficiency.clamp(0.0, 1.0));
    }

    /// Get proficiency for a capability (0.0 if absent).
    pub fn proficiency(&self, cap: CapabilityId) -> f64 {
        self.levels.get(&cap).copied().unwrap_or(0.0)
    }

    /// Whether capability is present (proficiency > 0).
    pub fn contains(&self, cap: CapabilityId) -> bool {
        self.proficiency(cap) > 0.0
    }

    /// Number of capabilities with non-zero proficiency.
    pub fn count(&self) -> usize {
        self.levels.values().filter(|&&p| p > 0.0).count()
    }

    /// All capabilities with their proficiency levels.
    pub fn iter(&self) -> impl Iterator<Item = (&CapabilityId, &f64)> {
        self.levels.iter()
    }

    /// Lattice meet (intersection): min proficiency for each capability.
    pub fn meet(&self, other: &Self) -> Self {
        let mut result = Self::new();
        for (&cap, &prof) in &self.levels {
            let other_prof = other.proficiency(cap);
            let min_prof = prof.min(other_prof);
            if min_prof > 0.0 {
                result.insert(cap, min_prof);
            }
        }
        result
    }

    /// Lattice join (union): max proficiency for each capability.
    pub fn join(&self, other: &Self) -> Self {
        let mut result = self.clone();
        for (&cap, &prof) in &other.levels {
            let current = result.proficiency(cap);
            if prof > current {
                result.insert(cap, prof);
            }
        }
        result
    }

    /// Build from a list of graded capabilities.
    pub fn from_graded(caps: &[GradedCapability]) -> Self {
        let mut set = Self::new();
        for gc in caps {
            set.insert(gc.capability, gc.proficiency);
        }
        set
    }
}

// ─── Capability Requirement ─────────────────────────────────────────────────

/// A requirement for a specific capability with a minimum threshold.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityRequirement {
    pub capability: CapabilityId,
    /// Minimum acceptable proficiency ∈ (0, 1].
    pub threshold: f64,
    /// Importance weight for scoring.
    pub weight: f64,
}

impl CapabilityRequirement {
    pub fn new(capability: CapabilityId, threshold: f64, weight: f64) -> Self {
        Self {
            capability,
            threshold: threshold.clamp(0.01, 1.0),
            weight: weight.max(0.0),
        }
    }

    /// Required capability with default threshold (0.5) and weight (1.0).
    pub fn required(capability: CapabilityId) -> Self {
        Self::new(capability, 0.5, 1.0)
    }

    /// High-priority requirement.
    pub fn critical(capability: CapabilityId) -> Self {
        Self::new(capability, 0.8, 2.0)
    }
}

// ─── Quality Function ───────────────────────────────────────────────────────

/// Compute the quality score Q(R, A) for matching requirements R against agent A.
///
///     Q(R, A) = Σ_{c ∈ R} w(c) · min(prof_A(c), threshold_R(c)) / threshold_R(c)
///             / Σ_{c ∈ R} w(c)
///
/// Returns ∈ [0, 1] where 1 = full match at or above all thresholds.
pub fn quality_score(requirements: &[CapabilityRequirement], agent: &GradedCapSet) -> f64 {
    if requirements.is_empty() {
        return 1.0;
    }

    let mut weighted_sum = 0.0;
    let mut total_weight = 0.0;

    for req in requirements {
        let prof = agent.proficiency(req.capability);
        let satisfaction = (prof.min(req.threshold)) / req.threshold;
        weighted_sum += req.weight * satisfaction;
        total_weight += req.weight;
    }

    if total_weight < 1e-10 {
        return 1.0;
    }

    weighted_sum / total_weight
}

/// Whether an agent satisfies all hard requirements (proficiency ≥ threshold).
pub fn satisfies_all(requirements: &[CapabilityRequirement], agent: &GradedCapSet) -> bool {
    requirements
        .iter()
        .all(|req| agent.proficiency(req.capability) >= req.threshold)
}

// ─── Taxonomy Similarity ────────────────────────────────────────────────────

/// Compute taxonomy depth for a capability (distance from root in hierarchy).
fn taxonomy_depth(cap: CapabilityId) -> usize {
    let mut depth = 0;
    let mut current = cap.parent();
    while let Some(parent) = current {
        depth += 1;
        current = parent.parent();
    }
    depth
}

/// Find the lowest common ancestor (LCA) of two capabilities.
///
/// Returns `None` if the capabilities are in disjoint trees.
fn lowest_common_ancestor(a: CapabilityId, b: CapabilityId) -> Option<CapabilityId> {
    // Collect ancestor chain for `a`.
    let mut ancestors_a = Vec::new();
    ancestors_a.push(a);
    let mut current = a.parent();
    while let Some(parent) = current {
        ancestors_a.push(parent);
        current = parent.parent();
    }

    // Walk up from `b` and check intersection.
    let mut current_b = Some(b);
    while let Some(cap) = current_b {
        if ancestors_a.contains(&cap) {
            return Some(cap);
        }
        current_b = cap.parent();
    }

    // Check if `a` itself is an ancestor of `b` or vice versa.
    if ancestors_a.contains(&b) {
        return Some(b);
    }

    None
}

/// Taxonomy-based similarity between two capabilities ∈ [0, 1].
///
///     sim(a, b) = (1 + depth(LCA(a, b))) / (1 + max(depth(a), depth(b)))
///
/// Returns 1.0 when a == b, decreases toward 0 for distant capabilities.
/// Returns 0.0 if capabilities are in completely disjoint trees.
pub fn taxonomy_similarity(a: CapabilityId, b: CapabilityId) -> f64 {
    if a == b {
        return 1.0;
    }

    let lca = match lowest_common_ancestor(a, b) {
        Some(lca) => lca,
        None => return 0.0,
    };

    let lca_depth = taxonomy_depth(lca);
    let max_depth = taxonomy_depth(a).max(taxonomy_depth(b));

    if max_depth == 0 {
        return 0.0;
    }

    (1.0 + lca_depth as f64) / (1.0 + max_depth as f64)
}

// ─── Agent Ranking ──────────────────────────────────────────────────────────

/// Ranked agent result from capability matching.
#[derive(Debug, Clone)]
pub struct RankedAgent {
    pub agent_id: String,
    pub quality_score: f64,
    pub satisfies_all: bool,
}

/// Rank agents by quality score against requirements.
///
/// Returns agents sorted by quality_score descending.
pub fn rank_agents(
    requirements: &[CapabilityRequirement],
    agents: &[(String, GradedCapSet)],
) -> Vec<RankedAgent> {
    let mut ranked: Vec<RankedAgent> = agents
        .iter()
        .map(|(id, caps)| RankedAgent {
            agent_id: id.clone(),
            quality_score: quality_score(requirements, caps),
            satisfies_all: satisfies_all(requirements, caps),
        })
        .collect();

    // Sort descending by: satisfies_all first, then quality_score.
    ranked.sort_by(|a, b| {
        b.satisfies_all
            .cmp(&a.satisfies_all)
            .then(b.quality_score.partial_cmp(&a.quality_score).unwrap_or(std::cmp::Ordering::Equal))
    });

    ranked
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_graded_capset_operations() {
        let mut a = GradedCapSet::new();
        a.insert(CapabilityId::TextGeneration, 0.9);
        a.insert(CapabilityId::WebSearch, 0.7);

        let mut b = GradedCapSet::new();
        b.insert(CapabilityId::TextGeneration, 0.6);
        b.insert(CapabilityId::CodeExecution, 0.8);

        // Meet: min of shared
        let meet = a.meet(&b);
        assert!((meet.proficiency(CapabilityId::TextGeneration) - 0.6).abs() < 1e-10);
        assert_eq!(meet.proficiency(CapabilityId::WebSearch), 0.0); // b doesn't have it
        assert_eq!(meet.proficiency(CapabilityId::CodeExecution), 0.0); // a doesn't have it

        // Join: max of all
        let join = a.join(&b);
        assert!((join.proficiency(CapabilityId::TextGeneration) - 0.9).abs() < 1e-10);
        assert!((join.proficiency(CapabilityId::WebSearch) - 0.7).abs() < 1e-10);
        assert!((join.proficiency(CapabilityId::CodeExecution) - 0.8).abs() < 1e-10);
    }

    #[test]
    fn test_quality_score_full_match() {
        let mut agent = GradedCapSet::new();
        agent.insert(CapabilityId::TextGeneration, 0.9);
        agent.insert(CapabilityId::WebSearch, 0.8);

        let reqs = vec![
            CapabilityRequirement::new(CapabilityId::TextGeneration, 0.5, 1.0),
            CapabilityRequirement::new(CapabilityId::WebSearch, 0.5, 1.0),
        ];

        let score = quality_score(&reqs, &agent);
        assert!((score - 1.0).abs() < 1e-10, "score = {}", score);
    }

    #[test]
    fn test_quality_score_partial_match() {
        let mut agent = GradedCapSet::new();
        agent.insert(CapabilityId::TextGeneration, 0.3); // below threshold

        let reqs = vec![
            CapabilityRequirement::new(CapabilityId::TextGeneration, 0.5, 1.0),
        ];

        let score = quality_score(&reqs, &agent);
        assert!((score - 0.6).abs() < 1e-10, "score = {}", score); // 0.3/0.5 = 0.6
    }

    #[test]
    fn test_quality_score_missing_capability() {
        let agent = GradedCapSet::new(); // empty
        let reqs = vec![CapabilityRequirement::required(CapabilityId::TextGeneration)];
        let score = quality_score(&reqs, &agent);
        assert!((score - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_satisfies_all() {
        let mut agent = GradedCapSet::new();
        agent.insert(CapabilityId::TextGeneration, 0.9);
        agent.insert(CapabilityId::WebSearch, 0.8);

        let reqs = vec![
            CapabilityRequirement::new(CapabilityId::TextGeneration, 0.5, 1.0),
            CapabilityRequirement::new(CapabilityId::WebSearch, 0.5, 1.0),
        ];
        assert!(satisfies_all(&reqs, &agent));

        // Raise threshold above agent level
        let hard_reqs = vec![
            CapabilityRequirement::new(CapabilityId::WebSearch, 0.95, 1.0),
        ];
        assert!(!satisfies_all(&hard_reqs, &agent));
    }

    #[test]
    fn test_taxonomy_similarity_same() {
        let sim = taxonomy_similarity(CapabilityId::TextGeneration, CapabilityId::TextGeneration);
        assert!((sim - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_taxonomy_similarity_parent_child() {
        // AudioProcessing → MediaProcessing (parent)
        let sim = taxonomy_similarity(
            CapabilityId::AudioProcessing,
            CapabilityId::ImageProcessing,
        );
        // Both at depth 1 under MediaProcessing. LCA = MediaProcessing (depth 0)
        // sim = (1+0) / (1+1) = 0.5
        assert!((sim - 0.5).abs() < 1e-10, "sim = {}", sim);
    }

    #[test]
    fn test_taxonomy_similarity_disjoint() {
        // TextGeneration and Mathematics are both roots — no common ancestor
        let sim = taxonomy_similarity(CapabilityId::TextGeneration, CapabilityId::Mathematics);
        assert!((sim - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_rank_agents() {
        let reqs = vec![
            CapabilityRequirement::required(CapabilityId::TextGeneration),
            CapabilityRequirement::required(CapabilityId::WebSearch),
        ];

        let mut a1 = GradedCapSet::new();
        a1.insert(CapabilityId::TextGeneration, 0.9);
        a1.insert(CapabilityId::WebSearch, 0.8);

        let mut a2 = GradedCapSet::new();
        a2.insert(CapabilityId::TextGeneration, 0.6);
        // a2 missing WebSearch

        let agents = vec![
            ("agent-1".into(), a1),
            ("agent-2".into(), a2),
        ];

        let ranked = rank_agents(&reqs, &agents);
        assert_eq!(ranked[0].agent_id, "agent-1");
        assert!(ranked[0].satisfies_all);
        assert!(!ranked[1].satisfies_all);
    }
}
