//! Memory commands — remember, recall, forget backed by SochDB vector store.
//!
//! Uses `MemoryManager<SochStore>` for:
//! - **remember**: Embed text + store in HNSW vector index via SochDB
//! - **recall**: Hybrid search (vector + BM25 RRF fusion) with min-relevance filter
//! - **forget**: Delete a memory by ID from the vector collection
//! - **search_memories**: Recall with explicit parameters
//!
//! All operations use SochDB's VectorStore trait implementation (HNSW + SIMD).

use crate::state::AppState;
use clawdesk_memory::manager::MemorySource;
use serde::{Deserialize, Serialize};
use tauri::State;

// ═══════════════════════════════════════════════════════════
// Request / Response types
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct RememberRequest {
    pub content: String,
    pub source: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct RememberResponse {
    pub id: String,
    pub content_length: usize,
}

#[derive(Debug, Deserialize)]
pub struct RecallRequest {
    pub query: String,
    pub max_results: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MemoryHit {
    pub id: String,
    pub score: f32,
    pub content: Option<String>,
    pub source: Option<String>,
    pub timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ForgetRequest {
    pub id: String,
}

#[derive(Debug, Serialize)]
pub struct MemoryStatsResponse {
    pub collection_name: String,
    pub embedding_provider: String,
    pub search_strategy: String,
    pub min_relevance: f32,
    pub max_results: usize,
}

#[derive(Debug, Deserialize)]
pub struct RememberBatchRequest {
    pub items: Vec<RememberBatchItem>,
}

#[derive(Debug, Deserialize)]
pub struct RememberBatchItem {
    pub content: String,
    pub source: Option<String>,
    pub metadata: Option<serde_json::Value>,
}

// ═══════════════════════════════════════════════════════════
// Commands
// ═══════════════════════════════════════════════════════════

/// Store a memory with automatic embedding into SochDB's HNSW vector index.
#[tauri::command]
pub async fn remember_memory(
    request: RememberRequest,
    state: State<'_, AppState>,
) -> Result<RememberResponse, String> {
    let source = parse_source(request.source.as_deref());
    let metadata = request.metadata.unwrap_or(serde_json::json!({}));
    let content_length = request.content.len();

    let id = state
        .memory
        .remember(&request.content, source, metadata)
        .await?;

    Ok(RememberResponse { id, content_length })
}

/// Batch-store multiple memories. Embeddings are computed in batch for efficiency.
#[tauri::command]
pub async fn remember_batch(
    request: RememberBatchRequest,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let items: Vec<(String, MemorySource, serde_json::Value)> = request
        .items
        .into_iter()
        .map(|item| {
            let source = parse_source(item.source.as_deref());
            let metadata = item.metadata.unwrap_or(serde_json::json!({}));
            (item.content, source, metadata)
        })
        .collect();

    state.memory.remember_batch(items).await
}

/// Recall relevant memories using hybrid search (vector + BM25 RRF fusion).
#[tauri::command]
pub async fn recall_memories(
    request: RecallRequest,
    state: State<'_, AppState>,
) -> Result<Vec<MemoryHit>, String> {
    let results = state
        .memory
        .recall(&request.query, request.max_results)
        .await?;

    Ok(results
        .into_iter()
        .map(|r| MemoryHit {
            id: r.id,
            score: r.score,
            content: r.content.or_else(|| {
                r.metadata.get("content").and_then(|v| v.as_str()).map(|s| s.to_string())
            }),
            source: r.metadata.get("source").and_then(|v| v.as_str()).map(|s| s.to_string()),
            timestamp: r.metadata.get("timestamp").and_then(|v| v.as_str()).map(|s| s.to_string()),
        })
        .collect())
}

/// Forget (delete) a specific memory by ID.
#[tauri::command]
pub async fn forget_memory(
    request: ForgetRequest,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    state.memory.forget(&request.id).await
}

/// Get memory system configuration / stats.
#[tauri::command]
pub async fn get_memory_stats(
    state: State<'_, AppState>,
) -> Result<MemoryStatsResponse, String> {
    // Report the config used to create the MemoryManager
    Ok(MemoryStatsResponse {
        collection_name: "memories".to_string(),
        embedding_provider: if std::env::var("OPENAI_API_KEY").is_ok() {
            "openai/text-embedding-3-small".to_string()
        } else {
            "ollama/nomic-embed-text".to_string()
        },
        search_strategy: "Hybrid (vector + BM25 RRF)".to_string(),
        min_relevance: 0.3,
        max_results: 10,
    })
}

// ═══════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════

fn parse_source(s: Option<&str>) -> MemorySource {
    match s {
        Some("conversation") => MemorySource::Conversation,
        Some("document") => MemorySource::Document,
        Some("user") | Some("saved") => MemorySource::UserSaved,
        Some("plugin") => MemorySource::Plugin,
        Some("system") => MemorySource::System,
        _ => MemorySource::UserSaved,
    }
}
