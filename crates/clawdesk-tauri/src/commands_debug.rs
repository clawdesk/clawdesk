//! Debug commands for diagnosing persistence and event flow issues.
//!
//! When debug mode is enabled, every persistence-related operation emits
//! a `debug:event` Tauri event to the frontend so the user can trace
//! exactly what happens to their data in real-time.

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use tauri::{AppHandle, Emitter, Manager, State};

/// Global debug mode flag — checked by instrumentation points.
static DEBUG_MODE: AtomicBool = AtomicBool::new(false);

/// Check if debug mode is currently enabled.
pub fn is_debug_enabled() -> bool {
    DEBUG_MODE.load(Ordering::Relaxed)
}

/// Emit a debug event to the frontend (no-op if debug mode is off).
pub fn emit_debug(app: &AppHandle, event: DebugEvent) {
    if !DEBUG_MODE.load(Ordering::Relaxed) {
        return;
    }
    let _ = app.emit("debug:event", &event);
}

/// A single debug event payload, emitted to the frontend as `debug:event`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugEvent {
    /// Monotonic timestamp (ms since Unix epoch)
    pub ts: u64,
    /// Category: "persist", "hydrate", "sochdb", "session", "shutdown", etc.
    pub category: String,
    /// Concise action label
    pub action: String,
    /// Human-readable detail string
    pub detail: String,
    /// Severity level: "info", "warn", "error"
    pub level: String,
}

impl DebugEvent {
    pub fn info(category: &str, action: &str, detail: impl Into<String>) -> Self {
        Self {
            ts: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
            category: category.to_string(),
            action: action.to_string(),
            detail: detail.into(),
            level: "info".to_string(),
        }
    }
    pub fn warn(category: &str, action: &str, detail: impl Into<String>) -> Self {
        let mut e = Self::info(category, action, detail);
        e.level = "warn".to_string();
        e
    }
    pub fn error(category: &str, action: &str, detail: impl Into<String>) -> Self {
        let mut e = Self::info(category, action, detail);
        e.level = "error".to_string();
        e
    }
}

// ═══════════════════════════════════════════════════════════
// Tauri commands
// ═══════════════════════════════════════════════════════════

