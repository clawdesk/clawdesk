//! RAG document store backed by SochDB.
//!
//! Uses SochDB's KV store for document metadata and chunk text,
//! and SochDB's vector store for chunk embeddings and similarity search.
//!
//! Key layout:
//!   rag/docs/{doc_id}                  → RagDocument JSON
//!   rag/chunks/{doc_id}/{chunk_index}  → chunk text (UTF-8)
//!
//! Vector collection: "rag_chunks" — stores chunk embeddings with metadata
//! containing doc_id, chunk_index, filename.

use crate::chunking::TextChunk;
use crate::extract::DocType;
use clawdesk_sochdb::SochStore;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{info, warn};

/// Collection name for RAG chunk embeddings in SochDB's vector store.
pub const RAG_COLLECTION: &str = "rag_chunks";

/// A document that has been ingested into the RAG system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagDocument {
    pub id: String,
    pub filename: String,
    pub file_path: String,
    pub doc_type: DocType,
    pub size_bytes: u64,
    pub word_count: usize,
    pub chunk_count: usize,
    pub created_at: String,
}

/// A search result from the RAG store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RagSearchResult {
    pub doc_id: String,
    pub filename: String,
    pub chunk_index: usize,
    pub chunk_text: String,
    pub similarity: f64,
}

/// RAG document store backed by SochDB.
pub struct RagStore {
    store: Arc<SochStore>,
}

impl RagStore {
    /// Create a new RAG store using the shared SochDB instance.
    pub fn new(store: Arc<SochStore>) -> Self {
        Self { store }
    }

    // ── Key helpers ──────────────────────────────────────────────

    fn doc_key(doc_id: &str) -> String {
        format!("rag/docs/{}", doc_id)
    }

    fn chunk_key(doc_id: &str, index: usize) -> String {
        format!("rag/chunks/{}/{:06}", doc_id, index)
    }

    fn chunk_prefix(doc_id: &str) -> String {
        format!("rag/chunks/{}/", doc_id)
    }

    fn vector_id(doc_id: &str, index: usize) -> String {
        format!("{}_{:06}", doc_id, index)
    }

    // ── CRUD ─────────────────────────────────────────────────────

    /// List all ingested documents.
    pub fn list_documents(&self) -> Result<Vec<RagDocument>, String> {
        let entries = self
            .store
            .scan("rag/docs/")
            .map_err(|e| format!("Failed to scan documents: {}", e))?;

        let mut docs = Vec::new();
        for (_key, bytes) in entries {
            match serde_json::from_slice::<RagDocument>(&bytes) {
                Ok(doc) => docs.push(doc),
                Err(e) => warn!(error = %e, "failed to parse stored document"),
            }
        }
        docs.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(docs)
    }

