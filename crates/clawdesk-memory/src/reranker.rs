//! Cross-encoder reranking — re-score candidate results for precision.
//!
//! Implements both local lexical reranking (zero-dependency) and API-based
//! cross-encoder reranking (Cohere, Jina).

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

/// Reranker configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankerConfig {
    pub strategy: RerankerStrategy,
    /// Maximum candidates to consider before reranking.
    pub max_candidates: usize,
    /// Final top-k after reranking.
    pub top_k: usize,
}

impl Default for RerankerConfig {
    fn default() -> Self {
        Self {
            strategy: RerankerStrategy::Lexical,
            max_candidates: 50,
            top_k: 10,
        }
    }
}

/// Reranking strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RerankerStrategy {
    /// Simple lexical overlap scoring (no API calls).
    Lexical,
    /// Cohere Rerank API.
    CohereRerank,
    /// Jina Reranker API.
    JinaRerank,
    /// No reranking — pass through scores unchanged.
    PassThrough,
}

/// Candidate for reranking.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub id: String,
    pub text: String,
    pub original_score: f64,
}

/// Reranked result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankedResult {
    pub id: String,
    pub text: String,
    pub original_score: f64,
    pub rerank_score: f64,
    pub final_score: f64,
}

/// Lexical reranker — scores based on query term overlap with candidate text.
/// No external dependencies; instant feedback.
pub fn lexical_rerank(
    query: &str,
    candidates: &[Candidate],
    top_k: usize,
    original_weight: f64,
) -> Vec<RerankedResult> {
    let query_terms: Vec<String> = tokenize(query);
    if query_terms.is_empty() {
        return candidates
            .iter()
            .take(top_k)
            .map(|c| RerankedResult {
                id: c.id.clone(),
                text: c.text.clone(),
                original_score: c.original_score,
                rerank_score: 0.0,
                final_score: c.original_score,
            })
            .collect();
    }

    let mut results: Vec<RerankedResult> = candidates
        .iter()
        .map(|c| {
            let doc_terms: Vec<String> = tokenize(&c.text);
            let rerank_score = compute_overlap_score(&query_terms, &doc_terms);

            // Blend original and rerank scores
            let final_score =
                c.original_score * original_weight + rerank_score * (1.0 - original_weight);

            RerankedResult {
                id: c.id.clone(),
                text: c.text.clone(),
                original_score: c.original_score,
                rerank_score,
                final_score,
            }
        })
        .collect();

    results.sort_by(|a, b| {
        b.final_score
            .partial_cmp(&a.final_score)
            .unwrap_or(Ordering::Equal)
    });
    results.truncate(top_k);
    results
}

/// Compute overlap-based relevance score.
/// Combines exact match ratio with weighted position proximity.
fn compute_overlap_score(query_terms: &[String], doc_terms: &[String]) -> f64 {
    if doc_terms.is_empty() {
        return 0.0;
    }

    let mut matched = 0usize;
    let mut position_score = 0.0;

    for qt in query_terms {
        for (pos, dt) in doc_terms.iter().enumerate() {
            if dt == qt {
                matched += 1;
                // Earlier positions get higher weight
                position_score += 1.0 / (1.0 + pos as f64 * 0.1);
                break;
            }
        }
    }

    let coverage = matched as f64 / query_terms.len() as f64;
    let density = matched as f64 / doc_terms.len() as f64;

    // Weighted combination of coverage, density, and position
    coverage * 0.5 + density * 0.2 + (position_score / query_terms.len() as f64) * 0.3
}

/// Build request body for Cohere Rerank API.
pub fn build_cohere_rerank_body(
    query: &str,
    documents: &[String],
    top_n: usize,
    model: &str,
) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "query": query,
        "documents": documents,
        "top_n": top_n,
        "return_documents": false,
    })
}

/// Build request body for Jina Reranker API.
pub fn build_jina_rerank_body(
    query: &str,
    documents: &[String],
    top_n: usize,
    model: &str,
) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "query": query,
        "documents": documents,
        "top_n": top_n,
    })
}

/// Parse Cohere rerank response scores.
pub fn parse_cohere_rerank_response(body: &serde_json::Value) -> Vec<(usize, f64)> {
    body["results"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let index = item["index"].as_u64()? as usize;
                    let score = item["relevance_score"].as_f64()?;
                    Some((index, score))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Simple whitespace tokenizer with lowercasing.
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() >= 2)
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_candidate(id: &str, text: &str, score: f64) -> Candidate {
        Candidate {
            id: id.to_string(),
            text: text.to_string(),
            original_score: score,
        }
    }

    #[test]
    fn lexical_rerank_promotes_matching() {
        let candidates = vec![
            make_candidate("a", "unrelated content about cooking", 0.9),
            make_candidate("b", "rust programming language guide", 0.5),
            make_candidate("c", "the rust programming book", 0.4),
        ];

        let results = lexical_rerank("rust programming", &candidates, 3, 0.3);
        assert_eq!(results.len(), 3);
        // "b" and "c" should be promoted above "a" due to term overlap
        assert!(results[0].id == "b" || results[0].id == "c");
    }

    #[test]
    fn empty_candidates() {
        let results = lexical_rerank("test query", &[], 5, 0.5);
        assert!(results.is_empty());
    }

    #[test]
    fn empty_query_preserves_order() {
        let candidates = vec![
            make_candidate("a", "first", 0.9),
            make_candidate("b", "second", 0.5),
        ];

        let results = lexical_rerank("", &candidates, 2, 0.5);
        assert_eq!(results[0].id, "a"); // Original order preserved
    }

    #[test]
    fn top_k_truncation() {
        let candidates: Vec<_> = (0..20)
            .map(|i| make_candidate(&i.to_string(), &format!("document {}", i), 0.5))
            .collect();

        let results = lexical_rerank("document", &candidates, 5, 0.5);
        assert_eq!(results.len(), 5);
    }

    #[test]
    fn cohere_body_structure() {
        let body = build_cohere_rerank_body(
            "query",
            &["doc1".to_string(), "doc2".to_string()],
            5,
            "rerank-english-v3.0",
        );
        assert_eq!(body["query"], "query");
        assert_eq!(body["top_n"], 5);
        assert_eq!(body["documents"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn parse_cohere_response() {
        let response = serde_json::json!({
            "results": [
                {"index": 1, "relevance_score": 0.95},
                {"index": 0, "relevance_score": 0.42},
            ]
        });
        let scores = parse_cohere_rerank_response(&response);
        assert_eq!(scores.len(), 2);
        assert_eq!(scores[0], (1, 0.95));
    }
}
