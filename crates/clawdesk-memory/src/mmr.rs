//! Maximal Marginal Relevance (MMR) — diversity re-ranking.
//!
//! Penalizes results that are too similar to already-selected results,
//! maximizing information diversity in the returned set.
//!
//! ## Algorithm (Carbonell & Goldstein, 1998)
//!
//! ```text
//! MMR(d_i) = λ · score(d_i) - (1 - λ) · max_{d_j ∈ S} sim(d_i, d_j)
//! ```
//!
//! Where:
//! - S = already-selected set (greedy, iterative)
//! - λ = relevance-diversity tradeoff (default 0.7)
//! - sim(d_i, d_j) = Jaccard token overlap (no stored embeddings needed)
//!
//! ## Complexity
//!
//! For k results from n candidates:
//! - Pairwise Jaccard: O(k · n · V) where V = avg vocabulary per doc
//! - Greedy selection: O(k · n)
//! - Total: O(k² · n) — for k=10, n=50: 5,000 ops. Negligible.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// Configuration for MMR re-ranking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MmrConfig {
    /// Relevance-diversity tradeoff: 1.0 = pure relevance, 0.0 = pure diversity.
    pub lambda: f32,
    /// Number of final results to return.
    pub top_k: usize,
}

impl Default for MmrConfig {
    fn default() -> Self {
        Self {
            lambda: 0.7,
            top_k: 10,
        }
    }
}

/// A candidate for MMR re-ranking.
#[derive(Debug, Clone)]
pub struct MmrCandidate {
    /// Unique identifier.
    pub id: String,
    /// Original relevance score (from hybrid search).
    pub score: f32,
    /// The text content (used for Jaccard similarity).
    pub content: String,
    /// Opaque metadata carried through.
    pub metadata: serde_json::Value,
}

/// Result of MMR re-ranking.
#[derive(Debug, Clone)]
pub struct MmrResult {
    pub id: String,
    pub score: f32,
    pub mmr_score: f32,
    pub content: String,
    pub metadata: serde_json::Value,
}

/// Apply MMR re-ranking to a set of candidates.
///
/// Greedily selects `top_k` results, at each step choosing the candidate
/// that maximizes `λ · relevance - (1-λ) · max_similarity_to_selected`.
pub fn mmr_rerank(candidates: &[MmrCandidate], config: &MmrConfig) -> Vec<MmrResult> {
    if candidates.is_empty() {
        return Vec::new();
    }

    let k = config.top_k.min(candidates.len());
    let lambda = config.lambda;

    // Pre-tokenize all candidates for Jaccard computation
    let tokenized: Vec<HashSet<String>> = candidates
        .iter()
        .map(|c| tokenize_for_jaccard(&c.content))
        .collect();

    let mut selected: Vec<usize> = Vec::with_capacity(k);
    let mut remaining: Vec<usize> = (0..candidates.len()).collect();
    let mut results: Vec<MmrResult> = Vec::with_capacity(k);

    // Normalize scores to [0, 1] for fair combination with Jaccard
    let max_score = candidates
        .iter()
        .map(|c| c.score)
        .fold(f32::NEG_INFINITY, f32::max);
    let min_score = candidates
        .iter()
        .map(|c| c.score)
        .fold(f32::INFINITY, f32::min);
    let score_range = (max_score - min_score).max(1e-6);

    for _ in 0..k {
        if remaining.is_empty() {
            break;
        }

        let mut best_idx_in_remaining = 0;
        let mut best_mmr = f32::NEG_INFINITY;

        for (ri, &ci) in remaining.iter().enumerate() {
            let norm_score = (candidates[ci].score - min_score) / score_range;
            let relevance_term = lambda * norm_score;

            // Max similarity to any already-selected document
            let diversity_penalty = if selected.is_empty() {
                0.0
            } else {
                selected
                    .iter()
                    .map(|&si| jaccard_similarity(&tokenized[ci], &tokenized[si]))
                    .fold(0.0f32, f32::max)
            };

            let mmr_score = relevance_term - (1.0 - lambda) * diversity_penalty;

            if mmr_score > best_mmr {
                best_mmr = mmr_score;
                best_idx_in_remaining = ri;
            }
        }

        let chosen = remaining.remove(best_idx_in_remaining);
        selected.push(chosen);

        results.push(MmrResult {
            id: candidates[chosen].id.clone(),
            score: candidates[chosen].score,
            mmr_score: best_mmr,
            content: candidates[chosen].content.clone(),
            metadata: candidates[chosen].metadata.clone(),
        });
    }

    results
}