/// Toggle debug mode on/off. Returns the new state.
#[tauri::command]
pub async fn toggle_debug_mode(
    enabled: bool,
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<bool, String> {
    let prev = DEBUG_MODE.swap(enabled, Ordering::SeqCst);
    tracing::info!(prev = prev, now = enabled, "Debug mode toggled");
    if enabled {
        // Emit an initial snapshot so the user sees something immediately
        let _ = app.emit(
            "debug:event",
            &DebugEvent::info(
                "system",
                "debug_enabled",
                "Debug mode enabled — persistence events will now be captured.",
            ),
        );
    }
    Ok(enabled)
}

/// Get current debug mode status.
#[tauri::command]
pub async fn get_debug_mode() -> Result<bool, String> {
    Ok(DEBUG_MODE.load(Ordering::Relaxed))
}

/// Comprehensive storage diagnostic snapshot.
///
/// Compares in-memory hot cache with what's actually stored in SochDB,
/// checks WAL health, and reports any mismatches.
#[derive(Debug, Serialize)]
pub struct StorageSnapshot {
    /// Is SochDB running in ephemeral (in-memory only) mode?
    pub is_ephemeral: bool,
    /// Path to the SochDB database on disk
    pub storage_path: String,
    /// Number of sessions in the in-memory hot cache
    pub memory_session_count: usize,
    /// Number of sessions found in SochDB (on-disk)
    pub sochdb_session_count: usize,
    /// Session IDs that are in memory but NOT in SochDB (data loss!)
    pub memory_only_sessions: Vec<String>,
    /// Session IDs that are in SochDB but NOT in memory (hydration failure!)
    pub sochdb_only_sessions: Vec<String>,
    /// Sessions that exist in both but have different message counts
    pub message_count_mismatches: Vec<SessionMismatch>,
    /// Number of agents in memory
    pub memory_agent_count: usize,
    /// Number of agents in SochDB
    pub sochdb_agent_count: usize,
    /// WAL file size in bytes (if available)
    pub wal_size_bytes: u64,
    /// Whether the WAL file exists
    pub wal_exists: bool,
    /// Old-format (chat_sessions/) entries still present
    pub old_format_session_count: usize,
    /// Any serialization test results
    pub roundtrip_test: String,
    /// Detailed per-session info
    pub session_details: Vec<SessionDetail>,
}

#[derive(Debug, Serialize)]
pub struct SessionMismatch {
    pub chat_id: String,
    pub memory_msg_count: usize,
    pub sochdb_msg_count: usize,
}

#[derive(Debug, Serialize)]
pub struct SessionDetail {
    pub chat_id: String,
    pub agent_id: String,
    pub title: String,
    pub message_count: usize,
    pub created_at: String,
    pub updated_at: String,
    /// Whether this session is in SochDB
    pub in_sochdb: bool,
    /// Whether this session is in memory
    pub in_memory: bool,
    /// Size of the serialized JSON for this session (bytes)
    pub serialized_size: usize,
}

#[tauri::command]
pub async fn debug_storage_snapshot(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<StorageSnapshot, String> {
    emit_debug(
        &app,
        DebugEvent::info("diagnostic", "snapshot_start", "Starting storage diagnostic snapshot..."),
    );

    let is_ephemeral = state.soch_store.is_ephemeral();
    let storage_path = state.soch_store.store_path().display().to_string();

    // ── Read in-memory sessions ──
    let memory_sessions: Vec<(String, crate::state::ChatSession)> = state.sessions.entries();
    let memory_session_count = memory_sessions.len();

    // ── Read agents from memory ──
    let memory_agent_count = {
        let agents = state.agents.read().map_err(|e| e.to_string())?;
        agents.len()
    };

    // ── Scan SochDB for sessions ──
    let sochdb_sessions: Vec<(String, Option<crate::state::ChatSession>)> = match state
        .soch_store
        .scan("chats/")
    {
        Ok(entries) => entries
            .into_iter()
            .map(|(key, value)| {
                let id = key
                    .strip_prefix("chats/")
                    .unwrap_or(&key)
                    .to_string();
                let parsed = serde_json::from_slice::<crate::state::ChatSession>(&value).ok();
                (id, parsed)
            })
            .collect(),
        Err(e) => {
            emit_debug(
                &app,
                DebugEvent::error(
                    "diagnostic",
                    "sochdb_scan_failed",
                    format!("Failed to scan SochDB for chats: {}", e),
                ),
            );
            Vec::new()
        }
    };
    let sochdb_session_count = sochdb_sessions.len();

    // ── Scan SochDB for agents ──
    let sochdb_agent_count = match state.soch_store.scan("agents/") {
        Ok(entries) => entries.len(),
        Err(_) => 0,
    };

    // ── Check old-format sessions ──
    let old_format_session_count = match state.soch_store.scan("chat_sessions/") {
        Ok(entries) => entries.len(),
        Err(_) => 0,
    };

    // ── Find mismatches ──
    let memory_ids: std::collections::HashSet<String> =
        memory_sessions.iter().map(|(id, _)| id.clone()).collect();
    let sochdb_ids: std::collections::HashSet<String> =
        sochdb_sessions.iter().map(|(id, _)| id.clone()).collect();

    let memory_only_sessions: Vec<String> = memory_ids.difference(&sochdb_ids).cloned().collect();
    let sochdb_only_sessions: Vec<String> = sochdb_ids.difference(&memory_ids).cloned().collect();

    // ── Message count mismatches ──
    let mut message_count_mismatches = Vec::new();
    let sochdb_map: std::collections::HashMap<String, Option<crate::state::ChatSession>> =
        sochdb_sessions.iter().cloned().collect();
    for (id, mem_session) in &memory_sessions {
        if let Some(Some(sochdb_session)) = sochdb_map.get(id) {
            if mem_session.messages.len() != sochdb_session.messages.len() {
                message_count_mismatches.push(SessionMismatch {
                    chat_id: id.clone(),
                    memory_msg_count: mem_session.messages.len(),
                    sochdb_msg_count: sochdb_session.messages.len(),
                });
            }
        }
    }

    // ── WAL file check ──
    let wal_path = std::path::PathBuf::from(&storage_path).join("wal.log");
    let wal_exists = wal_path.exists();
    let wal_size_bytes = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);

    // ── Roundtrip serialization test ──
    let roundtrip_test = {
        let test_session = crate::state::ChatSession {
            id: "__debug_test__".to_string(),
            agent_id: "test".to_string(),
            title: "Debug roundtrip test".to_string(),
            messages: vec![crate::state::ChatMessage {
                id: "test-msg-1".to_string(),
                role: "user".to_string(),
                content: "Hello debug".to_string(),
                timestamp: chrono::Utc::now().to_rfc3339(),
                metadata: None,
            }],
            created_at: chrono::Utc::now().to_rfc3339(),
            updated_at: chrono::Utc::now().to_rfc3339(),
        };
        match serde_json::to_vec(&test_session) {
            Ok(bytes) => {
                match serde_json::from_slice::<crate::state::ChatSession>(&bytes) {
                    Ok(parsed) => {
                        if parsed.messages.len() == 1 && parsed.id == "__debug_test__" {
                            "PASS: serialize → deserialize roundtrip OK".to_string()
                        } else {
                            "FAIL: roundtrip produced different data".to_string()
                        }
                    }
                    Err(e) => format!("FAIL: deserialize error: {}", e),
                }
            }
            Err(e) => format!("FAIL: serialize error: {}", e),
        }
    };

    // ── Build session details ──
    let mut session_details: Vec<SessionDetail> = Vec::new();
    // Add all memory sessions
    for (id, session) in &memory_sessions {
        let in_sochdb = sochdb_ids.contains(id);
        let serialized_size = serde_json::to_vec(session).map(|b| b.len()).unwrap_or(0);
        session_details.push(SessionDetail {
            chat_id: id.clone(),
            agent_id: session.agent_id.clone(),
            title: session.title.clone(),
            message_count: session.messages.len(),
            created_at: session.created_at.clone(),
            updated_at: session.updated_at.clone(),
            in_sochdb,
            in_memory: true,
            serialized_size,
        });
    }
    // Add SochDB-only sessions
    for (id, session_opt) in &sochdb_sessions {
        if memory_ids.contains(id) {
            continue; // already added
        }
        if let Some(session) = session_opt {
            session_details.push(SessionDetail {
                chat_id: id.clone(),
                agent_id: session.agent_id.clone(),
                title: session.title.clone(),
                message_count: session.messages.len(),
                created_at: session.created_at.clone(),
                updated_at: session.updated_at.clone(),
                in_sochdb: true,
                in_memory: false,
                serialized_size: 0,
            });
        } else {
            session_details.push(SessionDetail {
                chat_id: id.clone(),
                agent_id: "???".to_string(),
                title: "(deserialization failed)".to_string(),
                message_count: 0,
                created_at: String::new(),
                updated_at: String::new(),
                in_sochdb: true,
                in_memory: false,
                serialized_size: 0,
            });
        }
    }
    session_details.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));

    let snapshot = StorageSnapshot {
        is_ephemeral,
        storage_path,
        memory_session_count,
        sochdb_session_count,
        memory_only_sessions,
        sochdb_only_sessions,
        message_count_mismatches,
        memory_agent_count,
        sochdb_agent_count,
        wal_size_bytes,
        wal_exists,
        old_format_session_count,
        roundtrip_test,
        session_details,
    };

    // Emit summary
    let summary = format!(
        "Memory: {} sessions, {} agents | SochDB: {} sessions, {} agents | \
         Memory-only: {} | SochDB-only: {} | Mismatches: {} | \
         WAL: {} ({} bytes) | Ephemeral: {} | Old-format: {} | Roundtrip: {}",
        snapshot.memory_session_count,
        snapshot.memory_agent_count,
        snapshot.sochdb_session_count,
        snapshot.sochdb_agent_count,
        snapshot.memory_only_sessions.len(),
        snapshot.sochdb_only_sessions.len(),
        snapshot.message_count_mismatches.len(),
        if snapshot.wal_exists { "exists" } else { "missing" },
        snapshot.wal_size_bytes,
        snapshot.is_ephemeral,
        snapshot.old_format_session_count,
        snapshot.roundtrip_test,
    );
    emit_debug(
        &app,
        DebugEvent::info("diagnostic", "snapshot_complete", &summary),
    );

    // Log critical findings
    if snapshot.is_ephemeral {
        emit_debug(
            &app,
            DebugEvent::error(
                "diagnostic",
                "ephemeral_storage",
                "SochDB is running in EPHEMERAL mode! All data will be lost on restart. \
                 Check disk permissions at the storage path.",
            ),
        );
    }
    if !snapshot.memory_only_sessions.is_empty() {
        emit_debug(
            &app,
            DebugEvent::error(
                "diagnostic",
                "unpersisted_sessions",
                format!(
                    "{} sessions in memory but NOT in SochDB — data loss risk: {:?}",
                    snapshot.memory_only_sessions.len(),
                    snapshot.memory_only_sessions
                ),
            ),
        );
    }
    if !snapshot.sochdb_only_sessions.is_empty() {
        emit_debug(
            &app,
            DebugEvent::warn(
                "diagnostic",
                "orphaned_sessions",
                format!(
                    "{} sessions in SochDB but NOT in memory — possible hydration failure: {:?}",
                    snapshot.sochdb_only_sessions.len(),
                    snapshot.sochdb_only_sessions
                ),
            ),
        );
    }
    for m in &snapshot.message_count_mismatches {
        emit_debug(
            &app,
            DebugEvent::warn(
                "diagnostic",
                "msg_count_mismatch",
                format!(
                    "Chat {} — memory has {} msgs, SochDB has {} msgs",
                    m.chat_id, m.memory_msg_count, m.sochdb_msg_count
                ),
            ),
        );
    }

    Ok(snapshot)
}

