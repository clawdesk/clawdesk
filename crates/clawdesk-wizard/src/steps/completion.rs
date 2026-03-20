//! Completion step — summary display and atomic config write.

use crate::flow::{StepResult, WizardState};

/// Generate a human-readable summary of the wizard configuration.
pub fn generate_summary(state: &WizardState) -> String {
    let mut lines = Vec::new();
    lines.push("═══ Configuration Summary ═══".to_string());
    lines.push(String::new());

    let mut sorted: Vec<_> = state.accumulated_config.iter().collect();
    sorted.sort_by(|(a, _), (b, _)| a.cmp(b));

    for (key, value) in sorted {
        let display = match value {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        };
        lines.push(format!("  {key}: {display}"));
    }

    lines.push(String::new());
    lines.push(format!("  Steps completed: {}", state.completed_steps.len()));
    lines.push(format!("  Visible steps completed: {}", state.visible_steps_completed()));
    lines.push(String::new());
    lines.push("Review above and confirm to finalize.".to_string());

    lines.join("\n")
}

/// Atomically write the configuration to disk.
///
/// Write to a temporary file first, then rename. This prevents partial writes.
pub async fn finalize_config(
    state: &WizardState,
    config_path: &std::path::Path,
) -> Result<(), String> {
    let config_json = serde_json::to_string_pretty(&state.accumulated_config)
        .map_err(|e| format!("serialization error: {e}"))?;

    let tmp_path = config_path.with_extension("tmp");

    tokio::fs::write(&tmp_path, &config_json)
        .await
        .map_err(|e| format!("write error: {e}"))?;

    tokio::fs::rename(&tmp_path, config_path)
        .await
        .map_err(|e| format!("rename error: {e}"))?;

    Ok(())
}

/// Execute the completion step.
pub fn execute_completion(state: &WizardState) -> StepResult {
    // In the new 3-step wizard, sandbox is enabled by default (no risk ack needed).
    // Security setup is a background task — always runs with safe defaults.

    if state.accumulated_config.is_empty() {
        return StepResult::Error {
            message: "Cannot finalize: no configuration has been set.".into(),
        };
    }

    StepResult::Continue
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_includes_config() {
        let mut state = WizardState::default();
        state.set_config("provider", serde_json::json!("anthropic"));
        state.set_config("gateway_port", serde_json::json!(18789));
        let summary = generate_summary(&state);
        assert!(summary.contains("provider"));
        assert!(summary.contains("18789"));
    }

    #[test]
    fn completion_requires_config() {
        let state = WizardState::default();
        match execute_completion(&state) {
            StepResult::Error { message } => assert!(message.contains("no configuration")),
            _ => panic!("should require config"),
        }
    }

    #[test]
    fn completion_succeeds_with_config() {
        let mut state = WizardState::default();
        state.set_config("model", serde_json::json!("ollama"));
        match execute_completion(&state) {
            StepResult::Continue => {} // expected
            other => panic!("expected Continue, got {:?}", other),
        }
    }
}
