//! Infrastructure commands — notifications, clipboard, voice wake, idle (Tasks 20-22, 29).

use crate::state::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

// ═══════════════════════════════════════════════════════════
// Notifications
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct SendNotificationRequest {
    pub title: String,
    pub body: String,
    pub priority: Option<String>,
    pub group_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationInfo {
    pub id: String,
    pub title: String,
    pub body: String,
    pub priority: String,
    pub created_at: String,
}

#[tauri::command]
pub async fn send_notification(
    request: SendNotificationRequest,
    state: State<'_, AppState>,
) -> Result<NotificationInfo, String> {
    use clawdesk_infra::notifications::Notification;

    let mut notif = Notification::new(&request.title, &request.body);
    if let Some(ref group) = request.group_id {
        notif = notif.with_group(group);
    }
    let info = NotificationInfo {
        id: notif.id.clone(),
        title: notif.title.clone(),
        body: notif.body.clone(),
        priority: format!("{:?}", notif.priority),
        created_at: notif.created_at.to_rfc3339(),
    };
    // Store in notification history (hot cache + SochDB write-through)
    let mut history = state.notification_history.write().map_err(|e| e.to_string())?;
    history.push(info.clone());
    // Write-through to SochDB
    state.persist_notification(&info);
    // Keep last 500 notifications in hot cache
    if history.len() > 500 {
        let drain_count = history.len() - 500;
        history.drain(..drain_count);
    }
    Ok(info)
}

#[tauri::command]
pub async fn list_notifications(
    state: State<'_, AppState>,
) -> Result<Vec<NotificationInfo>, String> {
    let history = state.notification_history.read().map_err(|e| e.to_string())?;
    Ok(history.clone())
}

// ═══════════════════════════════════════════════════════════
// Clipboard
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Serialize)]
pub struct ClipboardEntryInfo {
    pub id: String,
    pub content_type: String,
    pub text: Option<String>,
    pub byte_size: usize,
    pub timestamp: String,
}

#[tauri::command]
pub async fn read_clipboard(
    state: State<'_, AppState>,
) -> Result<Option<ClipboardEntryInfo>, String> {
    let history = state.clipboard_history.read().map_err(|e| e.to_string())?;
    Ok(history.last().map(|e| ClipboardEntryInfo {
        id: e.id.clone(),
        content_type: format!("{:?}", e.content_type),
        text: e.text.clone(),
        byte_size: e.byte_size,
        timestamp: e.timestamp.to_rfc3339(),
    }))
}

#[tauri::command]
pub async fn write_clipboard(
    text: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    use clawdesk_infra::clipboard::ClipboardEntry;
    let entry = ClipboardEntry::text(&text);
    // Write-through to SochDB
    state.persist_clipboard_entry(&entry);
    let mut history = state.clipboard_history.write().map_err(|e| e.to_string())?;
    history.push(entry);
    // Keep last 100 entries in hot cache
    if history.len() > 100 {
        let drain_count = history.len() - 100;
        history.drain(..drain_count);
    }
    Ok(true)
}

#[tauri::command]
pub async fn get_clipboard_history(
    limit: Option<usize>,
    state: State<'_, AppState>,
) -> Result<Vec<ClipboardEntryInfo>, String> {
    let history = state.clipboard_history.read().map_err(|e| e.to_string())?;
    let n = limit.unwrap_or(20).min(history.len());
    Ok(history.iter().rev().take(n).map(|e| ClipboardEntryInfo {
        id: e.id.clone(),
        content_type: format!("{:?}", e.content_type),
        text: e.text.clone(),
        byte_size: e.byte_size,
        timestamp: e.timestamp.to_rfc3339(),
    }).collect())
}

