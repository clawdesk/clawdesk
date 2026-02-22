//! FTS fallback and query expansion for memory search.
//!
//! When vector search returns too few results (below `min_results` threshold),
//! this module provides:
//!
//! 1. **FTS Fallback** — Fall back to BM25/keyword search with boosted scoring.
//! 2. **Query Expansion** — Expand the original query with synonyms, stemmed
//!    variants, and n-gram decomposition to improve recall.
//! 3. **Reciprocal Rank Fusion** — Merge expanded results with original
//!    vector results using RRF.
//!
//! ## Architecture
//!
//! The `FtsSearchFallback` wraps a `HybridSearcher` and adds the fallback
//! logic. It's a decorator that sits between the memory manager and the
//! underlying search implementation.
//!
//! ## Algorithm
//!
//! ```text
//! 1. Run vector search with original query
//! 2. If |results| >= min_results: return results
//! 3. Else: expand query → [q₁, q₂, ..., qₙ]
//! 4. Run BM25/keyword search for each expanded query
//! 5. Merge via RRF: score(d) = Σᵢ 1/(k + rankᵢ(d))
//! 6. Return top-k from merged results
//! ```
//!
//! ## Complexity
//! - Query expansion: O(|tokens|)
//! - FTS fallback: O(E × search_cost) where E = expanded query count
//! - RRF merge: O(N log N) where N = total results

use std::collections::{HashMap, HashSet};

/// Configuration for FTS fallback behaviour.
#[derive(Debug, Clone)]
pub struct FtsFallbackConfig {
    /// Minimum results from vector search before triggering fallback.
    pub min_results: usize,
    /// Minimum similarity score threshold for vector results.
    pub min_score: f64,
    /// Maximum number of expanded queries to generate.
    pub max_expansions: usize,
    /// RRF constant k.
    pub rrf_k: f64,
    /// Whether to include bigram expansions.
    pub use_bigrams: bool,
}

impl Default for FtsFallbackConfig {
    fn default() -> Self {
        Self {
            min_results: 3,
            min_score: 0.3,
            max_expansions: 5,
            rrf_k: 60.0,
            use_bigrams: true,
        }
    }
}

