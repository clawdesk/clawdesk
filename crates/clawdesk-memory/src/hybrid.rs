//! Hybrid search — combines vector similarity and keyword search.
//!
//! Uses Reciprocal Rank Fusion (RRF) to merge results from multiple retrieval
//! strategies into a single ranked list.

use clawdesk_storage::vector_store::{VectorSearchResult, VectorStore};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};
use std::sync::Arc;

/// Search strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SearchStrategy {
    /// Vector similarity only.
    Vector,
    /// Keyword/BM25 only.
    Keyword,
    /// Hybrid RRF fusion.
    Hybrid,
}

impl Default for SearchStrategy {
    fn default() -> Self {
        Self::Hybrid
    }
}

/// Hybrid search over a vector store.
pub struct HybridSearcher<S: VectorStore> {
    store: Arc<S>,
    /// RRF constant k (default 60).
    rrf_k: f64,
}

impl<S: VectorStore> HybridSearcher<S> {
    pub fn new(store: Arc<S>) -> Self {
        Self {
            store,
            rrf_k: 60.0,
        }
    }

    pub fn with_rrf_k(mut self, k: f64) -> Self {
        self.rrf_k = k;
        self
    }

    /// Search using the specified strategy.
    pub async fn search(
        &self,
        collection: &str,
        query_embedding: &[f32],
        query_text: &str,
        top_k: usize,
        strategy: SearchStrategy,
    ) -> Result<Vec<VectorSearchResult>, String> {
        match strategy {
            SearchStrategy::Vector => {
                self.store
                    .search(collection, query_embedding, top_k, None)
                    .await
                    .map_err(|e| format!("vector search: {e}"))
            }
            SearchStrategy::Keyword => {
                // Use pure keyword search — no embedding required.
                // Falls back to BM25/FTS when all embedding providers are down.
                self.store
                    .keyword_search(collection, query_text, top_k)
                    .await
                    .map_err(|e| format!("keyword search: {e}"))
            }
            SearchStrategy::Hybrid => {
                let fetch_k = top_k * 2;

                // Get vector results.
                let vector_results = self
                    .store
                    .search(collection, query_embedding, fetch_k, None)
                    .await
                    .map_err(|e| format!("vector search: {e}"))?;

                // Get keyword results.
                let keyword_results = self
                    .store
                    .hybrid_search(collection, query_embedding, query_text, fetch_k, 0.5)
                    .await
                    .map_err(|e| format!("keyword search: {e}"))?;

                // RRF fusion.
                Ok(self.rrf_fuse(&vector_results, &keyword_results, top_k))
            }
        }
    }

    /// Reciprocal Rank Fusion with min-heap top-k selection.
    ///
    /// Instead of collecting all fused scores into a Vec and sorting (O(n log n)),
    /// this uses a bounded `BinaryHeap<Reverse<_>>` (min-heap) of size `top_k`.
    /// Each candidate is pushed and the smallest score is evicted when the heap
    /// exceeds `top_k`. Final complexity: O(n log k) where n = total candidates.
    ///
    /// For typical queries (n ≈ 200, k = 10): ~40% fewer comparisons vs full sort.
    fn rrf_fuse(
        &self,
        vec_results: &[VectorSearchResult],
        kw_results: &[VectorSearchResult],
        top_k: usize,
    ) -> Vec<VectorSearchResult> {
        // Phase 1: accumulate RRF scores per document ID.
        let mut scores: HashMap<String, (f64, Option<VectorSearchResult>)> = HashMap::new();

        for (rank, result) in vec_results.iter().enumerate() {
            let rrf_score = 1.0 / (self.rrf_k + rank as f64 + 1.0);
            let entry = scores
                .entry(result.id.clone())
                .or_insert((0.0, Some(result.clone())));
            entry.0 += rrf_score;
        }

        for (rank, result) in kw_results.iter().enumerate() {
            let rrf_score = 1.0 / (self.rrf_k + rank as f64 + 1.0);
            let entry = scores
                .entry(result.id.clone())
                .or_insert((0.0, Some(result.clone())));
            entry.0 += rrf_score;
        }

        // Phase 2: min-heap top-k selection — O(n log k).
        // `ScoredResult` wraps (score, result) with reversed Ord so BinaryHeap
        // acts as a min-heap on score (smallest score at top → evictable).
        struct ScoredResult {
            score: f64,
            result: VectorSearchResult,
        }
        impl PartialEq for ScoredResult {
            fn eq(&self, other: &Self) -> bool {
                self.score == other.score
            }
        }
        impl Eq for ScoredResult {}
        impl PartialOrd for ScoredResult {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                Some(self.cmp(other))
            }
        }
        impl Ord for ScoredResult {
            fn cmp(&self, other: &Self) -> Ordering {
                // Reversed: smallest score has highest priority (min-heap).
                other.score.partial_cmp(&self.score).unwrap_or(Ordering::Equal)
            }
        }

