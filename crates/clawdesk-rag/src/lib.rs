//! ClawDesk RAG — Document ingestion, chunking, and retrieval-augmented generation.
//!
//! Uses SochDB for persistent storage of document metadata, chunk text,
//! and vector embeddings. Supports PDF, Text, Markdown, and CSV.
//!
//! # Architecture
//!
//! ```text
//!   File upload  ──→  extract_text()  ──→  chunk_text()
//!        │                                      │
//!        │                                      ▼
//!        │                              RagStore.add_document()
//!        │                              (SochDB KV: metadata + chunks)
//!        │
//!        └──→  embedding provider ──→  RagStore.store_embeddings()
//!                                      (SochDB vectors)
//!
//!   Query  ──→  keyword_search()  ──→  RagSearchResult[]
//!          ──→  (vector search via SochDB VectorStore trait)
//! ```

pub mod chunking;
pub mod extract;
pub mod store;

pub use chunking::{chunk_text, ChunkConfig, TextChunk};
pub use extract::{extract_text, DocType};
pub use store::{RagDocument, RagSearchResult, RagStore, RAG_COLLECTION};

use clawdesk_sochdb::SochStore;
use std::path::Path;
use std::sync::Arc;
use tracing::info;

/// High-level RAG manager that combines extraction, chunking, and storage.
pub struct RagManager {
    pub store: RagStore,
    chunk_config: ChunkConfig,
}

impl RagManager {
    /// Create a new RAG manager backed by the shared SochDB instance.
    pub fn new(soch: Arc<SochStore>) -> Self {
        Self {
            store: RagStore::new(soch),
            chunk_config: ChunkConfig::default(),
        }
    }

    /// Ingest a file: extract text → chunk → store in SochDB.
    /// Returns the document ID and chunk count.
    pub fn ingest_file(&self, file_path: &Path) -> Result<(String, usize), String> {
        // Validate file exists and is supported
        if !file_path.exists() {
            return Err(format!("File not found: {}", file_path.display()));
        }
        let doc_type = DocType::from_path(file_path)
            .ok_or_else(|| format!("Unsupported file type: {}", file_path.display()))?;

        let filename = file_path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("unknown")
            .to_string();

        let size_bytes = std::fs::metadata(file_path)
            .map(|m| m.len())
            .unwrap_or(0);

        // Extract text
        let text = extract_text(file_path)?;
        let word_count = text.split_whitespace().count();

        // Chunk
        let chunks = chunk_text(&text, &self.chunk_config);
        let chunk_count = chunks.len();

        if chunk_count == 0 {
            return Err("Document produced no text chunks".to_string());
        }

        // Create document record
        let doc_id = uuid::Uuid::new_v4().to_string();
        let doc = RagDocument {
            id: doc_id.clone(),
            filename: filename.clone(),
            file_path: file_path.display().to_string(),
            doc_type,
            size_bytes,
            word_count,
            chunk_count,
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        // Store in SochDB
        self.store.add_document(doc, chunks)?;

        info!(
            doc_id = doc_id.as_str(),
            filename = filename.as_str(),
            chunk_count,
            word_count,
            "document ingested"
        );

        Ok((doc_id, chunk_count))
    }

    /// Remove a document from the RAG store.
    pub fn remove_document(&self, doc_id: &str) -> Result<(), String> {
        // Get chunk count for vector cleanup
        let doc = self.store.get_document(doc_id)?;
        if let Some(doc) = doc {
            self.store.delete_embeddings(doc_id, doc.chunk_count)?;
        }
        self.store.remove_document(doc_id)
    }

    /// List all ingested documents.
    pub fn list_documents(&self) -> Result<Vec<RagDocument>, String> {
        self.store.list_documents()
    }

    /// Keyword search across all documents.
    pub fn search(&self, query: &str, top_k: usize) -> Result<Vec<RagSearchResult>, String> {
        self.store.keyword_search(query, top_k)
    }

    /// Build a RAG context string for prompt injection.
    pub fn build_context(&self, query: &str, top_k: usize, max_chars: usize) -> Result<String, String> {
        let results = self.search(query, top_k)?;
        Ok(RagStore::build_context(&results, max_chars))
    }
}