    /// Get a document by ID.
    pub fn get_document(&self, doc_id: &str) -> Result<Option<RagDocument>, String> {
        let key = Self::doc_key(doc_id);
        match self
            .store
            .get(&key)
            .map_err(|e| format!("Failed to get document: {}", e))?
        {
            Some(bytes) => {
                let doc: RagDocument = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("Failed to parse document: {}", e))?;
                Ok(Some(doc))
            }
            None => Ok(None),
        }
    }

    /// Ingest a document: store metadata and chunks in SochDB.
    pub fn add_document(
        &self,
        doc: RagDocument,
        chunks: Vec<TextChunk>,
    ) -> Result<(), String> {
        let doc_id = doc.id.clone();

        // Serialize document metadata
        let doc_bytes = serde_json::to_vec(&doc)
            .map_err(|e| format!("Failed to serialize document: {}", e))?;

        // Build a batch: doc metadata + all chunk texts
        let doc_key = Self::doc_key(&doc_id);
        let mut batch: Vec<(String, Vec<u8>)> = Vec::with_capacity(1 + chunks.len());
        batch.push((doc_key.clone(), doc_bytes));

        for chunk in &chunks {
            let ck = Self::chunk_key(&doc_id, chunk.index);
            batch.push((ck, chunk.text.as_bytes().to_vec()));
        }

        // Write atomically
        let refs: Vec<(&str, &[u8])> = batch.iter().map(|(k, v)| (k.as_str(), v.as_slice())).collect();
        self.store
            .put_batch(&refs)
            .map_err(|e| format!("Failed to store document: {}", e))?;

        info!(doc_id = doc_id.as_str(), chunks = chunks.len(), "document ingested into SochDB");
        Ok(())
    }

    /// Remove a document and all its chunks.
    pub fn remove_document(&self, doc_id: &str) -> Result<(), String> {
        // Delete the document metadata
        self.store
            .delete(&Self::doc_key(doc_id))
            .map_err(|e| format!("Failed to delete document: {}", e))?;

        // Delete all chunks
        self.store
            .delete_prefix(&Self::chunk_prefix(doc_id))
            .map_err(|e| format!("Failed to delete chunks: {}", e))?;

        info!(doc_id, "document removed from SochDB");
        Ok(())
    }

    /// Get all chunk texts for a document.
    pub fn get_chunks(&self, doc_id: &str) -> Result<Vec<String>, String> {
        let entries = self
            .store
            .scan(&Self::chunk_prefix(doc_id))
            .map_err(|e| format!("Failed to scan chunks: {}", e))?;

        let mut chunks: Vec<(String, String)> = entries
            .into_iter()
            .filter_map(|(key, bytes)| {
                String::from_utf8(bytes).ok().map(|text| (key, text))
            })
            .collect();

        // Sort by key (which encodes chunk index as zero-padded)
        chunks.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(chunks.into_iter().map(|(_, text)| text).collect())
    }

    // ── Vector operations ────────────────────────────────────────

    /// Store chunk embeddings in SochDB's vector store.
    /// Call this after generating embeddings via an embedding provider.
    pub async fn store_embeddings(
        &self,
        doc_id: &str,
        filename: &str,
        embeddings: Vec<(usize, Vec<f32>)>,
    ) -> Result<(), String> {
        for (chunk_index, embedding) in embeddings {
            let vec_id = Self::vector_id(doc_id, chunk_index);
            let metadata = serde_json::json!({
                "doc_id": doc_id,
                "chunk_index": chunk_index,
                "filename": filename,
            });

            // Store in SochDB's vector index
            let key = format!("vectors/{}/{}/data", RAG_COLLECTION, vec_id);
            let emb_bytes: Vec<u8> = embedding
                .iter()
                .flat_map(|f| f.to_le_bytes())
                .collect();
            self.store
                .put(&key, &emb_bytes)
                .map_err(|e| format!("Failed to store embedding: {}", e))?;

            let meta_key = format!("vectors/{}/{}/meta", RAG_COLLECTION, vec_id);
            let meta_bytes = serde_json::to_vec(&metadata)
                .map_err(|e| format!("Failed to serialize metadata: {}", e))?;
            self.store
                .put(&meta_key, &meta_bytes)
                .map_err(|e| format!("Failed to store metadata: {}", e))?;
        }

        info!(doc_id, "stored chunk embeddings in SochDB vector store");
        Ok(())
    }

    /// Delete all vector embeddings for a document.
    pub fn delete_embeddings(&self, doc_id: &str, chunk_count: usize) -> Result<(), String> {
        for i in 0..chunk_count {
            let vec_id = Self::vector_id(doc_id, i);
            let data_key = format!("vectors/{}/{}/data", RAG_COLLECTION, vec_id);
            let meta_key = format!("vectors/{}/{}/meta", RAG_COLLECTION, vec_id);
            let _ = self.store.delete(&data_key);
            let _ = self.store.delete(&meta_key);
        }
        Ok(())
    }

    /// Keyword search across all document chunks (no embeddings needed).
    pub fn keyword_search(&self, query: &str, top_k: usize) -> Result<Vec<RagSearchResult>, String> {
        let query_lower = query.to_lowercase();
        let terms: Vec<&str> = query_lower.split_whitespace().collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }

        let docs = self.list_documents()?;
        let mut results = Vec::new();

        for doc in &docs {
            let chunks = self.get_chunks(&doc.id)?;
            for (idx, chunk_text) in chunks.iter().enumerate() {
                let chunk_lower = chunk_text.to_lowercase();
                let matches: usize = terms.iter().filter(|t| chunk_lower.contains(*t)).count();
                if matches > 0 {
                    results.push(RagSearchResult {
                        doc_id: doc.id.clone(),
                        filename: doc.filename.clone(),
                        chunk_index: idx,
                        chunk_text: chunk_text.clone(),
                        similarity: matches as f64 / terms.len() as f64,
                    });
                }
            }
        }

        results.sort_by(|a, b| {
            b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(top_k);
        Ok(results)
    }

    /// Build RAG context string from search results for injecting into prompts.
    pub fn build_context(results: &[RagSearchResult], max_chars: usize) -> String {
        let mut ctx = String::new();
        let mut used = 0;

        for (i, r) in results.iter().enumerate() {
            let header = format!("\n--- [{}] {} (chunk {}) ---\n", i + 1, r.filename, r.chunk_index);
            let entry_len = header.len() + r.chunk_text.len();
            if used + entry_len > max_chars {
                break;
            }
            ctx.push_str(&header);
            ctx.push_str(&r.chunk_text);
            ctx.push('\n');
            used += entry_len + 1;
        }

        ctx
    }
}
