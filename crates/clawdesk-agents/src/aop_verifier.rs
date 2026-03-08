//! Agent-Oriented Planning: Solvability, Completeness, and Non-Redundancy verification.
//!
//! Implements the three AOP predicates from ICLR 2025:
//!
//! 1. **Solvability** — every subtask must be within the capability envelope
//!    of at least one available agent.
//! 2. **Completeness** — the union of all subtask solutions must fully resolve
//!    the original query.
//! 3. **Non-redundancy** — no two subtasks should duplicate work (Jaccard < θ_r).
//!
//! The verification layer runs after task decomposition and before execution,
//! preventing wasted LLM calls on impossible subtasks.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, warn};

// ───────────────────────────────────────────────────────────────
// Core types
// ───────────────────────────────────────────────────────────────

/// A decomposed subtask.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subtask {
    pub id: String,
    pub description: String,
    /// Semantic embedding of this subtask (for coverage / redundancy checks).
    pub embedding: Vec<f32>,
    /// Required capability IDs for this subtask.
    pub required_capabilities: Vec<String>,
}

/// An available agent with its capabilities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCapabilityProfile {
    pub agent_id: String,
    /// Capability IDs this agent can handle.
    pub capabilities: Vec<String>,
    /// Per-capability confidence scores ∈ [0, 1].
    pub capability_scores: HashMap<String, f64>,
}

