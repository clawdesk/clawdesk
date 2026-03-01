//! File-backed memory store — JSON persistence without a vector database.
//!
//! ## Design
//!
//! Provides a lightweight `MemoryManager` alternative for environments where
//! SochDB or another vector store isn't available. Memories are stored as a
//! JSON array on disk, and recall uses BM25 keyword matching (no embeddings).
//!
//! ## Persistence format
//!
//! ```json
//! [
//!   {
//!     "id": "uuid",
//!     "content": "text",
//!     "source": "UserSaved",
//!     "tags": ["tag1", "tag2"],
//!     "timestamp": "2026-01-01T00:00:00Z",
//!     "metadata": {}
//!   }
//! ]
//! ```
//!
//! ## Concurrency
//!
//! Uses `tokio::sync::RwLock` for concurrent read access with exclusive writes.
//! File I/O is done via `tokio::fs` to avoid blocking the async runtime.

use chrono::{DateTime, Utc};
use clawdesk_types::DropOldest;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// A single memory entry in the file store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMemoryEntry {
    /// Unique identifier.
    pub id: String,
    /// The memory content text.
    pub content: String,
    /// Source of this memory (Conversation, UserSaved, etc.).
    pub source: String,
    /// Tags for categorization.
    pub tags: Vec<String>,
    /// When this memory was stored.
    pub timestamp: DateTime<Utc>,
    /// Additional metadata.
    pub metadata: serde_json::Value,
}

/// Result of a file-backed memory search.
#[derive(Debug, Clone)]
pub struct FileMemoryResult {
    pub entry: FileMemoryEntry,
    /// BM25-based relevance score.
    pub score: f32,
}

/// File-backed memory store configuration.
#[derive(Debug, Clone)]
pub struct FileMemoryConfig {
    /// Path to the JSON file.
    pub path: PathBuf,
    /// Maximum number of memories to store (FIFO eviction beyond this).
    pub max_entries: usize,
    /// Minimum BM25 score for recall results.
    pub min_relevance: f32,
}

impl Default for FileMemoryConfig {
    fn default() -> Self {
        Self {
            path: PathBuf::from("memories.json"),
            max_entries: 10_000,
            min_relevance: 0.1,
        }
    }
}

/// File-backed memory store.
///
/// Stores memories as a JSON array on disk. Recall uses BM25 keyword matching.
/// Suitable for development, testing, and environments without a vector DB.
pub struct FileMemoryStore {
    config: FileMemoryConfig,
    entries: RwLock<DropOldest<FileMemoryEntry>>,
}

impl FileMemoryStore {
    /// Create or load a file-backed memory store.
    pub async fn open(config: FileMemoryConfig) -> Result<Self, String> {
        let mut ring = DropOldest::new(config.max_entries.max(1));
        if config.path.exists() {
            let data = tokio::fs::read_to_string(&config.path)
                .await
                .map_err(|e| format!("read {}: {}", config.path.display(), e))?;
            let parsed: Vec<FileMemoryEntry> =
                serde_json::from_str(&data).unwrap_or_else(|e| {
                    warn!(
                        path = %config.path.display(),
                        error = %e,
                        "Failed to parse memory file, starting fresh"
                    );
                    Vec::new()
                });
            info!(
                path = %config.path.display(),
                entries = parsed.len(),
                "Loaded file-backed memory store"
            );
            ring.extend(parsed);
        } else {
            info!(
                path = %config.path.display(),
                "Creating new file-backed memory store"
            );
        }

        Ok(Self {
            config,
            entries: RwLock::new(ring),
        })
    }

    /// Store a memory entry.
    pub async fn remember(
        &self,
        content: &str,
        source: &str,
        tags: Vec<String>,
        metadata: serde_json::Value,
    ) -> Result<String, String> {
        let id = uuid::Uuid::new_v4().to_string();
        let entry = FileMemoryEntry {
            id: id.clone(),
            content: content.to_string(),
            source: source.to_string(),
            tags,
            timestamp: Utc::now(),
            metadata,
        };

        let mut entries = self.entries.write().await;

        // DropOldest handles FIFO eviction automatically
        entries.push(entry);
        debug!(id = %id, total = entries.len(), "file memory stored");

        // Persist to disk
        self.persist_locked(&entries.to_vec()).await?;

        Ok(id)
    }

    /// Recall memories matching a query using BM25 keyword matching.
    pub async fn recall(
        &self,
        query: &str,
        max_results: usize,
    ) -> Vec<FileMemoryResult> {
        let entries = self.entries.read().await;
        let query_terms = tokenize(query);

        if query_terms.is_empty() {
            return Vec::new();
        }

        // Compute IDF for each query term
        let n = entries.len() as f64;
        let mut idf: std::collections::HashMap<&str, f64> = std::collections::HashMap::new();
        for term in &query_terms {
            let df = entries
                .iter()
                .filter(|e| {
                    let doc_terms = tokenize(&e.content);
                    doc_terms.contains(term)
                })
                .count() as f64;
            let score = ((n - df + 0.5) / (df + 0.5) + 1.0).ln();
            idf.insert(term.as_str(), score.max(0.0));
        }

        // BM25 scoring
        let k1 = 1.2;
        let b = 0.75;
        let avg_dl: f64 = if entries.is_empty() {
            1.0
        } else {
            entries.iter().map(|e| e.content.split_whitespace().count() as f64).sum::<f64>() / n
        };

        let mut results: Vec<FileMemoryResult> = entries
            .iter()
            .filter_map(|entry| {
                let doc_terms = tokenize(&entry.content);
                let dl = doc_terms.len() as f64;
                let mut score = 0.0f64;

                for term in &query_terms {
                    let tf = doc_terms.iter().filter(|t| *t == term).count() as f64;
                    if tf == 0.0 {
                        continue;
                    }
                    let term_idf = idf.get(term.as_str()).copied().unwrap_or(0.0);
                    let numerator = tf * (k1 + 1.0);
                    let denominator = tf + k1 * (1.0 - b + b * dl / avg_dl);
                    score += term_idf * numerator / denominator;
                }

                // Tag bonus: if query terms match any tag, boost score
                let tag_boost: f64 = entry
                    .tags
                    .iter()
                    .filter(|t| query_terms.iter().any(|q| t.to_lowercase().contains(q)))
                    .count() as f64
                    * 0.2;

                score += tag_boost;

                if score as f32 >= self.config.min_relevance {
                    Some(FileMemoryResult {
                        entry: entry.clone(),
                        score: score as f32,
                    })
                } else {
                    None
                }
            })
            .collect();

        // Sort by score descending
        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        results.truncate(max_results);
        results
    }

