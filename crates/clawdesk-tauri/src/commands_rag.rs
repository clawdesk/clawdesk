//! Tauri IPC commands for RAG document management.
//!
//! Upload, list, delete, and search documents for retrieval-augmented generation.

use crate::state::AppState;
use clawdesk_rag::{RagDocument, RagSearchResult};
use serde::Deserialize;
use tauri::State;
use tracing::info;

// ── Ingest a document ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RagIngestRequest {
    pub file_path: String,
}

#[tauri::command]
pub async fn rag_ingest_document(
    request: RagIngestRequest,
    state: State<'_, AppState>,
) -> Result<RagDocument, String> {
    let path = std::path::PathBuf::from(&request.file_path);

    // Validate path safety: must be a real file, not a traversal
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("Invalid path: {}", e))?;
    if !canonical.is_file() {
        return Err("Path is not a file".to_string());
    }

    let guard = state.rag_manager.read().map_err(|e| e.to_string())?;
    let rag = guard.as_ref().ok_or("RAG manager not initialized")?;

    let (doc_id, chunk_count) = rag.ingest_file(&canonical)?;

    info!(doc_id = doc_id.as_str(), chunk_count, path = %canonical.display(), "document ingested via IPC");

    // Return the newly created document
    rag.store
        .get_document(&doc_id)?
        .ok_or("Document created but not found".to_string())
}

// ── List all documents ───────────────────────────────────────────────────

#[tauri::command]
pub async fn rag_list_documents(
    state: State<'_, AppState>,
) -> Result<Vec<RagDocument>, String> {
    let guard = state.rag_manager.read().map_err(|e| e.to_string())?;
    let rag = guard.as_ref().ok_or("RAG manager not initialized")?;
    rag.list_documents()
}

// ── Delete a document ────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RagDeleteRequest {
    pub doc_id: String,
}

#[tauri::command]
pub async fn rag_delete_document(
    request: RagDeleteRequest,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let guard = state.rag_manager.read().map_err(|e| e.to_string())?;
    let rag = guard.as_ref().ok_or("RAG manager not initialized")?;
    rag.remove_document(&request.doc_id)?;
    info!(doc_id = request.doc_id.as_str(), "document deleted via IPC");
    Ok(())
}

// ── Search documents ─────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RagSearchRequest {
    pub query: String,
    pub top_k: Option<usize>,
}

#[tauri::command]
pub async fn rag_search(
    request: RagSearchRequest,
    state: State<'_, AppState>,
) -> Result<Vec<RagSearchResult>, String> {
    let top_k = request.top_k.unwrap_or(5);
    let guard = state.rag_manager.read().map_err(|e| e.to_string())?;
    let rag = guard.as_ref().ok_or("RAG manager not initialized")?;
    rag.search(&request.query, top_k)
}

// ── Get document chunks ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RagGetChunksRequest {
    pub doc_id: String,
}

#[tauri::command]
pub async fn rag_get_chunks(
    request: RagGetChunksRequest,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let guard = state.rag_manager.read().map_err(|e| e.to_string())?;
    let rag = guard.as_ref().ok_or("RAG manager not initialized")?;
    rag.store.get_chunks(&request.doc_id)
}

// ── Build RAG context for prompt injection ───────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RagContextRequest {
    pub query: String,
    pub top_k: Option<usize>,
    pub max_chars: Option<usize>,
}

#[tauri::command]
pub async fn rag_build_context(
    request: RagContextRequest,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let top_k = request.top_k.unwrap_or(5);
    let max_chars = request.max_chars.unwrap_or(4000);
    let guard = state.rag_manager.read().map_err(|e| e.to_string())?;
    let rag = guard.as_ref().ok_or("RAG manager not initialized")?;
    rag.build_context(&request.query, top_k, max_chars)
}
