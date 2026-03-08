//! BM25 — Okapi BM25 scoring for keyword retrieval.
//!
//! Provides a lightweight in-process BM25 index that can be used alongside
//! vector search through Reciprocal Rank Fusion in `hybrid.rs`.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// BM25 tuning parameters.
#[derive(Debug, Clone, Copy)]
pub struct Bm25Params {
    /// Term frequency saturation (default 1.2).
    pub k1: f64,
    /// Document length normalization (default 0.75).
    pub b: f64,
}

impl Default for Bm25Params {
    fn default() -> Self {
        Self { k1: 1.2, b: 0.75 }
    }
}

/// In-memory BM25 index.
pub struct Bm25Index {
    params: Bm25Params,
    /// doc_id → token frequencies.
    docs: Vec<IndexedDoc>,
    /// token → list of (doc_index, tf).
    inverted: HashMap<String, Vec<(usize, u32)>>,
    /// Average document length (in tokens).
    avg_dl: f64,
    /// Total number of documents.
    doc_count: usize,
    /// Running sum of all document lengths — avoids O(N) recomputation.
    total_length: u64,
}

/// Indexed document.
struct IndexedDoc {
    id: String,
    length: u32,
    text: String,
}

/// BM25 search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bm25Result {
    pub id: String,
    pub score: f64,
    pub text: String,
}

impl Bm25Index {
    /// Create an empty index with default parameters.
    pub fn new() -> Self {
        Self::with_params(Bm25Params::default())
    }

    /// Create with custom BM25 parameters.
    pub fn with_params(params: Bm25Params) -> Self {
        Self {
            params,
            docs: Vec::new(),
            inverted: HashMap::new(),
            avg_dl: 0.0,
            doc_count: 0,
            total_length: 0,
        }
    }

    /// Add a document to the index.
    pub fn add_document(&mut self, id: &str, text: &str) {
        let tokens = tokenize(text);
        let length = tokens.len() as u32;
        let doc_idx = self.docs.len();

        // Count term frequencies
        let mut tf_map: HashMap<&str, u32> = HashMap::new();
        for token in &tokens {
            *tf_map.entry(token.as_str()).or_insert(0) += 1;
        }

        // Update inverted index
        for (token, tf) in &tf_map {
            self.inverted
                .entry(token.to_string())
                .or_default()
                .push((doc_idx, *tf));
        }

        self.docs.push(IndexedDoc {
            id: id.to_string(),
            length,
            text: text.to_string(),
        });

        self.doc_count = self.docs.len();
        // O(1) incremental avg_dl update instead of O(N) full sum.
        self.total_length += length as u64;
        self.avg_dl = self.total_length as f64 / self.doc_count as f64;
    }

    /// Search for documents matching the query.
    pub fn search(&self, query: &str, top_k: usize) -> Vec<Bm25Result> {
        if self.docs.is_empty() {
            return Vec::new();
        }

        let query_tokens = tokenize(query);
        let mut scores: Vec<f64> = vec![0.0; self.doc_count];

        for token in &query_tokens {
            if let Some(postings) = self.inverted.get(token.as_str()) {
                let df = postings.len() as f64;
                let idf = ((self.doc_count as f64 - df + 0.5) / (df + 0.5) + 1.0).ln();

                for &(doc_idx, tf) in postings {
                    let dl = self.docs[doc_idx].length as f64;
                    let tf_f = tf as f64;
                    let numerator = tf_f * (self.params.k1 + 1.0);
                    let denominator =
                        tf_f + self.params.k1 * (1.0 - self.params.b + self.params.b * dl / self.avg_dl);
                    scores[doc_idx] += idf * numerator / denominator;
                }
            }
        }

        // Top-k selection
        let mut scored: Vec<(usize, f64)> = scores
            .iter()
            .enumerate()
            .filter(|(_, &s)| s > 0.0)
            .map(|(i, &s)| (i, s))
            .collect();

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);

        scored
            .into_iter()
            .map(|(idx, score)| Bm25Result {
                id: self.docs[idx].id.clone(),
                score,
                text: self.docs[idx].text.clone(),
            })
            .collect()
    }

    /// Number of indexed documents.
    pub fn len(&self) -> usize {
        self.doc_count
    }

    pub fn is_empty(&self) -> bool {
        self.doc_count == 0
    }

    /// Number of unique terms.
    pub fn vocabulary_size(&self) -> usize {
        self.inverted.len()
    }

    /// Clear the index.
    pub fn clear(&mut self) {
        self.docs.clear();
        self.inverted.clear();
        self.avg_dl = 0.0;
        self.doc_count = 0;
        self.total_length = 0;
    }
}

/// Simple whitespace + punctuation tokenizer with lowercasing.
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() >= 2) // Skip single-char tokens
        .map(String::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_indexing_and_search() {
        let mut idx = Bm25Index::new();
        idx.add_document("1", "the quick brown fox jumps over the lazy dog");
        idx.add_document("2", "the quick brown fox");
        idx.add_document("3", "hello world from rust");

        let results = idx.search("quick fox", 10);
        assert!(!results.is_empty());
        // Both doc 1 and 2 should match
        assert!(results.iter().any(|r| r.id == "1"));
        assert!(results.iter().any(|r| r.id == "2"));
    }

    #[test]
    fn no_match_returns_empty() {
        let mut idx = Bm25Index::new();
        idx.add_document("1", "hello world");
        let results = idx.search("xyzzy", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn ranking_order() {
        let mut idx = Bm25Index::new();
        idx.add_document("a", "rust programming language");
        idx.add_document("b", "rust rust rust programming programming"); // higher TF
        idx.add_document("c", "python programming language");

        let results = idx.search("rust programming", 10);
        assert!(!results.is_empty());
        // "b" has more occurrences of "rust", should score highest
        assert_eq!(results[0].id, "b");
    }

    #[test]
    fn empty_index() {
        let idx = Bm25Index::new();
        assert!(idx.is_empty());
        let results = idx.search("anything", 5);
        assert!(results.is_empty());
    }

    #[test]
    fn vocabulary_tracking() {
        let mut idx = Bm25Index::new();
        idx.add_document("1", "hello world");
        idx.add_document("2", "hello rust");
        assert_eq!(idx.len(), 2);
        assert_eq!(idx.vocabulary_size(), 3); // hello, world, rust
    }

    #[test]
    fn tokenizer_skips_short() {
        let tokens = tokenize("I am a test");
        // "I", "a" are single chars, skipped
        assert!(tokens.iter().all(|t| t.len() >= 2));
        assert!(tokens.contains(&"am".to_string()));
        assert!(tokens.contains(&"test".to_string()));
    }
}
