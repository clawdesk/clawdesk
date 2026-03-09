//! RAG evaluation harness — measures retrieval quality.
//!
//! Provides offline evaluation of RAG pipeline quality using standard IR metrics:
//! - **Precision@K**: Fraction of top-K results that are relevant.
//! - **Recall@K**: Fraction of relevant documents found in top-K.
//! - **MRR (Mean Reciprocal Rank)**: Average of 1/rank of first relevant result.
//! - **NDCG@K (Normalized Discounted Cumulative Gain)**: Position-weighted relevance.
//!
//! # Usage
//!
//! ```ignore
//! let harness = EvalHarness::new();
//! harness.add_query("What is RAG?", &["doc1", "doc3"], &retrieved_ids);
//! let report = harness.evaluate();
//! println!("MRR: {:.3}", report.mean_mrr);
//! ```

/// A single evaluation query with ground-truth relevance labels.
#[derive(Debug, Clone)]
pub struct EvalQuery {
    /// The query text.
    pub query: String,
    /// IDs of actually relevant documents (ground truth).
    pub relevant_ids: Vec<String>,
    /// IDs of documents returned by the retrieval system, in ranked order.
    pub retrieved_ids: Vec<String>,
}

/// Evaluation metrics for a single query.
#[derive(Debug, Clone)]
pub struct QueryMetrics {
    pub query: String,
    pub precision_at_k: f64,
    pub recall_at_k: f64,
    pub mrr: f64,
    pub ndcg_at_k: f64,
    pub k: usize,
}

/// Aggregate evaluation report.
#[derive(Debug, Clone)]
pub struct EvalReport {
    /// Number of queries evaluated.
    pub num_queries: usize,
    /// Mean precision@K across all queries.
    pub mean_precision: f64,
    /// Mean recall@K across all queries.
    pub mean_recall: f64,
    /// Mean Reciprocal Rank across all queries.
    pub mean_mrr: f64,
    /// Mean NDCG@K across all queries.
    pub mean_ndcg: f64,
    /// Per-query metrics.
    pub per_query: Vec<QueryMetrics>,
}

/// Evaluation harness for RAG pipelines.
pub struct EvalHarness {
    queries: Vec<EvalQuery>,
    /// K for precision/recall/NDCG calculations.
    k: usize,
}

impl EvalHarness {
    /// Create a new evaluation harness. `k` is the cutoff for top-K metrics.
    pub fn new(k: usize) -> Self {
        Self {
            queries: Vec::new(),
            k,
        }
    }

    /// Add a query with its ground-truth relevant IDs and system-retrieved IDs.
    pub fn add_query(
        &mut self,
        query: impl Into<String>,
        relevant_ids: &[&str],
        retrieved_ids: &[&str],
    ) {
        self.queries.push(EvalQuery {
            query: query.into(),
            relevant_ids: relevant_ids.iter().map(|s| s.to_string()).collect(),
            retrieved_ids: retrieved_ids.iter().map(|s| s.to_string()).collect(),
        });
    }

    /// Run evaluation and produce an aggregate report.
    pub fn evaluate(&self) -> EvalReport {
        let per_query: Vec<QueryMetrics> = self
            .queries
            .iter()
            .map(|q| self.evaluate_query(q))
            .collect();

        let n = per_query.len().max(1) as f64;

        EvalReport {
            num_queries: per_query.len(),
            mean_precision: per_query.iter().map(|m| m.precision_at_k).sum::<f64>() / n,
            mean_recall: per_query.iter().map(|m| m.recall_at_k).sum::<f64>() / n,
            mean_mrr: per_query.iter().map(|m| m.mrr).sum::<f64>() / n,
            mean_ndcg: per_query.iter().map(|m| m.ndcg_at_k).sum::<f64>() / n,
            per_query,
        }
    }

    fn evaluate_query(&self, q: &EvalQuery) -> QueryMetrics {
        let top_k: Vec<&str> = q.retrieved_ids.iter().take(self.k).map(|s| s.as_str()).collect();
        let relevant: std::collections::HashSet<&str> =
            q.relevant_ids.iter().map(|s| s.as_str()).collect();

        let precision = precision_at_k(&top_k, &relevant);
        let recall = recall_at_k(&top_k, &relevant);
        let mrr = reciprocal_rank(&q.retrieved_ids, &relevant);
        let ndcg = ndcg_at_k(&top_k, &relevant);

        QueryMetrics {
            query: q.query.clone(),
            precision_at_k: precision,
            recall_at_k: recall,
            mrr,
            ndcg_at_k: ndcg,
            k: self.k,
        }
    }
}