/// A scored search result from FTS fallback.
#[derive(Debug, Clone)]
pub struct FtsResult {
    /// Document/chunk ID.
    pub id: String,
    /// The content text.
    pub content: String,
    /// Combined RRF score.
    pub score: f64,
    /// Which queries matched this result.
    pub matched_queries: Vec<String>,
    /// Source strategy (vector, keyword, or expanded).
    pub source: FtsResultSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FtsResultSource {
    Vector,
    Keyword,
    Expanded,
    Fused,
}

/// Expanded query set generated from the original query.
#[derive(Debug, Clone)]
pub struct ExpandedQueries {
    /// The original query.
    pub original: String,
    /// Expanded variants.
    pub expansions: Vec<String>,
}

/// Expand a query into variant forms for better recall.
///
/// Generates:
/// - Individual tokens (unigrams)
/// - Bigrams (if `use_bigrams` is true)
/// - Simple suffix-stripped stems
///
/// ## Complexity
/// O(|tokens|²) worst case (with bigrams), O(|tokens|) without.
pub fn expand_query(query: &str, config: &FtsFallbackConfig) -> ExpandedQueries {
    let tokens: Vec<&str> = query
        .split_whitespace()
        .filter(|t| t.len() > 2) // skip very short words
        .collect();

    let mut expansions = Vec::new();
    let mut seen = HashSet::new();
    seen.insert(query.to_lowercase());

    // Add stemmed variants of individual tokens
    for token in &tokens {
        let stemmed = simple_stem(token);
        if stemmed != token.to_lowercase() && seen.insert(stemmed.clone()) {
            expansions.push(stemmed);
        }
    }

    // Add bigrams if enabled
    if config.use_bigrams && tokens.len() >= 2 {
        for pair in tokens.windows(2) {
            let bigram = format!("{} {}", pair[0], pair[1]);
            if seen.insert(bigram.to_lowercase()) {
                expansions.push(bigram);
            }
        }
    }

    // Trim to max expansions
    expansions.truncate(config.max_expansions);

    ExpandedQueries {
        original: query.to_string(),
        expansions,
    }
}

/// Very simple English suffix stripping (not a full stemmer).
///
/// Handles common suffixes: -ing, -tion, -ness, -ment, -able, -ible, -ed, -ly, -er, -est, -s.
fn simple_stem(word: &str) -> String {
    let w = word.to_lowercase();

    if w.len() < 4 {
        return w;
    }

    // Order matters — try longest suffixes first
    for suffix in &["ation", "tion", "ness", "ment", "able", "ible", "ing", "ous", "ful"] {
        if let Some(stripped) = w.strip_suffix(suffix) {
            if stripped.len() >= 3 {
                return stripped.to_string();
            }
        }
    }

    for suffix in &["ed", "ly", "er", "est"] {
        if let Some(stripped) = w.strip_suffix(suffix) {
            if stripped.len() >= 3 {
                return stripped.to_string();
            }
        }
    }

    // Plural -s (but not -ss)
    if w.ends_with('s') && !w.ends_with("ss") && w.len() > 3 {
        return w[..w.len() - 1].to_string();
    }

    w
}

/// Merge multiple result sets using Reciprocal Rank Fusion.
///
/// Each result set is a Vec of (id, content) pairs, ranked by relevance
/// (index 0 = most relevant). The RRF score for document d is:
///
/// `score(d) = Σᵢ 1/(k + rankᵢ(d))`
///
/// where the sum is over all result sets containing d.
///
/// ## Complexity
/// O(N log N) where N = total results across all sets.
pub fn rrf_merge(
    result_sets: &[Vec<(String, String)>],
    k: f64,
    top_k: usize,
) -> Vec<FtsResult> {
    let mut scores: HashMap<String, (f64, String, Vec<usize>)> = HashMap::new();

    for (set_idx, results) in result_sets.iter().enumerate() {
        for (rank, (id, content)) in results.iter().enumerate() {
            let rrf_score = 1.0 / (k + rank as f64 + 1.0);

            let entry = scores
                .entry(id.clone())
                .or_insert_with(|| (0.0, content.clone(), Vec::new()));
            entry.0 += rrf_score;
            entry.2.push(set_idx);
        }
    }

    let mut results: Vec<FtsResult> = scores
        .into_iter()
        .map(|(id, (score, content, sources))| FtsResult {
            id,
            content,
            score,
            matched_queries: sources.iter().map(|i| format!("set_{i}")).collect(),
            source: if sources.len() > 1 {
                FtsResultSource::Fused
            } else {
                FtsResultSource::Keyword
            },
        })
        .collect();

    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    results.truncate(top_k);
    results
}

/// Check if vector results are sufficient or if FTS fallback is needed.
pub fn needs_fallback(result_count: usize, min_score: f64, scores: &[f64], config: &FtsFallbackConfig) -> bool {
    if result_count < config.min_results {
        return true;
    }

    // Check if all results are below minimum score threshold
    let above_threshold = scores.iter().filter(|&&s| s >= min_score).count();
    above_threshold < config.min_results
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_stem() {
        assert_eq!(simple_stem("running"), "runn");
        assert_eq!(simple_stem("processing"), "process");
        assert_eq!(simple_stem("meaningful"), "meaning");
        assert_eq!(simple_stem("quickly"), "quick");
        assert_eq!(simple_stem("dogs"), "dog");
        // Short words unchanged
        assert_eq!(simple_stem("go"), "go");
    }

    #[test]
    fn test_expand_query() {
        let config = FtsFallbackConfig::default();
        let expanded = expand_query("machine learning algorithms", &config);

        assert_eq!(expanded.original, "machine learning algorithms");
        // Should have stemmed variants and bigrams
        assert!(!expanded.expansions.is_empty());
        // Should include bigrams
        assert!(
            expanded
                .expansions
                .iter()
                .any(|e| e.contains(' ')),
            "should have bigram expansions"
        );
    }

    #[test]
    fn test_rrf_merge() {
        let set1 = vec![
            ("doc1".to_string(), "content 1".to_string()),
            ("doc2".to_string(), "content 2".to_string()),
        ];
        let set2 = vec![
            ("doc2".to_string(), "content 2".to_string()),
            ("doc3".to_string(), "content 3".to_string()),
        ];

        let merged = rrf_merge(&[set1, set2], 60.0, 10);

        // doc2 appears in both sets → highest RRF score
        assert_eq!(merged[0].id, "doc2");
        assert_eq!(merged[0].source, FtsResultSource::Fused);
        assert_eq!(merged.len(), 3);
    }

    #[test]
    fn test_needs_fallback() {
        let config = FtsFallbackConfig {
            min_results: 3,
            min_score: 0.3,
            ..Default::default()
        };

        // Too few results
        assert!(needs_fallback(2, 0.3, &[0.5, 0.4], &config));

        // Enough results, good scores
        assert!(!needs_fallback(3, 0.3, &[0.5, 0.4, 0.35], &config));

        // Enough results, but all below threshold
        assert!(needs_fallback(3, 0.3, &[0.1, 0.1, 0.1], &config));
    }

    #[test]
    fn test_rrf_ordering() {
        let set1 = vec![
            ("a".to_string(), "".to_string()),
            ("b".to_string(), "".to_string()),
            ("c".to_string(), "".to_string()),
        ];

        let merged = rrf_merge(&[set1], 60.0, 10);
        // Order should be preserved: a > b > c
        assert_eq!(merged[0].id, "a");
        assert_eq!(merged[1].id, "b");
        assert_eq!(merged[2].id, "c");
        assert!(merged[0].score > merged[1].score);
    }
}
