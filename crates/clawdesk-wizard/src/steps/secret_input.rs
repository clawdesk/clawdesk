//! Secret input step — masked credential entry with validation.

use crate::flow::{StepResult, WizardState};

/// Known provider key patterns for validation.
const KEY_PATTERNS: &[(&str, &str, usize)] = &[
    ("anthropic_key", "sk-ant-", 40),
    ("openai_key", "sk-", 30),
    ("gemini_key", "AI", 20),
];

/// Validate a provider API key by prefix and minimum length.
pub fn validate_api_key(provider: &str, key: &str) -> Result<(), String> {
    for &(name, prefix, min_len) in KEY_PATTERNS {
        if provider == name {
            if key.len() < min_len {
                return Err(format!("{provider} key too short (expected {min_len}+ chars)"));
            }
            if !key.starts_with(prefix) {
                return Err(format!("{provider} key should start with '{prefix}'"));
            }
            return Ok(());
        }
    }
    // Unknown provider — accept if non-empty.
    if key.is_empty() {
        Err(format!("{provider} key cannot be empty"))
    } else {
        Ok(())
    }
}

/// Mask a key for display: show first 6 and last 4 chars.
pub fn mask_key(key: &str) -> String {
    if key.len() <= 10 {
        return "*".repeat(key.len());
    }
    let prefix = &key[..6];
    let suffix = &key[key.len() - 4..];
    format!("{prefix}{}…{suffix}", "*".repeat(key.len().saturating_sub(10).min(20)))
}

/// Execute the secret input step.
pub fn execute_secret_input(state: &mut WizardState, provider: &str, key: &str) -> StepResult {
    match validate_api_key(provider, key) {
        Ok(()) => {
            state.set_config(provider, serde_json::json!(mask_key(key)));
            // In production, the actual key is stored in the credential vault, not in wizard state.
            StepResult::Continue
        }
        Err(msg) => StepResult::Error { message: msg },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_anthropic_key() {
        assert!(validate_api_key("anthropic_key", "sk-ant-api03-1234567890abcdefghijklmnopqrstuv").is_ok());
        assert!(validate_api_key("anthropic_key", "short").is_err());
    }

    #[test]
    fn validate_openai_key() {
        assert!(validate_api_key("openai_key", "sk-proj-1234567890abcdefghijklmnopqrstuv").is_ok());
    }

    #[test]
    fn mask_key_format() {
        let masked = mask_key("sk-ant-api03-1234567890abcdefghij");
        assert!(masked.starts_with("sk-ant"));
        assert!(masked.ends_with("ghij"));
        assert!(masked.contains('*'));
    }

    #[test]
    fn mask_short_key() {
        let masked = mask_key("abcd");
        assert_eq!(masked, "****");
    }
}