/// Jaccard similarity between two token sets.
///
/// J(A, B) = |A ∩ B| / |A ∪ B|
fn jaccard_similarity(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let intersection = a.intersection(b).count();
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    intersection as f32 / union as f32
}

/// Tokenize text for Jaccard computation.
///
/// Simple whitespace + punctuation split with lowercasing.
/// Filters tokens shorter than 2 characters.
fn tokenize_for_jaccard(text: &str) -> HashSet<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() >= 2)
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_candidate(id: &str, score: f32, content: &str) -> MmrCandidate {
        MmrCandidate {
            id: id.to_string(),
            score,
            content: content.to_string(),
            metadata: serde_json::json!({}),
        }
    }

    #[test]
    fn mmr_empty_candidates() {
        let results = mmr_rerank(&[], &MmrConfig::default());
        assert!(results.is_empty());
    }

    #[test]
    fn mmr_single_candidate() {
        let candidates = vec![make_candidate("1", 0.9, "hello world")];
        let results = mmr_rerank(&candidates, &MmrConfig::default());
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "1");
    }

    #[test]
    fn mmr_diversity_penalizes_duplicates() {
        let candidates = vec![
            make_candidate("1", 0.95, "the quick brown fox jumps over the lazy dog"),
            make_candidate("2", 0.90, "the quick brown fox jumps over the lazy dog"),  // duplicate
            make_candidate("3", 0.80, "rust programming language systems design"),     // diverse
        ];
        let config = MmrConfig { lambda: 0.5, top_k: 2 };
        let results = mmr_rerank(&candidates, &config);
        assert_eq!(results.len(), 2);
        // First should be highest-scoring
        assert_eq!(results[0].id, "1");
        // Second should be diverse (3), not duplicate (2)
        assert_eq!(results[1].id, "3");
    }

    #[test]
    fn mmr_pure_relevance() {
        let candidates = vec![
            make_candidate("1", 0.9, "hello world"),
            make_candidate("2", 0.8, "hello world"),
            make_candidate("3", 0.7, "goodbye world"),
        ];
        let config = MmrConfig { lambda: 1.0, top_k: 3 };
        let results = mmr_rerank(&candidates, &config);
        // With lambda=1.0, pure relevance — should be in score order
        assert_eq!(results[0].id, "1");
        assert_eq!(results[1].id, "2");
        assert_eq!(results[2].id, "3");
    }

    #[test]
    fn jaccard_identical() {
        let a: HashSet<String> = ["hello", "world"].iter().map(|s| s.to_string()).collect();
        let b = a.clone();
        assert!((jaccard_similarity(&a, &b) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn jaccard_disjoint() {
        let a: HashSet<String> = ["hello", "world"].iter().map(|s| s.to_string()).collect();
        let b: HashSet<String> = ["foo", "bar"].iter().map(|s| s.to_string()).collect();
        assert!(jaccard_similarity(&a, &b).abs() < 1e-6);
    }

    #[test]
    fn jaccard_partial_overlap() {
        let a: HashSet<String> = ["hello", "world", "test"].iter().map(|s| s.to_string()).collect();
        let b: HashSet<String> = ["hello", "world", "other"].iter().map(|s| s.to_string()).collect();
        // intersection=2, union=4 → 0.5
        assert!((jaccard_similarity(&a, &b) - 0.5).abs() < 1e-6);
    }
}