    /// Forget a specific memory by ID.
    pub async fn forget(&self, id: &str) -> Result<bool, String> {
        let mut entries = self.entries.write().await;
        let before = entries.len();
        entries.retain(|e| e.id != id);
        let removed = entries.len() < before;
        if removed {
            self.persist_locked(&entries.to_vec()).await?;
        }
        Ok(removed)
    }

    /// Get total number of stored memories.
    pub async fn count(&self) -> usize {
        self.entries.read().await.len()
    }

    /// Persist current entries to disk.
    async fn persist_locked(&self, entries: &[FileMemoryEntry]) -> Result<(), String> {
        let json = serde_json::to_string_pretty(entries)
            .map_err(|e| format!("serialize: {}", e))?;

        // Write to temp file first, then rename (atomic on most filesystems)
        let tmp_path = self.config.path.with_extension("json.tmp");
        tokio::fs::write(&tmp_path, json)
            .await
            .map_err(|e| format!("write {}: {}", tmp_path.display(), e))?;

        tokio::fs::rename(&tmp_path, &self.config.path)
            .await
            .map_err(|e| format!("rename: {}", e))?;

        Ok(())
    }
}

/// Simple whitespace tokenizer with lowercasing and punctuation removal.
fn tokenize(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|w| w.len() > 2)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    async fn test_store() -> FileMemoryStore {
        let tmp = NamedTempFile::new().unwrap();
        let config = FileMemoryConfig {
            path: tmp.path().to_path_buf(),
            max_entries: 100,
            min_relevance: 0.01,
        };
        FileMemoryStore::open(config).await.unwrap()
    }

    #[tokio::test]
    async fn remember_and_recall() {
        let store = test_store().await;

        store
            .remember(
                "Rust is a systems programming language",
                "UserSaved",
                vec!["programming".into()],
                serde_json::json!({}),
            )
            .await
            .unwrap();

        store
            .remember(
                "Python is great for data science",
                "Conversation",
                vec!["programming".into(), "data".into()],
                serde_json::json!({}),
            )
            .await
            .unwrap();

        let results = store.recall("Rust programming", 5).await;
        assert!(!results.is_empty());
        assert!(results[0].entry.content.contains("Rust"));
    }

    #[tokio::test]
    async fn forget_entry() {
        let store = test_store().await;

        let id = store
            .remember("temporary note", "UserSaved", vec![], serde_json::json!({}))
            .await
            .unwrap();

        assert_eq!(store.count().await, 1);
        assert!(store.forget(&id).await.unwrap());
        assert_eq!(store.count().await, 0);
    }

    #[tokio::test]
    async fn fifo_eviction() {
        let tmp = NamedTempFile::new().unwrap();
        let config = FileMemoryConfig {
            path: tmp.path().to_path_buf(),
            max_entries: 3,
            min_relevance: 0.01,
        };
        let store = FileMemoryStore::open(config).await.unwrap();

        for i in 0..5 {
            store
                .remember(
                    &format!("memory entry {}", i),
                    "System",
                    vec![],
                    serde_json::json!({}),
                )
                .await
                .unwrap();
        }

        assert_eq!(store.count().await, 3);
        // Oldest entries should have been evicted
        let results = store.recall("entry", 10).await;
        assert!(results.iter().all(|r| !r.entry.content.contains("entry 0")));
        assert!(results.iter().all(|r| !r.entry.content.contains("entry 1")));
    }

    #[tokio::test]
    async fn tag_boost() {
        let store = test_store().await;

        store
            .remember(
                "The weather is nice today",
                "Conversation",
                vec!["weather".into()],
                serde_json::json!({}),
            )
            .await
            .unwrap();

        store
            .remember(
                "I like sunny days",
                "UserSaved",
                vec!["preference".into()],
                serde_json::json!({}),
            )
            .await
            .unwrap();

        let results = store.recall("weather", 5).await;
        assert!(!results.is_empty());
        // The tagged entry should rank higher
        assert!(results[0].entry.content.contains("weather"));
    }

    #[tokio::test]
    async fn persistence_across_reloads() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path().to_path_buf();

        // Write
        {
            let config = FileMemoryConfig {
                path: path.clone(),
                max_entries: 100,
                min_relevance: 0.01,
            };
            let store = FileMemoryStore::open(config).await.unwrap();
            store
                .remember("persistent data", "UserSaved", vec![], serde_json::json!({}))
                .await
                .unwrap();
        }

        // Read back
        {
            let config = FileMemoryConfig {
                path,
                max_entries: 100,
                min_relevance: 0.01,
            };
            let store = FileMemoryStore::open(config).await.unwrap();
            assert_eq!(store.count().await, 1);
            let results = store.recall("persistent", 5).await;
            assert_eq!(results.len(), 1);
            assert!(results[0].entry.content.contains("persistent"));
        }
    }
}
