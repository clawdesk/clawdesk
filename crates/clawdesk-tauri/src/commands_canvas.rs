//! Canvas workspace commands — block-based content organization.

use crate::canvas::{Block, BlockMetadata, BlockType, Canvas, Connection, Position, Size};
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

// ═══════════════════════════════════════════════════════════
// Serializable types for the frontend
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Clone, Serialize)]
pub struct CanvasSummary {
    pub id: String,
    pub title: String,
    pub block_count: usize,
    pub connection_count: usize,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateCanvasRequest {
    pub title: String,
}

#[derive(Debug, Deserialize)]
pub struct AddBlockRequest {
    pub canvas_id: String,
    pub block_type: String,
    pub content: String,
    pub x: f64,
    pub y: f64,
    pub language: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ConnectBlocksRequest {
    pub canvas_id: String,
    pub from_block: String,
    pub to_block: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BlockInfo {
    pub id: String,
    pub block_type: String,
    pub content: String,
    pub x: f64,
    pub y: f64,
    pub language: Option<String>,
    pub editable: bool,
    pub pinned: bool,
    pub tags: Vec<String>,
}

fn block_to_info(b: &Block) -> BlockInfo {
    BlockInfo {
        id: b.id.clone(),
        block_type: b.block_type.label().to_string(),
        content: b.content.clone(),
        x: b.position.x,
        y: b.position.y,
        language: b.metadata.language.clone(),
        editable: b.metadata.editable,
        pinned: b.metadata.pinned,
        tags: b.metadata.tags.clone(),
    }
}

fn parse_block_type(s: &str) -> BlockType {
    match s.to_lowercase().as_str() {
        "code" => BlockType::Code,
        "markdown" => BlockType::Markdown,
        "image" => BlockType::Image,
        "chat" => BlockType::Chat,
        "note" => BlockType::Note,
        "table" => BlockType::Table,
        "diagram" => BlockType::Diagram,
        _ => BlockType::Text,
    }
}

// ═══════════════════════════════════════════════════════════
// Commands
// ═══════════════════════════════════════════════════════════

#[tauri::command]
pub async fn create_canvas(
    request: CreateCanvasRequest,
    state: State<'_, AppState>,
) -> Result<CanvasSummary, String> {
    let canvas = Canvas::new(&request.title);
    let summary = CanvasSummary {
        id: canvas.id.clone(),
        title: canvas.title.clone(),
        block_count: 0,
        connection_count: 0,
        created_at: canvas.created_at.clone(),
        updated_at: canvas.updated_at.clone(),
    };
    let mut canvases = state.canvases.write().map_err(|e| e.to_string())?;
    // Write-through to SochDB
    state.persist_canvas(&canvas);
    canvases.insert(canvas.id.clone(), canvas);
    Ok(summary)
}

#[tauri::command]
pub async fn get_canvas(
    canvas_id: String,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let canvases = state.canvases.read().map_err(|e| e.to_string())?;
    let canvas = canvases.get(&canvas_id)
        .ok_or_else(|| format!("Canvas {} not found", canvas_id))?;
    serde_json::to_value(canvas).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn list_canvases(state: State<'_, AppState>) -> Result<Vec<CanvasSummary>, String> {
    let canvases = state.canvases.read().map_err(|e| e.to_string())?;
    Ok(canvases.values().map(|c| CanvasSummary {
        id: c.id.clone(),
        title: c.title.clone(),
        block_count: c.block_count(),
        connection_count: c.connections.len(),
        created_at: c.created_at.clone(),
        updated_at: c.updated_at.clone(),
    }).collect())
}

#[tauri::command]
pub async fn add_canvas_block(
    request: AddBlockRequest,
    state: State<'_, AppState>,
) -> Result<BlockInfo, String> {
    let mut canvases = state.canvases.write().map_err(|e| e.to_string())?;
    let canvas = canvases.get_mut(&request.canvas_id)
        .ok_or_else(|| format!("Canvas {} not found", request.canvas_id))?;

    let block_type = parse_block_type(&request.block_type);
    let block = if block_type == BlockType::Code {
        Block::code(&request.content, request.language.as_deref().unwrap_or("rust"), request.x, request.y)
    } else {
        Block::new(block_type, &request.content, request.x, request.y)
    };
    let info = block_to_info(&block);
    canvas.add_block(block);
    // Write-through to SochDB
    state.persist_canvas(canvas);
    Ok(info)
}

#[tauri::command]
pub async fn remove_canvas_block(
    canvas_id: String,
    block_id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let mut canvases = state.canvases.write().map_err(|e| e.to_string())?;
    let canvas = canvases.get_mut(&canvas_id)
        .ok_or_else(|| format!("Canvas {} not found", canvas_id))?;
    let removed = canvas.remove_block(&block_id).is_some();
    if removed {
        // Write-through to SochDB
        state.persist_canvas(canvas);
    }
    Ok(removed)
}

#[tauri::command]
pub async fn connect_canvas_blocks(
    request: ConnectBlocksRequest,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let mut canvases = state.canvases.write().map_err(|e| e.to_string())?;
    let canvas = canvases.get_mut(&request.canvas_id)
        .ok_or_else(|| format!("Canvas {} not found", request.canvas_id))?;
    canvas.connect(&request.from_block, &request.to_block, request.label);
    // Write-through to SochDB
    state.persist_canvas(canvas);
    Ok(true)
}

#[tauri::command]
pub async fn export_canvas_markdown(
    canvas_id: String,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let canvases = state.canvases.read().map_err(|e| e.to_string())?;
    let canvas = canvases.get(&canvas_id)
        .ok_or_else(|| format!("Canvas {} not found", canvas_id))?;
    Ok(canvas.to_markdown())
}
