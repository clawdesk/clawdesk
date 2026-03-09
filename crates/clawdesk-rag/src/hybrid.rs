//! Hybrid search with Reciprocal Rank Fusion (RRF).
//!
//! Combines vector (semantic) search and keyword (BM25) search results using
//! RRF to produce a single ranked list that benefits from both retrieval modes.
//!
//! ## Algorithm
//!
//! Given ranked lists $L_1, L_2, \ldots, L_n$ and a smoothing constant $k$:
//!
//! $$
//! \text{RRF}(d) = \sum_{i=1}^{n} \frac{1}{k + \text{rank}_i(d)}
//! $$
//!
//! Default $k = 60$ (from the original Cormack et al. paper). This balances
//! contributions from results ranked highly in either list.
//!
//! ## BM25 Parameters
//!
//! $$
//! \text{BM25}(q, d) = \sum_{t \in q} \text{IDF}(t) \cdot
//!   \frac{f(t, d) \cdot (k_1 + 1)}{f(t, d) + k_1 \cdot (1 - b + b \cdot \frac{|d|}{\text{avgdl}})}
//! $$
//!
//! Default: $k_1 = 1.2$, $b = 0.75$

use std::collections::HashMap;
use tracing::debug;

/// Configuration for hybrid search.
#[derive(Debug, Clone)]
pub struct HybridSearchConfig {
    /// RRF smoothing constant (default: 60).
    pub rrf_k: f64,
    /// Weight for vector search results in RRF (default: 0.5).
    pub vector_weight: f64,
    /// Weight for keyword search results in RRF (default: 0.5).
    pub keyword_weight: f64,
    /// BM25 k1 parameter — term frequency saturation (default: 1.2).
    pub bm25_k1: f64,
    /// BM25 b parameter — document length normalization (default: 0.75).
    pub bm25_b: f64,
    /// Maximum results to return.
    pub top_k: usize,
}

impl Default for HybridSearchConfig {
    fn default() -> Self {
        Self {
            rrf_k: 60.0,
            vector_weight: 0.5,
            keyword_weight: 0.5,
            bm25_k1: 1.2,
            bm25_b: 0.75,
            top_k: 10,
        }
    }
}

/// A single result from either vector or keyword search.
#[derive(Debug, Clone)]
pub struct SearchHit {
    /// Unique document/chunk identifier.
    pub id: String,
    /// The text content of the chunk.
    pub text: String,
    /// Original score from the search backend (similarity or BM25).
    pub score: f64,
    /// Optional metadata (doc_id, chunk_index, filename, etc.).
    pub metadata: HashMap<String, String>,
}

/// A fused result after RRF merging.
#[derive(Debug, Clone)]
pub struct HybridResult {
    /// Unique document/chunk identifier.
    pub id: String,
    /// The text content.
    pub text: String,
    /// RRF score (higher = better).
    pub rrf_score: f64,
    /// Rank in the vector search list (None if not present).
    pub vector_rank: Option<usize>,
    /// Rank in the keyword search list (None if not present).
    pub keyword_rank: Option<usize>,
    /// Original vector similarity score.
    pub vector_score: Option<f64>,
    /// Original keyword/BM25 score.
    pub keyword_score: Option<f64>,
    /// Merged metadata.
    pub metadata: HashMap<String, String>,
}

/// Reciprocal Rank Fusion combiner.
///
/// Merges two ranked lists into a single list ranked by weighted RRF score.
/// Complexity: O(n + m) where n, m are the sizes of the input lists.
pub struct RrfCombiner {
    config: HybridSearchConfig,
}

impl RrfCombiner {
    pub fn new(config: HybridSearchConfig) -> Self {
        Self { config }
    }

    pub fn with_defaults() -> Self {
        Self::new(HybridSearchConfig::default())
    }

