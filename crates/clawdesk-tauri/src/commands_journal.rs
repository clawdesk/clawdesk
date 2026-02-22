//! T9: Journal Store — IPC commands for journaling with SochDB persistence.
//!
//! Wires [`clawdesk_skills::journal`] domain model to Tauri IPC layer with
//! hot-cache in `AppState` + write-through to SochDB (`journal/{id}`).

use crate::state::AppState;
use chrono::Utc;
use clawdesk_skills::journal::{EntryType, JournalEntry, JournalTimeSeries};
use serde::Deserialize;
use std::collections::HashMap;
use tauri::State;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct CreateJournalEntryRequest {
    pub entry_type: EntryType,
    /// ISO-8601 datetime for when the event actually occurred (defaults to now).
    #[serde(default)]
    pub occurred_at: Option<String>,
    pub content: String,
    #[serde(default)]
    pub data: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub attachments: Vec<String>,
    #[serde(default = "default_source")]
    pub source: String,
}

fn default_source() -> String {
    "clawdesk".to_string()
}

/// Add a journal entry.
#[tauri::command]
pub async fn add_journal_entry(
    request: CreateJournalEntryRequest,
    state: State<'_, AppState>,
) -> Result<JournalEntry, String> {
    let now = Utc::now();
    let occurred_at = request
        .occurred_at
        .as_deref()
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or(now);

    let entry = JournalEntry {
        id: Uuid::new_v4().to_string(),
        entry_type: request.entry_type,
        recorded_at: now,
        occurred_at,
        content: request.content,
        data: request.data,
        tags: request.tags,
        value: request.value,
        unit: request.unit,
        attachments: request.attachments,
        source: request.source,
    };

    // Persist to SochDB
    state.persist_journal_entry(&entry);

    // Insert into hot cache
    {
        let mut journal = state.journal_entries.write().map_err(|e| e.to_string())?;
        journal.insert(entry.id.clone(), entry.clone());
    }

    Ok(entry)
}

/// List journal entries with optional filters.
#[tauri::command]
pub async fn list_journal_entries(
    entry_type: Option<String>,
    tag: Option<String>,
    from: Option<String>,
    to: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<JournalEntry>, String> {
    let journal = state.journal_entries.read().map_err(|e| e.to_string())?;
    let mut entries: Vec<JournalEntry> = journal.values().cloned().collect();

    // Filter by type
    if let Some(ref type_str) = entry_type {
        if let Ok(et) = serde_json::from_value::<EntryType>(serde_json::Value::String(type_str.clone())) {
            entries.retain(|e| e.entry_type == et);
        }
    }

    // Filter by tag
    if let Some(ref t) = tag {
        entries.retain(|e| e.tags.iter().any(|tag| tag == t));
    }

    // Filter by date range
    if let Some(ref from_str) = from {
        if let Ok(from_dt) = chrono::DateTime::parse_from_rfc3339(from_str) {
            let from_utc = from_dt.with_timezone(&Utc);
            entries.retain(|e| e.occurred_at >= from_utc);
        }
    }
    if let Some(ref to_str) = to {
        if let Ok(to_dt) = chrono::DateTime::parse_from_rfc3339(to_str) {
            let to_utc = to_dt.with_timezone(&Utc);
            entries.retain(|e| e.occurred_at <= to_utc);
        }
    }

    entries.sort_by(|a, b| b.occurred_at.cmp(&a.occurred_at));
    Ok(entries)
}

/// Get a single journal entry by ID.
#[tauri::command]
pub async fn get_journal_entry(
    id: String,
    state: State<'_, AppState>,
) -> Result<JournalEntry, String> {
    let journal = state.journal_entries.read().map_err(|e| e.to_string())?;
    journal
        .get(&id)
        .cloned()
        .ok_or_else(|| format!("Journal entry '{}' not found", id))
}

/// Delete a journal entry.
#[tauri::command]
pub async fn delete_journal_entry(
    id: String,
    state: State<'_, AppState>,
) -> Result<bool, String> {
    let removed = {
        let mut journal = state.journal_entries.write().map_err(|e| e.to_string())?;
        journal.remove(&id).is_some()
    };

    if removed {
        state.delete_journal_entry_from_store(&id);
    }

    Ok(removed)
}

/// Run a case-crossover trigger analysis on journal entries.
///
/// `outcome_type` — the entry type representing the outcome (e.g. "Symptom")
/// `exposure_tag` — the tag representing the exposure (e.g. "caffeine")
/// `days` — number of days to analyze (default: 30)
#[tauri::command]
pub async fn analyze_journal_triggers(
    outcome_type: String,
    exposure_tag: String,
    days: Option<i64>,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let journal = state.journal_entries.read().map_err(|e| e.to_string())?;

    let outcome_et: EntryType =
        serde_json::from_value(serde_json::Value::String(outcome_type.clone()))
            .map_err(|_| format!("Invalid entry type: {}", outcome_type))?;

    // Build time series
    let mut ts = JournalTimeSeries::default();
    for entry in journal.values() {
        ts.add(entry.clone());
    }

    let days = days.unwrap_or(30);
    let end = Utc::now().date_naive();
    let start = end - chrono::Duration::days(days);

    let study = clawdesk_skills::journal::analyze_trigger(
        &ts,
        &outcome_type,
        &exposure_tag,
        |day_entries: &[&JournalEntry]| day_entries.iter().any(|e| e.entry_type == outcome_et),
        |day_entries: &[&JournalEntry]| {
            day_entries
                .iter()
                .any(|e| e.tags.iter().any(|t| t == &exposure_tag))
        },
        start,
        end,
    );

    Ok(serde_json::json!({
        "case_exposed": study.case_exposed,
        "case_unexposed": study.case_unexposed,
        "control_exposed": study.control_exposed,
        "control_unexposed": study.control_unexposed,
        "odds_ratio": study.odds_ratio(),
        "interpretation": format!("{:?}", study.interpret()),
    }))
}

/// Get daily aggregated values for a specific entry type.
#[tauri::command]
pub async fn get_journal_daily_values(
    entry_type: String,
    state: State<'_, AppState>,
) -> Result<Vec<serde_json::Value>, String> {
    let journal = state.journal_entries.read().map_err(|e| e.to_string())?;

    let et: EntryType =
        serde_json::from_value(serde_json::Value::String(entry_type.clone()))
            .map_err(|_| format!("Invalid entry type: {}", entry_type))?;

    let mut ts = JournalTimeSeries::default();
    for entry in journal.values().filter(|e| e.entry_type == et) {
        ts.add(entry.clone());
    }

    let daily = ts.daily_values(et);
    let result: Vec<serde_json::Value> = daily
        .iter()
        .map(|(date, avg)| {
            serde_json::json!({
                "date": date.to_string(),
                "value": avg,
            })
        })
        .collect();

    Ok(result)
}