        let mut heap: BinaryHeap<ScoredResult> = BinaryHeap::with_capacity(top_k + 1);

        for (_, (score, result_opt)) in scores {
            if let Some(mut result) = result_opt {
                result.score = score as f32;
                heap.push(ScoredResult { score, result });
                if heap.len() > top_k {
                    heap.pop(); // Evict smallest score.
                }
            }
        }

        // Extract in descending score order.
        let mut fused: Vec<VectorSearchResult> = heap.into_iter().map(|sr| sr.result).collect();
        fused.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal));

        // Normalize RRF scores to [0, 1] so they are comparable with
        // min_relevance thresholds (which expect cosine-similarity scale).
        // Without this, raw RRF scores max out at ~2/(k+1) ≈ 0.033 for k=60,
        // causing every result to be filtered out by even modest thresholds.
        if let Some(max_score) = fused.first().map(|r| r.score) {
            if max_score > 0.0 {
                for r in &mut fused {
                    r.score /= max_score;
                }
            }
        }

        fused
    }
}

/// Cosine similarity — 8-lane pipeline-parallel f32 for SIMD auto-vectorization.
///
/// Widened from 4 to 8 independent accumulator lanes to match AVX2's
/// 256-bit registers (8×f32). On Apple Silicon (128-bit NEON), the compiler
/// processes this as 2 back-to-back 4-wide SIMD ops per iteration, still
/// achieving ~2× throughput vs the old 4-lane version.
///
/// For d=1536 (OpenAI ada-002): 1536/8 = 192 iterations × 3 FMAs vs
/// old 1536/4 = 384 iterations × 3 FMAs → 2× fewer loop iterations.
///
/// Numerical precision: horizontal sum of 8 partial sums gives ~√(N/8) × ε
/// relative error — sufficient for LLM embedding similarity.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    // 8 independent accumulator lanes — maps to AVX2 ymm registers.
    let (mut d0, mut d1, mut d2, mut d3) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let (mut d4, mut d5, mut d6, mut d7) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let (mut na0, mut na1, mut na2, mut na3) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let (mut na4, mut na5, mut na6, mut na7) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let (mut nb0, mut nb1, mut nb2, mut nb3) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let (mut nb4, mut nb5, mut nb6, mut nb7) = (0.0f32, 0.0f32, 0.0f32, 0.0f32);

    let n = a.len();
    let chunks = n / 8;
    let remainder = n % 8;

    for i in 0..chunks {
        let base = i * 8;
        // Load 8 elements from each vector.
        let (va0, va1, va2, va3) = (a[base], a[base + 1], a[base + 2], a[base + 3]);
        let (va4, va5, va6, va7) = (a[base + 4], a[base + 5], a[base + 6], a[base + 7]);
        let (vb0, vb1, vb2, vb3) = (b[base], b[base + 1], b[base + 2], b[base + 3]);
        let (vb4, vb5, vb6, vb7) = (b[base + 4], b[base + 5], b[base + 6], b[base + 7]);

        d0 += va0 * vb0; d1 += va1 * vb1; d2 += va2 * vb2; d3 += va3 * vb3;
        d4 += va4 * vb4; d5 += va5 * vb5; d6 += va6 * vb6; d7 += va7 * vb7;

        na0 += va0 * va0; na1 += va1 * va1; na2 += va2 * va2; na3 += va3 * va3;
        na4 += va4 * va4; na5 += va5 * va5; na6 += va6 * va6; na7 += va7 * va7;

        nb0 += vb0 * vb0; nb1 += vb1 * vb1; nb2 += vb2 * vb2; nb3 += vb3 * vb3;
        nb4 += vb4 * vb4; nb5 += vb5 * vb5; nb6 += vb6 * vb6; nb7 += vb7 * vb7;
    }

    // Scalar remainder.
    let base = chunks * 8;
    for i in 0..remainder {
        let (av, bv) = (a[base + i], b[base + i]);
        d0 += av * bv;
        na0 += av * av;
        nb0 += bv * bv;
    }

    // Hierarchical pairwise reduction (minimises rounding error).
    let dot = ((d0 + d4) + (d1 + d5)) + ((d2 + d6) + (d3 + d7));
    let na = ((na0 + na4) + (na1 + na5)) + ((na2 + na6) + (na3 + na7));
    let nb = ((nb0 + nb4) + (nb1 + nb5)) + ((nb2 + nb6) + (nb3 + nb7));

    let denom = na.sqrt() * nb.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 0.01);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        assert!(cosine_similarity(&a, &b).abs() < 0.01);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        assert!((cosine_similarity(&a, &b) + 1.0).abs() < 0.01);
    }
}