    /// Fuse vector and keyword results using weighted RRF.
    ///
    /// Each result's RRF contribution is:
    ///   vector: weight_v / (k + rank_v)
    ///   keyword: weight_k / (k + rank_k)
    ///
    /// Results present in only one list get the contribution from that list only.
    pub fn fuse(
        &self,
        vector_results: &[SearchHit],
        keyword_results: &[SearchHit],
    ) -> Vec<HybridResult> {
        let k = self.config.rrf_k;
        let wv = self.config.vector_weight;
        let wk = self.config.keyword_weight;

        // Build lookup by ID → (rank, score, hit)
        let mut combined: HashMap<String, HybridResult> = HashMap::new();

        for (rank, hit) in vector_results.iter().enumerate() {
            let rrf_contribution = wv / (k + (rank + 1) as f64);
            combined
                .entry(hit.id.clone())
                .and_modify(|r| {
                    r.rrf_score += rrf_contribution;
                    r.vector_rank = Some(rank + 1);
                    r.vector_score = Some(hit.score);
                })
                .or_insert_with(|| HybridResult {
                    id: hit.id.clone(),
                    text: hit.text.clone(),
                    rrf_score: rrf_contribution,
                    vector_rank: Some(rank + 1),
                    keyword_rank: None,
                    vector_score: Some(hit.score),
                    keyword_score: None,
                    metadata: hit.metadata.clone(),
                });
        }

        for (rank, hit) in keyword_results.iter().enumerate() {
            let rrf_contribution = wk / (k + (rank + 1) as f64);
            combined
                .entry(hit.id.clone())
                .and_modify(|r| {
                    r.rrf_score += rrf_contribution;
                    r.keyword_rank = Some(rank + 1);
                    r.keyword_score = Some(hit.score);
                    // Merge metadata (keyword metadata supplements vector metadata)
                    for (mk, mv) in &hit.metadata {
                        r.metadata.entry(mk.clone()).or_insert_with(|| mv.clone());
                    }
                })
                .or_insert_with(|| HybridResult {
                    id: hit.id.clone(),
                    text: hit.text.clone(),
                    rrf_score: rrf_contribution,
                    vector_rank: None,
                    keyword_rank: Some(rank + 1),
                    vector_score: None,
                    keyword_score: Some(hit.score),
                    metadata: hit.metadata.clone(),
                });
        }

        // Sort by RRF score descending, take top_k
        let mut results: Vec<HybridResult> = combined.into_values().collect();
        results.sort_by(|a, b| b.rrf_score.partial_cmp(&a.rrf_score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(self.config.top_k);

        debug!(
            total_fused = results.len(),
            vector_input = vector_results.len(),
            keyword_input = keyword_results.len(),
            "hybrid search RRF fusion complete"
        );

        results
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// BM25 scorer
// ─────────────────────────────────────────────────────────────────────────────

/// Simple BM25 scorer for keyword search over a document corpus.
///
/// Operates on pre-tokenized documents. Tokenization is left to the caller
/// (typically whitespace split + lowercasing + optional stemming).
pub struct Bm25Scorer {
    /// BM25 k1 parameter.
    k1: f64,
    /// BM25 b parameter.
    b: f64,
    /// Average document length across the corpus.
    avgdl: f64,
    /// Total number of documents.
    num_docs: usize,
    /// Document frequency: term → count of documents containing it.
    doc_freq: HashMap<String, usize>,
    /// Per-document term frequencies and lengths.
    doc_store: Vec<DocEntry>,
}

struct DocEntry {
    id: String,
    text: String,
    term_freqs: HashMap<String, usize>,
    doc_len: usize,
    metadata: HashMap<String, String>,
}

impl Bm25Scorer {
    /// Build a BM25 index from a set of documents.
    ///
    /// Each document is a (id, text, metadata) triple. Text is tokenized
    /// by splitting on whitespace and lowercasing.
    pub fn build(
        documents: Vec<(String, String, HashMap<String, String>)>,
        k1: f64,
        b: f64,
    ) -> Self {
        let num_docs = documents.len();
        let mut doc_freq: HashMap<String, usize> = HashMap::new();
        let mut doc_store = Vec::with_capacity(num_docs);
        let mut total_len: usize = 0;

        for (id, text, metadata) in documents {
            let tokens: Vec<String> = text.split_whitespace().map(|t| t.to_lowercase()).collect();
            let doc_len = tokens.len();
            total_len += doc_len;

            let mut term_freqs: HashMap<String, usize> = HashMap::new();
            let mut seen_terms: std::collections::HashSet<String> = std::collections::HashSet::new();

            for token in &tokens {
                *term_freqs.entry(token.clone()).or_insert(0) += 1;
                if seen_terms.insert(token.clone()) {
                    *doc_freq.entry(token.clone()).or_insert(0) += 1;
                }
            }

            doc_store.push(DocEntry {
                id,
                text,
                term_freqs,
                doc_len,
                metadata,
            });
        }

        let avgdl = if num_docs > 0 {
            total_len as f64 / num_docs as f64
        } else {
            1.0
        };

        Self {
            k1,
            b,
            avgdl,
            num_docs,
            doc_freq,
            doc_store,
        }
    }

    /// Score all documents against the query and return top results.
    pub fn search(&self, query: &str, top_k: usize) -> Vec<SearchHit> {
        let query_terms: Vec<String> = query.split_whitespace().map(|t| t.to_lowercase()).collect();
        let mut scored: Vec<(usize, f64)> = Vec::with_capacity(self.doc_store.len());

        for (idx, doc) in self.doc_store.iter().enumerate() {
            let mut score = 0.0;
            for term in &query_terms {
                let tf = *doc.term_freqs.get(term).unwrap_or(&0) as f64;
                if tf == 0.0 {
                    continue;
                }
                let df = *self.doc_freq.get(term).unwrap_or(&0) as f64;
                let idf = ((self.num_docs as f64 - df + 0.5) / (df + 0.5) + 1.0).ln();
                let norm = tf * (self.k1 + 1.0)
                    / (tf + self.k1 * (1.0 - self.b + self.b * doc.doc_len as f64 / self.avgdl));
                score += idf * norm;
            }
            if score > 0.0 {
                scored.push((idx, score));
            }
        }

        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(top_k);

        scored
            .into_iter()
            .map(|(idx, score)| {
                let doc = &self.doc_store[idx];
                SearchHit {
                    id: doc.id.clone(),
                    text: doc.text.clone(),
                    score,
                    metadata: doc.metadata.clone(),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_hit(id: &str, score: f64) -> SearchHit {
        SearchHit {
            id: id.to_string(),
            text: format!("text for {id}"),
            score,
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn rrf_basic_fusion() {
        let combiner = RrfCombiner::with_defaults();
        let vector = vec![make_hit("a", 0.9), make_hit("b", 0.8), make_hit("c", 0.7)];
        let keyword = vec![make_hit("b", 5.0), make_hit("d", 4.0), make_hit("a", 3.0)];

        let results = combiner.fuse(&vector, &keyword);

        // "b" should rank highest: vector rank 2 + keyword rank 1
        // "a" should rank second: vector rank 1 + keyword rank 3
        assert_eq!(results[0].id, "b");
        assert_eq!(results[1].id, "a");

        // Both should have contributions from both lists
        assert!(results[0].vector_rank.is_some());
        assert!(results[0].keyword_rank.is_some());
    }

    #[test]
    fn rrf_single_list() {
        let combiner = RrfCombiner::with_defaults();
        let vector = vec![make_hit("x", 0.95)];
        let keyword = vec![];

        let results = combiner.fuse(&vector, &keyword);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "x");
        assert!(results[0].keyword_rank.is_none());
    }

    #[test]
    fn rrf_respects_top_k() {
        let config = HybridSearchConfig {
            top_k: 2,
            ..Default::default()
        };
        let combiner = RrfCombiner::new(config);
        let vector = vec![make_hit("a", 0.9), make_hit("b", 0.8), make_hit("c", 0.7)];
        let keyword = vec![make_hit("d", 5.0), make_hit("e", 4.0)];

        let results = combiner.fuse(&vector, &keyword);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn bm25_basic_scoring() {
        let docs = vec![
            ("doc1".into(), "the quick brown fox".into(), HashMap::new()),
            ("doc2".into(), "the lazy brown dog".into(), HashMap::new()),
            ("doc3".into(), "quick fox jumps over".into(), HashMap::new()),
        ];

        let scorer = Bm25Scorer::build(docs, 1.2, 0.75);
        let results = scorer.search("quick fox", 10);

        // doc1 and doc3 both contain "quick" and "fox"
        assert!(results.len() >= 2);
        // Both "quick fox" docs should score higher than doc2 which has neither
        let ids: Vec<&str> = results.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"doc1"));
        assert!(ids.contains(&"doc3"));
    }

    #[test]
    fn bm25_empty_query() {
        let docs = vec![("doc1".into(), "hello world".into(), HashMap::new())];
        let scorer = Bm25Scorer::build(docs, 1.2, 0.75);
        let results = scorer.search("", 10);
        assert!(results.is_empty());
    }
}
