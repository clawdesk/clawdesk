//! Provider setup step — multi-provider configuration.

use crate::flow::{StepResult, WizardState};

/// Supported providers for wizard setup.
pub const PROVIDERS: &[ProviderInfo] = &[
    ProviderInfo { id: "anthropic", name: "Anthropic (Claude)", env_var: "ANTHROPIC_API_KEY", key_prefix: "sk-ant-" },
    ProviderInfo { id: "openai", name: "OpenAI (GPT)", env_var: "OPENAI_API_KEY", key_prefix: "sk-" },
    ProviderInfo { id: "gemini", name: "Google Gemini", env_var: "GEMINI_API_KEY", key_prefix: "AI" },
    ProviderInfo { id: "ollama", name: "Ollama (local)", env_var: "OLLAMA_HOST", key_prefix: "" },
];

pub struct ProviderInfo {
    pub id: &'static str,
    pub name: &'static str,
    pub env_var: &'static str,
    pub key_prefix: &'static str,
}

/// Auto-detect providers from environment variables.
pub fn detect_providers_from_env() -> Vec<&'static str> {
    PROVIDERS
        .iter()
        .filter(|p| std::env::var(p.env_var).is_ok())
        .map(|p| p.id)
        .collect()
}

/// Execute the provider setup step.
pub fn execute_provider_setup(state: &mut WizardState, selected_providers: &[&str]) -> StepResult {
    if selected_providers.is_empty() {
        return StepResult::Error {
            message: "At least one provider must be configured. Use Ollama for local-only operation.".into(),
        };
    }

    state.set_config("providers", serde_json::json!(selected_providers));

    // Set default model based on first provider.
    let default_model = match selected_providers.first().copied() {
        Some("anthropic") => "claude-sonnet-4-20250514",
        Some("openai") => "gpt-4o",
        Some("gemini") => "gemini-2.0-flash",
        Some("ollama") => "llama3.1",
        _ => "claude-sonnet-4-20250514",
    };
    state.set_config("default_model", serde_json::json!(default_model));

    StepResult::Continue
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn providers_list() {
        assert!(PROVIDERS.len() >= 4);
    }

    #[test]
    fn empty_providers_rejected() {
        let mut state = WizardState::default();
        match execute_provider_setup(&mut state, &[]) {
            StepResult::Error { .. } => {}
            _ => panic!("empty provider list should be rejected"),
        }
    }

    #[test]
    fn anthropic_sets_default_model() {
        let mut state = WizardState::default();
        let result = execute_provider_setup(&mut state, &["anthropic"]);
        assert!(matches!(result, StepResult::Continue));
        let model = state.accumulated_config.get("default_model").unwrap();
        assert!(model.as_str().unwrap().contains("claude"));
    }
}