/// Force-persist all in-memory state to SochDB and return confirmation.
/// Useful for debugging to verify the persist() path works.
#[tauri::command]
pub async fn debug_force_persist(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
    emit_debug(
        &app,
        DebugEvent::info("persist", "force_persist_start", "Manually triggered force persist"),
    );

    state.persist();

    // Verify by reading back
    let mem_count = state.sessions.len();
    let sochdb_count = state.soch_store.scan("chats/").map(|e| e.len()).unwrap_or(0);

    let result = format!(
        "Force persist complete. Memory: {} sessions, SochDB: {} sessions.",
        mem_count, sochdb_count
    );
    emit_debug(
        &app,
        DebugEvent::info("persist", "force_persist_done", &result),
    );
    Ok(result)
}

/// Re-hydrate sessions from SochDB (replacing in-memory state).
/// WARNING: This overwrites the hot cache with whatever is in SochDB.
#[tauri::command]
pub async fn debug_rehydrate(
    state: State<'_, AppState>,
    app: AppHandle,
) -> Result<String, String> {
    emit_debug(
        &app,
        DebugEvent::info("hydrate", "rehydrate_start", "Manually triggered re-hydration from SochDB"),
    );

    let new_sessions: std::collections::HashMap<String, crate::state::ChatSession> =
        crate::state::hydrate_map(&state.soch_store, "chats/");

    let count = new_sessions.len();
    state.sessions.clear();
    state.sessions.load_bulk(new_sessions);

    let result = format!("Re-hydrated {} sessions from SochDB.", count);
    emit_debug(
        &app,
        DebugEvent::info("hydrate", "rehydrate_done", &result),
    );
    Ok(result)
}
