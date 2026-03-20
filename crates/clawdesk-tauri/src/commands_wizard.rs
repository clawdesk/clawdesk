//! Tauri commands for the Wizard Flow (Phase 1.2).
//!
//! Exposes the 3-step adaptive onboarding wizard to the Tauri frontend.

use crate::state::AppState;
use clawdesk_wizard::flow::{
    BackgroundTaskStatus, UseCase, WizardFlow, WizardState, WizardStep,
};
use serde::{Deserialize, Serialize};
use tauri::State;
use std::sync::Mutex;

/// Get the current wizard state (or create a new one).
#[tauri::command]
pub async fn wizard_get_state(
    state: State<'_, AppState>,
) -> Result<WizardState, String> {
    let flow = WizardFlow::new();
    Ok(flow.state)
}

/// Advance the wizard to the next visible step.
/// Returns the list of background tasks that should be launched.
#[tauri::command]
pub async fn wizard_advance(
    mut wizard_state: WizardState,
) -> Result<WizardAdvanceResult, String> {
    let bg_tasks = wizard_state.advance()
        .map_err(|e| e.to_string())?;

    let bg_task_names: Vec<String> = bg_tasks.iter()
        .map(|t| format!("{:?}", t))
        .collect();

    Ok(WizardAdvanceResult {
        state: wizard_state,
        background_tasks: bg_task_names,
    })
}

/// Set personalization data on the wizard.
#[tauri::command]
pub async fn wizard_set_personalization(
    mut wizard_state: WizardState,
    name: Option<String>,
    avatar: Option<String>,
    use_cases: Vec<String>,
) -> Result<WizardState, String> {
    let parsed_cases: Vec<UseCase> = use_cases.iter()
        .filter_map(|s| match s.as_str() {
            "coding" => Some(UseCase::Coding),
            "writing" => Some(UseCase::Writing),
            "research" => Some(UseCase::Research),
            "data_analysis" => Some(UseCase::DataAnalysis),
            "automation" => Some(UseCase::Automation),
            "communication" => Some(UseCase::Communication),
            "creative" => Some(UseCase::Creative),
            "education" => Some(UseCase::Education),
            "business" => Some(UseCase::Business),
            "personal" => Some(UseCase::Personal),
            _ => None,
        })
        .collect();

    wizard_state.set_personalization(name, avatar, parsed_cases);
    Ok(wizard_state)
}

/// Get the list of available use cases.
#[tauri::command]
pub async fn wizard_get_use_cases() -> Result<Vec<UseCaseInfo>, String> {
    Ok(UseCase::all().iter().map(|uc| UseCaseInfo {
        id: format!("{:?}", uc).to_lowercase(),
        label: uc.label().to_string(),
    }).collect())
}

/// Get the default configuration that's applied during background setup.
#[tauri::command]
pub async fn wizard_get_defaults() -> Result<std::collections::HashMap<String, serde_json::Value>, String> {
    Ok(WizardFlow::default_config())
}

/// Get wizard progress percentage.
#[tauri::command]
pub async fn wizard_get_progress(
    wizard_state: WizardState,
) -> Result<f32, String> {
    let flow = WizardFlow::resume(wizard_state);
    Ok(flow.progress())
}

#[derive(Debug, Serialize)]
pub struct WizardAdvanceResult {
    pub state: WizardState,
    pub background_tasks: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct UseCaseInfo {
    pub id: String,
    pub label: String,
}