/// Precision@K: |relevant ∩ retrieved@K| / K.
fn precision_at_k(retrieved: &[&str], relevant: &std::collections::HashSet<&str>) -> f64 {
    if retrieved.is_empty() {
        return 0.0;
    }
    let hits = retrieved.iter().filter(|id| relevant.contains(*id)).count();
    hits as f64 / retrieved.len() as f64
}

/// Recall@K: |relevant ∩ retrieved@K| / |relevant|.
fn recall_at_k(retrieved: &[&str], relevant: &std::collections::HashSet<&str>) -> f64 {
    if relevant.is_empty() {
        return 0.0;
    }
    let hits = retrieved.iter().filter(|id| relevant.contains(*id)).count();
    hits as f64 / relevant.len() as f64
}

/// Reciprocal Rank: 1/rank of first relevant result.
fn reciprocal_rank(retrieved: &[String], relevant: &std::collections::HashSet<&str>) -> f64 {
    for (i, id) in retrieved.iter().enumerate() {
        if relevant.contains(id.as_str()) {
            return 1.0 / (i + 1) as f64;
        }
    }
    0.0
}

/// NDCG@K: normalized discounted cumulative gain.
fn ndcg_at_k(retrieved: &[&str], relevant: &std::collections::HashSet<&str>) -> f64 {
    let dcg: f64 = retrieved
        .iter()
        .enumerate()
        .map(|(i, id)| {
            let rel = if relevant.contains(*id) { 1.0 } else { 0.0 };
            rel / (i as f64 + 2.0).log2()
        })
        .sum();

    // Ideal DCG: all relevant docs at the top.
    let ideal_relevant = relevant.len().min(retrieved.len());
    let idcg: f64 = (0..ideal_relevant)
        .map(|i| 1.0 / (i as f64 + 2.0).log2())
        .sum();

    if idcg == 0.0 {
        0.0
    } else {
        dcg / idcg
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perfect_retrieval() {
        let mut harness = EvalHarness::new(5);
        harness.add_query(
            "test query",
            &["a", "b", "c"],
            &["a", "b", "c", "d", "e"],
        );

        let report = harness.evaluate();
        assert!((report.mean_precision - 0.6).abs() < 0.01); // 3/5
        assert!((report.mean_recall - 1.0).abs() < 0.01); // 3/3
        assert!((report.mean_mrr - 1.0).abs() < 0.01); // first result is relevant
        assert!(report.mean_ndcg > 0.9);
    }

    #[test]
    fn no_relevant_results() {
        let mut harness = EvalHarness::new(5);
        harness.add_query("test", &["a", "b"], &["x", "y", "z"]);

        let report = harness.evaluate();
        assert_eq!(report.mean_precision, 0.0);
        assert_eq!(report.mean_recall, 0.0);
        assert_eq!(report.mean_mrr, 0.0);
    }

    #[test]
    fn mrr_with_late_hit() {
        let mut harness = EvalHarness::new(10);
        harness.add_query("test", &["c"], &["a", "b", "c", "d"]);

        let report = harness.evaluate();
        assert!((report.mean_mrr - 1.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn multiple_queries() {
        let mut harness = EvalHarness::new(3);
        harness.add_query("q1", &["a"], &["a", "b", "c"]);
        harness.add_query("q2", &["x"], &["y", "z", "w"]);

        let report = harness.evaluate();
        assert_eq!(report.num_queries, 2);
        // q1: precision=1/3, q2: precision=0 → mean=1/6
        assert!((report.mean_precision - 1.0 / 6.0).abs() < 0.01);
    }

    #[test]
    fn ndcg_position_sensitivity() {
        let mut h1 = EvalHarness::new(3);
        h1.add_query("test", &["a"], &["a", "b", "c"]); // relevant at position 1

        let mut h2 = EvalHarness::new(3);
        h2.add_query("test", &["a"], &["b", "c", "a"]); // relevant at position 3

        let r1 = h1.evaluate();
        let r2 = h2.evaluate();

        // NDCG should be higher when relevant doc is at position 1.
        assert!(r1.mean_ndcg > r2.mean_ndcg);
    }
}
