//! # Semantic Skill Router — Two-stage ranker replacing keyword substring matching.
//!
//! Stage 1: Capability bitset overlap O(W) — fast pruning of irrelevant skills.
//! Stage 2: Embedding-based semantic reranking O(S·d) — intent matching against
//! skill schema embeddings.
//!
//! This is materially stronger than the current `msg.contains("browse")` pattern
//! in `BrowserSkillProvider`, which is O(T·|m|) and semantically weak.
//!
//! ## Fallback
//!
//! When embeddings are unavailable (cold start, provider failure), the router
//! falls back to keyword matching as a degraded mode — preserving existing behavior.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ═══════════════════════════════════════════════════════════════════════════
// Skill descriptor — what the router knows about each skill
// ═══════════════════════════════════════════════════════════════════════════

/// A skill's routing metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDescriptor {
    /// Unique skill identifier.
    pub skill_id: String,
    /// Human-readable name.
    pub name: String,
    /// Short description of what this skill does.
    pub description: String,
    /// Required capability flags (bitset for fast overlap check).
    pub capabilities: u64,
    /// Pre-computed embedding of the skill description (d dimensions).
    /// `None` if embeddings are not available.
    pub embedding: Option<Vec<f32>>,
    /// Keyword triggers (fallback for when embeddings are unavailable).
    pub keyword_triggers: Vec<String>,
    /// Priority weight (higher = preferred when scores are equal).
    pub priority: f64,
}

/// Result of skill routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillRouteResult {
    /// Skill ID.
    pub skill_id: String,
    /// Combined routing score (0.0 - 1.0).
    pub score: f64,
    /// Whether this match was from semantic routing or keyword fallback.
    pub match_type: MatchType,
    /// Stage 1 capability overlap score.
    pub capability_overlap: f64,
    /// Stage 2 semantic similarity score (None if keywords were used).
    pub semantic_similarity: Option<f64>,
}

/// How the skill was matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchType {
    /// Matched via embedding similarity.
    Semantic,
    /// Matched via keyword substring (degraded mode).
    Keyword,
    /// Matched via capability overlap only.
    CapabilityOnly,
}

// ═══════════════════════════════════════════════════════════════════════════
// Capability flags — bitset for O(W) overlap check
// ═══════════════════════════════════════════════════════════════════════════

/// Capability flags for skills. Bitwise OR to combine, AND to check overlap.
pub mod capabilities {
    pub const TEXT_GENERATION: u64 = 1 << 0;
    pub const WEB_BROWSING: u64 = 1 << 1;
    pub const FILE_SYSTEM: u64 = 1 << 2;
    pub const CODE_EXECUTION: u64 = 1 << 3;
    pub const WEB_SEARCH: u64 = 1 << 4;
    pub const MEMORY: u64 = 1 << 5;
    pub const MESSAGING: u64 = 1 << 6;
    pub const MEDIA: u64 = 1 << 7;
    pub const SCHEDULING: u64 = 1 << 8;
    pub const DELEGATION: u64 = 1 << 9;
    pub const BROWSER_AUTOMATION: u64 = 1 << 10;
    pub const DATABASE: u64 = 1 << 11;
    pub const API_INTEGRATION: u64 = 1 << 12;
    pub const DOCUMENT_PROCESSING: u64 = 1 << 13;
}

/// Compute capability overlap score between request and skill.
/// Returns [0.0, 1.0] where 1.0 = perfect match.
pub fn capability_overlap(request_caps: u64, skill_caps: u64) -> f64 {
    if request_caps == 0 || skill_caps == 0 {
        return 0.0;
    }
    let intersection = (request_caps & skill_caps).count_ones() as f64;
    let union = (request_caps | skill_caps).count_ones() as f64;
    intersection / union // Jaccard similarity
}

// ═══════════════════════════════════════════════════════════════════════════
// Semantic similarity — cosine similarity on embeddings
// ═══════════════════════════════════════════════════════════════════════════

