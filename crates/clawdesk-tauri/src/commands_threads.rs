//! Tauri IPC commands for the namespaced chat-thread store.
//!
//! Wraps `clawdesk_threads::ThreadStore` methods as `#[tauri::command]`
//! functions, following the same patterns as `commands_sochdb.rs`.
//!
//! ## Thread identifiers
//!
//! Thread and message IDs are `u128` internally but serialized as hex strings
//! across the IPC boundary (JSON cannot represent u128 natively). The frontend
//! sends/receives `String` IDs; this module converts in both directions.

use crate::state::AppState;
use clawdesk_threads::{Message, MessageRole, ThreadMeta, ThreadQuery, ThreadSummary, SortOrder};
use serde::{Deserialize, Serialize};
use tauri::State;

// ═══════════════════════════════════════════════════════════════════════════
// IPC-friendly types — hex string IDs instead of u128
// ═══════════════════════════════════════════════════════════════════════════

/// Thread metadata with string IDs for frontend consumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadMetaIpc {
    pub id: String,
    pub agent_id: String,
    pub title: String,
    pub created_at: String,
    pub updated_at: String,
    pub message_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub pinned: bool,
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

impl From<ThreadMeta> for ThreadMetaIpc {
    fn from(m: ThreadMeta) -> Self {
        Self {
            id: format!("{:032x}", m.id),
            agent_id: m.agent_id,
            title: m.title,
            created_at: m.created_at,
            updated_at: m.updated_at,
            message_count: m.message_count,
            model: m.model,
            pinned: m.pinned,
            archived: m.archived,
            tags: m.tags,
        }
    }
}

/// Thread summary with string ID.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadSummaryIpc {
    pub id: String,
    pub agent_id: String,
    pub title: String,
    pub updated_at: String,
    pub message_count: u64,
    pub pinned: bool,
    pub archived: bool,
}

impl From<ThreadSummary> for ThreadSummaryIpc {
    fn from(s: ThreadSummary) -> Self {
        Self {
            id: format!("{:032x}", s.id),
            agent_id: s.agent_id,
            title: s.title,
            updated_at: s.updated_at,
            message_count: s.message_count,
            pinned: s.pinned,
            archived: s.archived,
        }
    }
}

/// Chat message with string IDs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageIpc {
    pub id: String,
    pub thread_id: String,
    pub role: MessageRole,
    pub content: String,
    pub timestamp_us: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    pub has_attachment: bool,
}

impl From<Message> for MessageIpc {
    fn from(m: Message) -> Self {
        Self {
            id: format!("{:032x}", m.id),
            thread_id: format!("{:032x}", m.thread_id),
            role: m.role,
            content: m.content,
            timestamp_us: m.timestamp_us,
            metadata: m.metadata,
            has_attachment: m.has_attachment,
        }
    }
}

/// Thread store stats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreadStoreStats {
    pub total_threads: u64,
    pub total_messages: u64,
}

// ═══════════════════════════════════════════════════════════════════════════
// Helpers
// ═══════════════════════════════════════════════════════════════════════════

/// Parse a hex string into a u128 thread/message ID.
fn parse_id(hex: &str) -> Result<u128, String> {
    u128::from_str_radix(hex, 16).map_err(|e| format!("Invalid ID '{hex}': {e}"))
}

// ═══════════════════════════════════════════════════════════════════════════
// Thread CRUD commands
// ═══════════════════════════════════════════════════════════════════════════

/// Create a new chat thread.
#[tauri::command]
pub fn thread_create(
    state: State<'_, AppState>,
    agent_id: String,
    title: String,
    model: Option<String>,
) -> Result<ThreadMetaIpc, String> {
    state
        .thread_store
        .create_thread(&agent_id, &title, model.as_deref())
        .map(ThreadMetaIpc::from)
        .map_err(|e| format!("Failed to create thread: {e}"))
}