// ═══════════════════════════════════════════════════════════
// Voice Wake
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Deserialize)]
pub struct VoiceWakeConfigRequest {
    pub enabled: bool,
    pub wake_phrases: Vec<String>,
    pub target_agent: Option<String>,
    pub silence_timeout_secs: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct VoiceWakeStatus {
    pub enabled: bool,
    pub wake_phrases: Vec<String>,
    pub target_agent: String,
    pub listening: bool,
}

#[tauri::command]
pub async fn configure_voice_wake(
    request: VoiceWakeConfigRequest,
    state: State<'_, AppState>,
) -> Result<VoiceWakeStatus, String> {
    use clawdesk_infra::voice_wake::VoiceWakeManager;

    let config = clawdesk_infra::voice_wake::VoiceWakeConfig {
        enabled: request.enabled,
        wake_phrases: request.wake_phrases.clone(),
        target_agent: request.target_agent.unwrap_or_else(|| "default".to_string()),
        command_template: "{text}".to_string(),
        play_confirmation: true,
        silence_timeout_secs: request.silence_timeout_secs.unwrap_or(3),
    };
    let manager = VoiceWakeManager::new(config);
    let status = VoiceWakeStatus {
        enabled: manager.is_enabled(),
        wake_phrases: request.wake_phrases,
        target_agent: "default".to_string(),
        listening: false,
    };
    let mut voice = state.voice_wake.write().map_err(|e| e.to_string())?;
    *voice = Some(manager);
    Ok(status)
}

#[tauri::command]
pub async fn get_voice_wake_status(
    state: State<'_, AppState>,
) -> Result<VoiceWakeStatus, String> {
    let voice = state.voice_wake.read().map_err(|e| e.to_string())?;
    match voice.as_ref() {
        Some(v) => Ok(VoiceWakeStatus {
            enabled: v.is_enabled(),
            wake_phrases: vec![],
            target_agent: "default".to_string(),
            listening: false,
        }),
        None => Ok(VoiceWakeStatus {
            enabled: false,
            wake_phrases: vec!["Hey ClawDesk".to_string()],
            target_agent: "default".to_string(),
            listening: false,
        }),
    }
}

// ═══════════════════════════════════════════════════════════
// Idle Detection
// ═══════════════════════════════════════════════════════════

#[derive(Debug, Serialize)]
pub struct IdleStatus {
    pub is_idle: bool,
    pub idle_duration_secs: u64,
}

#[tauri::command]
pub async fn get_idle_status(
    state: State<'_, AppState>,
) -> Result<IdleStatus, String> {
    match state.idle_detector.as_ref() {
        Some(detector) => Ok(IdleStatus {
            is_idle: detector.is_idle(),
            idle_duration_secs: detector.idle_duration().as_secs(),
        }),
        None => Ok(IdleStatus {
            is_idle: false,
            idle_duration_secs: 0,
        }),
    }
}

#[tauri::command]
pub async fn record_activity(
    state: State<'_, AppState>,
) -> Result<bool, String> {
    if let Some(ref detector) = state.idle_detector {
        detector.record_activity();
    }
    Ok(true)
}

// ═══════════════════════════════════════════════════════════
// Session Cost Summary (tray / status bar display)
// ═══════════════════════════════════════════════════════════

/// Cost summary for display in the system tray / status bar.
/// Mirrors the macOS app's cost-in-menu-bar feature.
#[derive(Debug, Serialize)]
pub struct SessionCostSummary {
    /// Total cost today in USD.
    pub cost_today_usd: f64,
    /// Total cost this month in USD.
    pub cost_month_usd: f64,
    /// Total input tokens today.
    pub input_tokens_today: u64,
    /// Total output tokens today.
    pub output_tokens_today: u64,
    /// Most expensive model today.
    pub top_model: Option<String>,
    /// Number of active sessions.
    pub active_sessions: usize,
    /// Formatted string for tray display.
    pub tray_label: String,
}

#[tauri::command]
pub async fn get_session_cost_summary(
    state: State<'_, AppState>,
) -> Result<SessionCostSummary, String> {
    use std::sync::atomic::Ordering;

    let cost_micro = state.total_cost_today.load(Ordering::Relaxed);
    let cost_today = cost_micro as f64 / 1_000_000.0;
    let input = state.total_input_tokens.load(Ordering::Relaxed);
    let output = state.total_output_tokens.load(Ordering::Relaxed);

    // Get most expensive model from model costs map
    let top_model = state.model_costs.read()
        .map(|costs| {
            costs.iter()
                .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(model, _)| model.clone())
        })
        .ok()
        .flatten();

    let active = state.sessions.len();

    let tray_label = if cost_today < 0.01 {
        "< $0.01".to_string()
    } else if cost_today < 1.0 {
        format!("${:.2}", cost_today)
    } else {
        format!("${:.1}", cost_today)
    };

    Ok(SessionCostSummary {
        cost_today_usd: cost_today,
        cost_month_usd: cost_today, // Approximation until monthly tracking
        input_tokens_today: input,
        output_tokens_today: output,
        top_model,
        active_sessions: active,
        tray_label,
    })
}
