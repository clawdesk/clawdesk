//! # Retrieval Stage — First-class lineage-aware retrieval pipeline node.
//!
//! Promotes retrieval from a subsystem/decorator to an explicit, inspectable
//! stage in the execution lineage DAG. Records which retrieval path was used,
//! which expansions fired, and which evidence won the merge.
//!
//! Wraps the existing `fts_fallback` hybrid search with lineage tracking.

use chrono::Utc;
use serde::{Deserialize, Serialize};

/// A retrieval stage execution record for the lineage graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalStageRecord {
    /// Unique record ID.
    pub id: String,
    /// The query that initiated retrieval.
    pub query: String,
    /// Which retrieval path was used.
    pub path: RetrievalPath,
    /// Number of results from vector search.
    pub vector_results: usize,
    /// Number of results from keyword/FTS search.
    pub keyword_results: usize,
    /// Number of query expansions used.
    pub expansions_used: usize,
    /// Expansion terms generated.
    pub expansion_terms: Vec<String>,
    /// Final merged result count.
    pub final_result_count: usize,
    /// Whether RRF fusion was applied.
    pub rrf_applied: bool,
    /// Top-k evidence entries with scores.
    pub evidence: Vec<EvidenceEntry>,
    /// Duration of the retrieval stage in milliseconds.
    pub duration_ms: u64,
    /// Timestamp.
    pub timestamp: String,
}

/// Which retrieval path was taken.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalPath {
    /// Vector search returned sufficient results.
    VectorOnly,
    /// Vector search was insufficient; FTS fallback activated.
    HybridWithFallback,
    /// Vector search unavailable; pure keyword search.
    KeywordOnly,
    /// Cache hit — no retrieval needed.
    CacheHit,
}

/// A single piece of evidence from retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceEntry {
    /// Content ID or reference.
    pub id: String,
    /// Preview of the evidence content.
    pub content_preview: String,
    /// Score from the retrieval system.
    pub score: f64,
    /// Source of this evidence (vector, keyword, expanded, fused).
    pub source: String,
}

impl RetrievalStageRecord {
    /// Create a new record for a retrieval execution.
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            query: query.into(),
            path: RetrievalPath::VectorOnly,
            vector_results: 0,
            keyword_results: 0,
            expansions_used: 0,
            expansion_terms: Vec::new(),
            final_result_count: 0,
            rrf_applied: false,
            evidence: Vec::new(),
            duration_ms: 0,
            timestamp: Utc::now().to_rfc3339(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retrieval_record_creation() {
        let record = RetrievalStageRecord::new("what is the config format?");
        assert!(!record.id.is_empty());
        assert_eq!(record.query, "what is the config format?");
        assert!(matches!(record.path, RetrievalPath::VectorOnly));
    }

    #[test]
    fn test_retrieval_record_serialization() {
        let mut record = RetrievalStageRecord::new("test query");
        record.path = RetrievalPath::HybridWithFallback;
        record.rrf_applied = true;
        record.evidence.push(EvidenceEntry {
            id: "e1".into(),
            content_preview: "relevant content".into(),
            score: 0.95,
            source: "vector".into(),
        });

        let json = serde_json::to_string(&record).unwrap();
        let parsed: RetrievalStageRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.evidence.len(), 1);
        assert!(parsed.rrf_applied);
    }
}