/// Get thread metadata by ID.
#[tauri::command]
pub fn thread_get(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<Option<ThreadMetaIpc>, String> {
    let id = parse_id(&thread_id)?;
    state
        .thread_store
        .get_thread(id)
        .map(|opt| opt.map(ThreadMetaIpc::from))
        .map_err(|e| format!("Failed to get thread: {e}"))
}

/// List threads with optional filtering, pagination, and sorting.
#[tauri::command]
pub fn thread_list(
    state: State<'_, AppState>,
    agent_id: Option<String>,
    include_archived: Option<bool>,
    limit: Option<usize>,
    offset: Option<usize>,
    sort: Option<String>,
) -> Result<Vec<ThreadSummaryIpc>, String> {
    let sort_order = match sort.as_deref() {
        Some("created_asc") => SortOrder::CreatedAsc,
        Some("created_desc") => SortOrder::CreatedDesc,
        _ => SortOrder::UpdatedDesc, // default
    };

    let query = ThreadQuery {
        agent_id,
        include_archived: include_archived.unwrap_or(false),
        limit,
        offset: offset.unwrap_or(0),
        sort: sort_order,
    };

    state
        .thread_store
        .list_threads(&query)
        .map(|threads| threads.into_iter().map(ThreadSummaryIpc::from).collect())
        .map_err(|e| format!("Failed to list threads: {e}"))
}

/// Update thread metadata (title, pinned, archived, tags, model).
#[tauri::command]
pub fn thread_update(
    state: State<'_, AppState>,
    thread_id: String,
    title: Option<String>,
    pinned: Option<bool>,
    archived: Option<bool>,
    tags: Option<Vec<String>>,
    model: Option<String>,
) -> Result<ThreadMetaIpc, String> {
    let id = parse_id(&thread_id)?;
    state
        .thread_store
        .update_thread(id, |meta| {
            if let Some(t) = title {
                meta.title = t;
            }
            if let Some(p) = pinned {
                meta.pinned = p;
            }
            if let Some(a) = archived {
                meta.archived = a;
            }
            if let Some(tg) = tags {
                meta.tags = tg;
            }
            if let Some(m) = model {
                meta.model = Some(m);
            }
        })
        .map(ThreadMetaIpc::from)
        .map_err(|e| format!("Failed to update thread: {e}"))
}

/// Delete a thread and all its messages (cascading delete).
#[tauri::command]
pub fn thread_delete(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<bool, String> {
    let id = parse_id(&thread_id)?;
    state
        .thread_store
        .delete_thread(id)
        .map_err(|e| format!("Failed to delete thread: {e}"))
}

// ═══════════════════════════════════════════════════════════════════════════
// Message commands
// ═══════════════════════════════════════════════════════════════════════════

/// Append a message to a thread.
#[tauri::command]
pub fn thread_append_message(
    state: State<'_, AppState>,
    thread_id: String,
    role: String,
    content: String,
    metadata: Option<serde_json::Value>,
) -> Result<MessageIpc, String> {
    let id = parse_id(&thread_id)?;
    let msg_role = match role.as_str() {
        "user" => MessageRole::User,
        "assistant" => MessageRole::Assistant,
        "system" => MessageRole::System,
        "tool" => MessageRole::Tool,
        other => return Err(format!("Invalid role '{other}': expected user|assistant|system|tool")),
    };

    state
        .thread_store
        .append_message(id, msg_role, &content, metadata)
        .map(MessageIpc::from)
        .map_err(|e| format!("Failed to append message: {e}"))
}

/// Get all messages in a thread (chronological order).
#[tauri::command]
pub fn thread_get_messages(
    state: State<'_, AppState>,
    thread_id: String,
) -> Result<Vec<MessageIpc>, String> {
    let id = parse_id(&thread_id)?;
    state
        .thread_store
        .get_thread_messages(id)
        .map(|msgs| msgs.into_iter().map(MessageIpc::from).collect())
        .map_err(|e| format!("Failed to get messages: {e}"))
}

/// Get the most recent N messages in a thread.
#[tauri::command]
pub fn thread_get_recent_messages(
    state: State<'_, AppState>,
    thread_id: String,
    limit: usize,
) -> Result<Vec<MessageIpc>, String> {
    let id = parse_id(&thread_id)?;
    state
        .thread_store
        .get_recent_messages(id, limit)
        .map(|msgs| msgs.into_iter().map(MessageIpc::from).collect())
        .map_err(|e| format!("Failed to get recent messages: {e}"))
}

/// Get messages in a time range within a thread.
#[tauri::command]
pub fn thread_get_messages_range(
    state: State<'_, AppState>,
    thread_id: String,
    from_us: u64,
    to_us: u64,
) -> Result<Vec<MessageIpc>, String> {
    let id = parse_id(&thread_id)?;
    state
        .thread_store
        .get_thread_messages_range(id, from_us, to_us)
        .map(|msgs| msgs.into_iter().map(MessageIpc::from).collect())
        .map_err(|e| format!("Failed to get messages in range: {e}"))
}

/// Delete a single message from a thread.
#[tauri::command]
pub fn thread_delete_message(
    state: State<'_, AppState>,
    thread_id: String,
    timestamp_us: u64,
    msg_id: String,
) -> Result<bool, String> {
    let tid = parse_id(&thread_id)?;
    let mid = parse_id(&msg_id)?;
    state
        .thread_store
        .delete_message(tid, timestamp_us, mid)
        .map_err(|e| format!("Failed to delete message: {e}"))
}

// ═══════════════════════════════════════════════════════════════════════════
// Attachment commands
// ═══════════════════════════════════════════════════════════════════════════

/// Append a message with a binary attachment.
///
/// The attachment is passed as a base64-encoded string from the frontend.
#[tauri::command]
pub fn thread_append_message_with_attachment(
    state: State<'_, AppState>,
    thread_id: String,
    role: String,
    content: String,
    metadata: Option<serde_json::Value>,
    attachment_base64: String,
) -> Result<MessageIpc, String> {
    let id = parse_id(&thread_id)?;
    let msg_role = match role.as_str() {
        "user" => MessageRole::User,
        "assistant" => MessageRole::Assistant,
        "system" => MessageRole::System,
        "tool" => MessageRole::Tool,
        other => return Err(format!("Invalid role '{other}'")),
    };

    use base64::Engine;
    let attachment = base64::engine::general_purpose::STANDARD
        .decode(&attachment_base64)
        .map_err(|e| format!("Invalid base64 attachment: {e}"))?;

    state
        .thread_store
        .append_message_with_attachment(id, msg_role, &content, metadata, &attachment)
        .map(MessageIpc::from)
        .map_err(|e| format!("Failed to append message with attachment: {e}"))
}

/// Get an attachment blob by message ID, returned as base64.
#[tauri::command]
pub fn thread_get_attachment(
    state: State<'_, AppState>,
    msg_id: String,
) -> Result<Option<String>, String> {
    let mid = parse_id(&msg_id)?;
    state
        .thread_store
        .get_attachment(mid)
        .map(|opt| {
            opt.map(|bytes| {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.encode(&bytes)
            })
        })
        .map_err(|e| format!("Failed to get attachment: {e}"))
}

// ═══════════════════════════════════════════════════════════════════════════
// Maintenance commands
// ═══════════════════════════════════════════════════════════════════════════

/// Force a checkpoint + GC on the thread store.
#[tauri::command]
pub fn thread_store_checkpoint(
    state: State<'_, AppState>,
) -> Result<u64, String> {
    state
        .thread_store
        .checkpoint_and_gc()
        .map_err(|e| format!("ThreadStore checkpoint failed: {e}"))
}

/// Force fsync on the thread store.
#[tauri::command]
pub fn thread_store_sync(
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .thread_store
        .sync()
        .map_err(|e| format!("ThreadStore sync failed: {e}"))
}

/// Get thread store statistics.
#[tauri::command]
pub fn thread_store_stats(
    state: State<'_, AppState>,
) -> Result<ThreadStoreStats, String> {
    let (total_threads, total_messages) = state
        .thread_store
        .stats()
        .map_err(|e| format!("ThreadStore stats failed: {e}"))?;
    Ok(ThreadStoreStats {
        total_threads,
        total_messages,
    })
}