/// Compute cosine similarity between two embedding vectors.
/// Returns [-1.0, 1.0] where 1.0 = identical direction.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0f64;
    let mut norm_a = 0.0f64;
    let mut norm_b = 0.0f64;

    for (x, y) in a.iter().zip(b.iter()) {
        let x = *x as f64;
        let y = *y as f64;
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom < 1e-12 {
        0.0
    } else {
        dot / denom
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Semantic skill router
// ═══════════════════════════════════════════════════════════════════════════

/// Two-stage skill router: capability filtering → semantic reranking.
pub struct SemanticSkillRouter {
    /// Registered skill descriptors.
    skills: Vec<SkillDescriptor>,
    /// Score threshold below which skills are not returned.
    min_score: f64,
    /// Maximum number of results to return.
    max_results: usize,
    /// Weight for capability overlap vs semantic similarity.
    /// 0.0 = pure semantic, 1.0 = pure capability.
    capability_weight: f64,
}

impl SemanticSkillRouter {
    pub fn new() -> Self {
        Self {
            skills: Vec::new(),
            min_score: 0.3,
            max_results: 5,
            capability_weight: 0.3,
        }
    }

    pub fn with_min_score(mut self, score: f64) -> Self {
        self.min_score = score;
        self
    }

    pub fn with_max_results(mut self, max: usize) -> Self {
        self.max_results = max;
        self
    }

    /// Register a skill for routing.
    pub fn register(&mut self, skill: SkillDescriptor) {
        self.skills.push(skill);
    }

    /// Route a user request to the most relevant skills.
    ///
    /// Stage 1: Filter by capability overlap (bitset AND → O(W)).
    /// Stage 2: Rerank by semantic similarity (embedding dot product → O(S·d)).
    /// Fallback: Keyword matching if no embeddings available.
    pub fn route(
        &self,
        request_caps: u64,
        query_embedding: Option<&[f32]>,
        query_text: &str,
    ) -> Vec<SkillRouteResult> {
        let mut results: Vec<SkillRouteResult> = Vec::new();

        for skill in &self.skills {
            // Stage 1: Capability overlap (fast prune)
            let cap_score = capability_overlap(request_caps, skill.capabilities);

            // Stage 2: Semantic or keyword scoring
            let (semantic_score, match_type) =
                if let (Some(q_emb), Some(s_emb)) = (query_embedding, &skill.embedding) {
                    // Semantic reranking via cosine similarity
                    let sim = cosine_similarity(q_emb, s_emb);
                    let normalized = (sim + 1.0) / 2.0; // Map [-1,1] → [0,1]
                    (normalized, MatchType::Semantic)
                } else if !skill.keyword_triggers.is_empty() {
                    // Keyword fallback
                    let lower = query_text.to_lowercase();
                    let keyword_hit = skill
                        .keyword_triggers
                        .iter()
                        .any(|t| lower.contains(&t.to_lowercase()));
                    if keyword_hit {
                        (0.8, MatchType::Keyword)
                    } else {
                        (0.0, MatchType::Keyword)
                    }
                } else {
                    (0.0, MatchType::CapabilityOnly)
                };

            // Combined score: weighted sum of capability overlap + semantic
            let combined = self.capability_weight * cap_score
                + (1.0 - self.capability_weight) * semantic_score;

            // Apply priority weight
            let final_score = combined * skill.priority;

            if final_score >= self.min_score {
                results.push(SkillRouteResult {
                    skill_id: skill.skill_id.clone(),
                    score: final_score,
                    match_type,
                    capability_overlap: cap_score,
                    semantic_similarity: if match_type == MatchType::Semantic {
                        Some(semantic_score)
                    } else {
                        None
                    },
                });
            }
        }

        // Sort by score descending
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(self.max_results);
        results
    }

    /// Infer capability flags from a user message using heuristics.
    ///
    /// This bridges the gap between the old keyword matching and the new
    /// capability-based routing. In production, this would be replaced by
    /// an intent classifier.
    pub fn infer_capabilities(message: &str) -> u64 {
        let lower = message.to_lowercase();
        let mut caps = 0u64;

        if lower.contains("browse")
            || lower.contains("website")
            || lower.contains("navigate")
            || lower.contains("click")
            || lower.contains("web page")
        {
            caps |= capabilities::WEB_BROWSING | capabilities::BROWSER_AUTOMATION;
        }
        if lower.contains("search") || lower.contains("look up") || lower.contains("find") {
            caps |= capabilities::WEB_SEARCH;
        }
        if lower.contains("remember") || lower.contains("recall") || lower.contains("memory") {
            caps |= capabilities::MEMORY;
        }
        if lower.contains("file") || lower.contains("read") || lower.contains("write") {
            caps |= capabilities::FILE_SYSTEM;
        }
        if lower.contains("code") || lower.contains("run") || lower.contains("execute") {
            caps |= capabilities::CODE_EXECUTION;
        }
        if lower.contains("send") || lower.contains("message") || lower.contains("notify") {
            caps |= capabilities::MESSAGING;
        }
        if lower.contains("schedule") || lower.contains("cron") || lower.contains("remind") {
            caps |= capabilities::SCHEDULING;
        }
        if lower.contains("image") || lower.contains("video") || lower.contains("audio") {
            caps |= capabilities::MEDIA;
        }

        if caps == 0 {
            caps = capabilities::TEXT_GENERATION; // default
        }

        caps
    }
}

impl Default for SemanticSkillRouter {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn browser_skill() -> SkillDescriptor {
        SkillDescriptor {
            skill_id: "browser".into(),
            name: "Browser Automation".into(),
            description: "Navigate websites, click buttons, fill forms".into(),
            capabilities: capabilities::WEB_BROWSING | capabilities::BROWSER_AUTOMATION,
            embedding: None,
            keyword_triggers: vec![
                "browse".into(),
                "website".into(),
                "click".into(),
                "navigate".into(),
            ],
            priority: 1.0,
        }
    }

    fn memory_skill() -> SkillDescriptor {
        SkillDescriptor {
            skill_id: "memory".into(),
            name: "Memory Recall".into(),
            description: "Remember and recall information from past conversations".into(),
            capabilities: capabilities::MEMORY,
            embedding: None,
            keyword_triggers: vec!["remember".into(), "recall".into()],
            priority: 1.0,
        }
    }

    #[test]
    fn test_capability_overlap() {
        let a = capabilities::WEB_BROWSING | capabilities::TEXT_GENERATION;
        let b = capabilities::WEB_BROWSING | capabilities::BROWSER_AUTOMATION;
        let score = capability_overlap(a, b);
        // intersection = 1 (WEB_BROWSING), union = 3
        assert!((score - 1.0/3.0).abs() < 0.01);
    }

    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![1.0f32, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0f32, 0.0, 0.0];
        let b = vec![0.0f32, 1.0, 0.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn test_keyword_routing() {
        let mut router = SemanticSkillRouter::new().with_min_score(0.1);
        router.register(browser_skill());
        router.register(memory_skill());

        let results = router.route(
            capabilities::WEB_BROWSING,
            None,
            "Please browse to example.com",
        );

        assert!(!results.is_empty());
        assert_eq!(results[0].skill_id, "browser");
        assert_eq!(results[0].match_type, MatchType::Keyword);
    }

    #[test]
    fn test_semantic_routing() {
        let mut router = SemanticSkillRouter::new().with_min_score(0.1);

        let mut skill = browser_skill();
        skill.embedding = Some(vec![0.9, 0.1, 0.0]);
        router.register(skill);

        let query_emb = vec![0.85f32, 0.15, 0.0]; // similar to browser
        let results = router.route(
            capabilities::WEB_BROWSING,
            Some(&query_emb),
            "go to example.com",
        );

        assert!(!results.is_empty());
        assert_eq!(results[0].match_type, MatchType::Semantic);
        assert!(results[0].semantic_similarity.unwrap() > 0.8);
    }

    #[test]
    fn test_no_match_below_threshold() {
        let mut router = SemanticSkillRouter::new().with_min_score(0.9);
        router.register(browser_skill());

        let results = router.route(capabilities::MEMORY, None, "what time is it");
        assert!(results.is_empty());
    }

    #[test]
    fn test_infer_capabilities() {
        let caps = SemanticSkillRouter::infer_capabilities("Browse to example.com and click login");
        assert!(caps & capabilities::WEB_BROWSING != 0);
        assert!(caps & capabilities::BROWSER_AUTOMATION != 0);

        let mem_caps = SemanticSkillRouter::infer_capabilities("Do you remember what we discussed?");
        assert!(mem_caps & capabilities::MEMORY != 0);
    }
}