/// Result of the three-predicate verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationResult {
    /// Whether all three predicates pass.
    pub valid: bool,
    /// Solvability check result.
    pub solvability: SolvabilityResult,
    /// Completeness check result.
    pub completeness: CompletenessResult,
    /// Non-redundancy check result.
    pub non_redundancy: NonRedundancyResult,
    /// Overall decomposition quality score ∈ [0, 1].
    pub quality_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolvabilityResult {
    pub passed: bool,
    /// Subtasks for which no capable agent exists.
    pub unsolvable_subtasks: Vec<String>,
    /// Best agent assignment per subtask: subtask_id → (agent_id, score).
    pub assignments: HashMap<String, (String, f64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletenessResult {
    pub passed: bool,
    /// Estimated semantic coverage of the original query ∈ [0, 1].
    pub coverage: f64,
    /// Aspects of the query not covered by any subtask.
    pub uncovered_aspects: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NonRedundancyResult {
    pub passed: bool,
    /// Pairs of subtasks with excessive overlap.
    pub redundant_pairs: Vec<(String, String, f64)>,
}

/// Configuration for the verification predicates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationConfig {
    /// Minimum capability score for solvability (θ_s).
    pub solvability_threshold: f64,
    /// Minimum coverage for completeness.
    pub completeness_threshold: f64,
    /// Maximum Jaccard overlap for non-redundancy (θ_r).
    pub redundancy_threshold: f64,
    /// Minimum overall quality to accept decomposition (θ_q).
    pub quality_threshold: f64,
    /// Maximum decomposition-verify-replan iterations.
    pub max_iterations: u32,
}

impl Default for VerificationConfig {
    fn default() -> Self {
        Self {
            solvability_threshold: 0.3,
            completeness_threshold: 0.7,
            redundancy_threshold: 0.6,
            quality_threshold: 0.5,
            max_iterations: 3,
        }
    }
}

// ───────────────────────────────────────────────────────────────
// Verifier
// ───────────────────────────────────────────────────────────────

/// Agent-Oriented Planning verifier.
pub struct AopVerifier {
    config: VerificationConfig,
}

impl AopVerifier {
    pub fn new(config: VerificationConfig) -> Self {
        Self { config }
    }

    /// Run all three verification predicates on a task decomposition.
    ///
    /// - `subtasks` — the decomposed subtasks.
    /// - `agents` — available agents with capability profiles.
    /// - `query_embedding` — embedding of the original query for coverage check.
    pub fn verify(
        &self,
        subtasks: &[Subtask],
        agents: &[AgentCapabilityProfile],
        query_embedding: &[f32],
    ) -> VerificationResult {
        let solvability = self.check_solvability(subtasks, agents);
        let completeness = self.check_completeness(subtasks, query_embedding);
        let non_redundancy = self.check_non_redundancy(subtasks);

        // R(D, Q) = w_s · solvability_frac + w_c · coverage + w_r · (1 - redundancy_frac)
        let solvable_frac = if subtasks.is_empty() {
            1.0
        } else {
            1.0 - (solvability.unsolvable_subtasks.len() as f64 / subtasks.len() as f64)
        };
        let redundancy_frac = if subtasks.len() < 2 {
            0.0
        } else {
            let total_pairs = subtasks.len() * (subtasks.len() - 1) / 2;
            non_redundancy.redundant_pairs.len() as f64 / total_pairs.max(1) as f64
        };

        let quality_score =
            0.4 * solvable_frac + 0.4 * completeness.coverage + 0.2 * (1.0 - redundancy_frac);

        let valid = solvability.passed
            && completeness.passed
            && non_redundancy.passed
            && quality_score >= self.config.quality_threshold;

        debug!(
            solvable = solvability.passed,
            complete = completeness.passed,
            non_redundant = non_redundancy.passed,
            quality = quality_score,
            "AOP verification"
        );

        VerificationResult {
            valid,
            solvability,
            completeness,
            non_redundancy,
            quality_score,
        }
    }

    /// Predicate 1: Solvability — ∀ s_i ∈ D, ∃ a_j ∈ A : cap(a_j, s_i) > θ_s.
    /// Complexity: O(k · |A|).
    fn check_solvability(
        &self,
        subtasks: &[Subtask],
        agents: &[AgentCapabilityProfile],
    ) -> SolvabilityResult {
        let mut unsolvable = Vec::new();
        let mut assignments = HashMap::new();

        for st in subtasks {
            let mut best_agent: Option<(&str, f64)> = None;

            for agent in agents {
                // Compute capability match score.
                let score = Self::capability_score(st, agent);
                if let Some((_, best_score)) = best_agent {
                    if score > best_score {
                        best_agent = Some((&agent.agent_id, score));
                    }
                } else {
                    best_agent = Some((&agent.agent_id, score));
                }
            }

            match best_agent {
                Some((agent_id, score)) if score >= self.config.solvability_threshold => {
                    assignments.insert(st.id.clone(), (agent_id.to_string(), score));
                }
                _ => {
                    unsolvable.push(st.id.clone());
                    warn!(subtask = %st.id, "no agent meets solvability threshold");
                }
            }
        }

        SolvabilityResult {
            passed: unsolvable.is_empty(),
            unsolvable_subtasks: unsolvable,
            assignments,
        }
    }

    /// Compute capability match score between a subtask and an agent.
    fn capability_score(subtask: &Subtask, agent: &AgentCapabilityProfile) -> f64 {
        if subtask.required_capabilities.is_empty() {
            // No specific capabilities required — any agent can try.
            return 0.5;
        }
        let mut total = 0.0;
        let mut matched = 0.0;
        for cap in &subtask.required_capabilities {
            total += 1.0;
            if let Some(&score) = agent.capability_scores.get(cap) {
                matched += score;
            } else if agent.capabilities.contains(cap) {
                matched += 0.5; // Has capability but no confidence score.
            }
        }
        if total == 0.0 {
            0.5
        } else {
            matched / total
        }
    }

    /// Predicate 2: Completeness — Q ⊆ ⋃ sem(s_i).
    /// Uses cosine similarity between query embedding and subtask embeddings.
    fn check_completeness(
        &self,
        subtasks: &[Subtask],
        query_embedding: &[f32],
    ) -> CompletenessResult {
        if subtasks.is_empty() || query_embedding.is_empty() {
            return CompletenessResult {
                passed: false,
                coverage: 0.0,
                uncovered_aspects: vec!["no subtasks or no query embedding".into()],
            };
        }

        // Compute max cosine similarity between query and each subtask.
        // This measures how well the subtask set covers the query semantics.
        let similarities: Vec<f64> = subtasks
            .iter()
            .map(|st| Self::cosine_similarity_f32(query_embedding, &st.embedding) as f64)
            .collect();

        // Coverage = max similarity (best matching subtask).
        // A more sophisticated approach would decompose the query into aspects.
        let max_sim = similarities
            .iter()
            .copied()
            .fold(0.0f64, f64::max);

        // Average similarity as a secondary metric.
        let avg_sim = similarities.iter().sum::<f64>() / similarities.len() as f64;

        // Blend: coverage = 0.6 × max + 0.4 × avg.
        let coverage = 0.6 * max_sim + 0.4 * avg_sim;

        CompletenessResult {
            passed: coverage >= self.config.completeness_threshold,
            coverage,
            uncovered_aspects: if coverage < self.config.completeness_threshold {
                vec![format!(
                    "coverage {:.2} below threshold {:.2}",
                    coverage, self.config.completeness_threshold
                )]
            } else {
                Vec::new()
            },
        }
    }

    /// Predicate 3: Non-redundancy — ∀ i ≠ j, Jaccard(sem(s_i), sem(s_j)) < θ_r.
    /// Complexity: O(k²).
    fn check_non_redundancy(&self, subtasks: &[Subtask]) -> NonRedundancyResult {
        let mut redundant = Vec::new();

        for i in 0..subtasks.len() {
            for j in (i + 1)..subtasks.len() {
                let sim = Self::cosine_similarity_f32(
                    &subtasks[i].embedding,
                    &subtasks[j].embedding,
                ) as f64;
                // Use cosine similarity as a proxy for semantic Jaccard overlap.
                if sim > self.config.redundancy_threshold {
                    redundant.push((
                        subtasks[i].id.clone(),
                        subtasks[j].id.clone(),
                        sim,
                    ));
                    warn!(
                        a = %subtasks[i].id,
                        b = %subtasks[j].id,
                        similarity = sim,
                        "redundant subtask pair detected"
                    );
                }
            }
        }

        NonRedundancyResult {
            passed: redundant.is_empty(),
            redundant_pairs: redundant,
        }
    }

    /// Cosine similarity for f32 vectors.
    fn cosine_similarity_f32(a: &[f32], b: &[f32]) -> f32 {
        if a.len() != b.len() || a.is_empty() {
            return 0.0;
        }
        let mut dot = 0.0f32;
        let mut norm_a = 0.0f32;
        let mut norm_b = 0.0f32;
        for i in 0..a.len() {
            dot += a[i] * b[i];
            norm_a += a[i] * a[i];
            norm_b += b[i] * b[i];
        }
        let denom = norm_a.sqrt() * norm_b.sqrt();
        if denom < f32::EPSILON {
            0.0
        } else {
            dot / denom
        }
    }
}

// ───────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_subtask(id: &str, caps: &[&str], emb: Vec<f32>) -> Subtask {
        Subtask {
            id: id.into(),
            description: format!("Subtask {}", id),
            embedding: emb,
            required_capabilities: caps.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn make_agent(id: &str, caps: &[(&str, f64)]) -> AgentCapabilityProfile {
        AgentCapabilityProfile {
            agent_id: id.into(),
            capabilities: caps.iter().map(|(c, _)| c.to_string()).collect(),
            capability_scores: caps.iter().map(|(c, s)| (c.to_string(), *s)).collect(),
        }
    }

    #[test]
    fn test_solvability_pass() {
        let verifier = AopVerifier::new(VerificationConfig::default());
        let subtasks = vec![
            make_subtask("s1", &["coding"], vec![1.0, 0.0, 0.0]),
            make_subtask("s2", &["research"], vec![0.0, 1.0, 0.0]),
        ];
        let agents = vec![
            make_agent("coder", &[("coding", 0.9)]),
            make_agent("researcher", &[("research", 0.8)]),
        ];
        let result = verifier.verify(&subtasks, &agents, &[0.5, 0.5, 0.0]);
        assert!(result.solvability.passed);
    }

    #[test]
    fn test_solvability_fail() {
        let verifier = AopVerifier::new(VerificationConfig::default());
        let subtasks = vec![make_subtask("s1", &["quantum_physics"], vec![1.0, 0.0])];
        let agents = vec![make_agent("coder", &[("coding", 0.9)])];
        let result = verifier.verify(&subtasks, &agents, &[1.0, 0.0]);
        assert!(!result.solvability.passed);
        assert_eq!(result.solvability.unsolvable_subtasks, vec!["s1"]);
    }

    #[test]
    fn test_non_redundancy_detected() {
        let verifier = AopVerifier::new(VerificationConfig {
            redundancy_threshold: 0.9,
            ..Default::default()
        });
        // Nearly identical embeddings → redundant.
        let subtasks = vec![
            make_subtask("s1", &[], vec![1.0, 0.0, 0.0]),
            make_subtask("s2", &[], vec![0.99, 0.01, 0.0]),
        ];
        let result = verifier.verify(&subtasks, &[], &[1.0, 0.0, 0.0]);
        assert!(!result.non_redundancy.passed);
        assert_eq!(result.non_redundancy.redundant_pairs.len(), 1);
    }

    #[test]
    fn test_quality_score() {
        let verifier = AopVerifier::new(VerificationConfig::default());
        let subtasks = vec![
            make_subtask("s1", &["coding"], vec![0.8, 0.2, 0.0]),
            make_subtask("s2", &["research"], vec![0.2, 0.8, 0.0]),
        ];
        let agents = vec![
            make_agent("a1", &[("coding", 0.9), ("research", 0.7)]),
        ];
        let result = verifier.verify(&subtasks, &agents, &[0.5, 0.5, 0.0]);
        assert!(result.quality_score > 0.0);
        assert!(result.quality_score <= 1.0);
    }
}
