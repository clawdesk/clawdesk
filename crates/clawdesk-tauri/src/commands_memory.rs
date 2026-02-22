//! Memory commands — remember, recall, forget backed by SochDB MemoryBackend.
//!
//! Uses `MemoryManager<SochMemoryBackend>` for:
//! - **remember**: Embed text + atomically store in HNSW vector index + knowledge graph
//! - **recall**: Graph-contextual hybrid search (vector + BM25 RRF fusion) with temporal pre-filter
//! - **forget**: Delete a memory by ID from the vector collection + invalidate temporal edges
//! - **search_memories**: Recall with explicit parameters
//!
//! All operations use SochDB's MemoryBackend implementation (atomic writes, graph, temporal, policy, traces).

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
    _state: State<'_, AppState>,
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

// ═══════════════════════════════════════════════════════════
// Episode / Event / Entity commands (A4 — Memory Schema)
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct CreateEpisodeRequest {
    pub episode_id: String,
    pub episode_type: String,
    pub summary: String,
    pub tags: Option<Vec<String>>,
    pub entity_ids: Option<Vec<String>>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
pub struct SearchEpisodesRequest {
    pub query: String,
    pub max_results: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct AppendEventRequest {
    pub episode_id: String,
    pub seq: u64,
    pub role: String,
    pub tool_name: Option<String>,
    pub input_toon: String,
    pub output_toon: String,
    pub error: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GetTimelineRequest {
    pub episode_id: String,
    pub max_events: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct UpsertEntityRequest {
    pub entity_id: String,
    pub name: String,
    pub kind: String,
    pub attributes: Option<std::collections::HashMap<String, serde_json::Value>>,
    pub metadata: Option<std::collections::HashMap<String, serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
pub struct SearchEntitiesRequest {
    pub kind: Option<String>,
    pub query: String,
    pub max_results: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct ContextQueryRequest {
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub token_budget: usize,
    pub sections: Vec<ContextSectionInput>,
    pub truncation: Option<String>,
    pub format: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ContextSectionInput {
    pub name: String,
    pub priority: i32,
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct EnqueueTaskRequest {
    pub queue_id: Option<String>,
    pub priority: i64,
    pub payload: String,
    pub delay_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct ClaimTaskRequest {
    pub queue_id: Option<String>,
    pub worker_id: String,
}

#[derive(Debug, Deserialize)]
pub struct AckTaskRequest {
    pub queue_id: Option<String>,
    pub task_id: String,
}

#[derive(Debug, Deserialize)]
pub struct NackTaskRequest {
    pub queue_id: Option<String>,
    pub task_id: String,
    pub delay_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct QueryViewRequest {
    pub view_name: String,
    pub filters: Option<std::collections::HashMap<String, serde_json::Value>>,
    pub limit: Option<usize>,
}

// ── Episode Commands ────────────────────────────────────────

/// Create a new episode (conversation session, task, workflow).
#[tauri::command]
pub async fn create_episode(
    request: CreateEpisodeRequest,
    state: State<'_, AppState>,
) -> Result<(), String> {
    use clawdesk_memory::{Episode, EpisodeType};

    let episode_type = match request.episode_type.as_str() {
        "conversation" => EpisodeType::Conversation,
        "task" => EpisodeType::Task,
        "workflow" => EpisodeType::Workflow,
        "debug" => EpisodeType::Debug,
        "agent_interaction" => EpisodeType::AgentInteraction,
        _ => EpisodeType::Other,
    };

    let now = chrono::Utc::now().timestamp_millis() as u64;
    let episode = Episode {
        episode_id: request.episode_id,
        episode_type,
        entity_ids: request.entity_ids.unwrap_or_default(),
        ts_start: now,
        ts_end: now,
        summary: request.summary,
        tags: request.tags.unwrap_or_default(),
        embedding: None,
        metadata: request.metadata
            .and_then(|v| v.as_object().cloned())
            .map(|obj| obj.into_iter().collect())
            .unwrap_or_default(),
    };

    state.memory.create_episode(&episode)
}

/// Get an episode by ID.
#[tauri::command]
pub async fn get_episode(
    episode_id: String,
    state: State<'_, AppState>,
) -> Result<Option<serde_json::Value>, String> {
    let ep = state.memory.get_episode(&episode_id)?;
    Ok(ep.map(|e| serde_json::to_value(e).unwrap_or_default()))
}

/// Search episodes by text query.
#[tauri::command]
pub async fn search_episodes(
    request: SearchEpisodesRequest,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let k = request.max_results.unwrap_or(10);
    let episodes = state.memory.search_episodes(&request.query, k)?;
    Ok(episodes.iter().filter_map(|e| serde_json::to_value(e).ok()).collect())
}

// ── Event Commands ──────────────────────────────────────────

/// Append an event to an episode's timeline.
#[tauri::command]
pub async fn append_event(
    request: AppendEventRequest,
    state: State<'_, AppState>,
) -> Result<(), String> {
    use clawdesk_memory::{Event, EventRole, EventMetrics};

    let role = match request.role.as_str() {
        "user" => EventRole::User,
        "assistant" => EventRole::Assistant,
        "system" => EventRole::System,
        "tool" => EventRole::Tool,
        _ => EventRole::User,
    };

    let now = chrono::Utc::now().timestamp_millis() as u64;
    let event = Event {
        episode_id: request.episode_id,
        seq: request.seq,
        ts: now,
        role,
        tool_name: request.tool_name,
        input_toon: request.input_toon,
        output_toon: request.output_toon,
        error: request.error,
        metrics: EventMetrics::default(),
    };

    state.memory.append_event(&event)
}

/// Get the timeline for an episode.
#[tauri::command]
pub async fn get_timeline(
    request: GetTimelineRequest,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let max = request.max_events.unwrap_or(100);
    let events = state.memory.get_timeline(&request.episode_id, max)?;
    Ok(events.iter().filter_map(|e| serde_json::to_value(e).ok()).collect())
}

// ── Entity Commands ─────────────────────────────────────────

/// Create or update an entity.
#[tauri::command]
pub async fn upsert_entity(
    request: UpsertEntityRequest,
    state: State<'_, AppState>,
) -> Result<(), String> {
    use clawdesk_memory::{Entity, EntityKind};

    let kind = match request.kind.as_str() {
        "user" => EntityKind::User,
        "organization" => EntityKind::Organization,
        "project" => EntityKind::Project,
        "document" => EntityKind::Document,
        "service" => EntityKind::Service,
        "agent" => EntityKind::Agent,
        _ => EntityKind::Custom,
    };

    let now = chrono::Utc::now().timestamp_millis() as u64;
    let entity = Entity {
        entity_id: request.entity_id,
        name: request.name,
        kind,
        attributes: request.attributes.unwrap_or_default(),
        embedding: None,
        metadata: request.metadata.unwrap_or_default(),
        created_at: now,
        updated_at: now,
    };

    state.memory.upsert_entity(&entity)
}

/// Get an entity by ID.
#[tauri::command]
pub async fn get_entity(
    entity_id: String,
    state: State<'_, AppState>,
) -> Result<Option<serde_json::Value>, String> {
    let entity = state.memory.get_entity(&entity_id)?;
    Ok(entity.map(|e| serde_json::to_value(e).unwrap_or_default()))
}

/// Search entities by kind and query.
#[tauri::command]
pub async fn search_entities(
    request: SearchEntitiesRequest,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    use clawdesk_memory::EntityKind;

    let kind = request.kind.map(|k| match k.as_str() {
        "user" => EntityKind::User,
        "organization" => EntityKind::Organization,
        "project" => EntityKind::Project,
        "document" => EntityKind::Document,
        "service" => EntityKind::Service,
        "agent" => EntityKind::Agent,
        _ => EntityKind::Custom,
    });

    let k = request.max_results.unwrap_or(10);
    let entities = state.memory.search_entities(kind, &request.query, k)?;
    Ok(entities.iter().filter_map(|e| serde_json::to_value(e).ok()).collect())
}

/// Get entity facts (entity + recent episodes + related entities).
#[tauri::command]
pub async fn get_entity_facts(
    entity_id: String,
    state: State<'_, AppState>,
) -> Result<Option<serde_json::Value>, String> {
    let facts = state.memory.get_entity_facts(&entity_id)?;
    Ok(facts.map(|f| serde_json::to_value(f).unwrap_or_default()))
}

// ── Context Query Command (A1) ──────────────────────────────

/// Build token-budgeted context for LLM prompts.
#[tauri::command]
pub async fn build_context(
    request: ContextQueryRequest,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    use clawdesk_memory::{ContextFormat, TruncationStrategy};

    let truncation = match request.truncation.as_deref() {
        Some("head_drop") => TruncationStrategy::HeadDrop,
        Some("proportional") => TruncationStrategy::Proportional,
        Some("strict") => TruncationStrategy::Strict,
        _ => TruncationStrategy::TailDrop,
    };

    let format = match request.format.as_deref() {
        Some("json") => ContextFormat::Json,
        Some("text") => ContextFormat::Text,
        Some("soch") => ContextFormat::Soch,
        _ => ContextFormat::Markdown,
    };

    let sections: Vec<(&str, i32, &str)> = request.sections.iter()
        .map(|s| (s.name.as_str(), s.priority, s.content.as_str()))
        .collect();

    let result = state.memory.build_context(
        request.session_id.as_deref(),
        request.agent_id.as_deref(),
        request.token_budget,
        sections,
        truncation,
        format,
    )?;

    serde_json::to_value(result).map_err(|e| format!("serialize: {e}"))
}

// ── Task Queue Commands (A8) ────────────────────────────────

/// Enqueue a background task.
#[tauri::command]
pub async fn enqueue_task(
    request: EnqueueTaskRequest,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let queue_id = request.queue_id.as_deref().unwrap_or("memory_maintenance");
    let payload = request.payload.into_bytes();

    let task = if let Some(delay) = request.delay_ms {
        state.memory.enqueue_delayed_task(queue_id, request.priority, payload, delay)?
    } else {
        state.memory.enqueue_task(queue_id, request.priority, payload)?
    };

    serde_json::to_value(task).map_err(|e| format!("serialize: {e}"))
}

/// Claim a task from the queue.
#[tauri::command]
pub async fn claim_task(
    request: ClaimTaskRequest,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let queue_id = request.queue_id.as_deref().unwrap_or("memory_maintenance");
    let result = state.memory.claim_task(queue_id, &request.worker_id)?;
    serde_json::to_value(result).map_err(|e| format!("serialize: {e}"))
}

/// Acknowledge successful task completion.
#[tauri::command]
pub async fn ack_task(
    request: AckTaskRequest,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let queue_id = request.queue_id.as_deref().unwrap_or("memory_maintenance");
    state.memory.ack_task(queue_id, &request.task_id)
}

/// Negative-acknowledge a task (return to queue).
#[tauri::command]
pub async fn nack_task(
    request: NackTaskRequest,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let queue_id = request.queue_id.as_deref().unwrap_or("memory_maintenance");
    state.memory.nack_task(queue_id, &request.task_id, request.delay_ms)
}

/// Get task queue statistics.
#[tauri::command]
pub async fn queue_stats(
    queue_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let qid = queue_id.as_deref().unwrap_or("memory_maintenance");
    let stats = state.memory.queue_stats(qid)?;
    serde_json::to_value(stats).map_err(|e| format!("serialize: {e}"))
}

// ── Predefined Views Commands (A5) ──────────────────────────

/// List available predefined views.
#[tauri::command]
pub async fn list_views(
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    Ok(state.memory.list_views())
}

/// Query a predefined view with optional filters.
#[tauri::command]
pub async fn query_view(
    request: QueryViewRequest,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let rows = state.memory.query_view(&request.view_name, request.filters, request.limit)?;
    Ok(rows.into_iter().map(|r| serde_json::to_value(r).unwrap_or_default()).collect())
}
