//! D3: Config Secret References (`$vault:provider:credential_id` pattern).
//!
//! Provides a structured way to reference secrets stored in the CredentialVault
//! from configuration values, replacing plaintext API keys with vault references.
//!
//! ## Reference format
//!
//! ```text
//! $vault:<provider>:<credential_id>
//! ```
//!
//! Examples:
//! - `$vault:anthropic:default` → resolves to the Anthropic API key
//! - `$vault:openai:org-key-2` → resolves to a specific OpenAI key
//!
//! ## Secret detection
//!
//! Regex patterns detect plaintext API keys for all major providers,
//! enabling auto-migration from env vars to vault references.
//!
//! ## Integration
//!
//! Config loaders call `resolve_or_passthrough()` on every string value.
//! If the value is a vault reference, it resolves from the CredentialVault.
//! Otherwise, the original value is returned unchanged.

use crate::credential_vault::{CredentialVault, VaultError};
use regex::Regex;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

// ═══════════════════════════════════════════════════════════════════════════
// Secret reference types
// ═══════════════════════════════════════════════════════════════════════════

/// A parsed vault reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRef {
    /// Provider name (e.g., "anthropic", "openai").
    pub provider: String,
    /// Credential identifier within the provider.
    pub credential_id: String,
}

impl SecretRef {
    /// Create a new secret reference.
    pub fn new(provider: impl Into<String>, credential_id: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            credential_id: credential_id.into(),
        }
    }

    /// Serialize to the `$vault:provider:credential_id` format.
    pub fn to_ref_string(&self) -> String {
        format!("$vault:{}:{}", self.provider, self.credential_id)
    }

    /// Parse a `$vault:provider:credential_id` string.
    ///
    /// Returns `None` if the string is not a valid vault reference.
    pub fn parse(value: &str) -> Option<Self> {
        let rest = value.strip_prefix("$vault:")?;
        let (provider, credential_id) = rest.split_once(':')?;

        if provider.is_empty() || credential_id.is_empty() {
            return None;
        }

        // Validate: no nested colons in credential_id
        if credential_id.contains(':') {
            return None;
        }

        Some(Self {
            provider: provider.to_string(),
            credential_id: credential_id.to_string(),
        })
    }

    /// Check if a string is a vault reference.
    pub fn is_vault_ref(value: &str) -> bool {
        value.starts_with("$vault:") && Self::parse(value).is_some()
    }
}

impl std::fmt::Display for SecretRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "$vault:{}:{}", self.provider, self.credential_id)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Secret resolution
// ═══════════════════════════════════════════════════════════════════════════

/// Resolve a config value that may be a vault reference.
///
/// - If `value` is `$vault:provider:credential_id`, resolves from the vault.
/// - Otherwise, returns the value unchanged.
///
/// This is the primary integration point for config loaders.
pub fn resolve_or_passthrough(
    value: &str,
    vault: &CredentialVault,
) -> Result<String, VaultError> {
    if let Some(secret_ref) = SecretRef::parse(value) {
        match vault.get_secret(&secret_ref.provider, &secret_ref.credential_id)? {
            Some(secret) => {
                debug!(
                    provider = %secret_ref.provider,
                    credential_id = %secret_ref.credential_id,
                    "resolved vault reference"
                );
                Ok(secret)
            }
            None => {
                warn!(
                    provider = %secret_ref.provider,
                    credential_id = %secret_ref.credential_id,
                    "vault reference not found, returning empty"
                );
                Err(VaultError::NotFound)
            }
        }
    } else {
        Ok(value.to_string())
    }
}

/// Resolve a config value, falling back to the original value on vault miss.
///
/// Unlike `resolve_or_passthrough`, this never errors — it returns the
/// original value if the vault reference can't be resolved.
pub fn resolve_or_fallback(value: &str, vault: &CredentialVault) -> String {
    match resolve_or_passthrough(value, vault) {
        Ok(resolved) => resolved,
        Err(_) => value.to_string(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Secret detection — identify plaintext API keys
// ═══════════════════════════════════════════════════════════════════════════

/// A detected plaintext secret in a config value.
#[derive(Debug, Clone)]
pub struct DetectedSecret {
    /// Which provider this key belongs to.
    pub provider: String,
    /// The raw key value (sensitive).
    pub value: String,
    /// How confident we are this is a real key (0.0 - 1.0).
    pub confidence: f64,
    /// Suggested credential_id for vault storage.
    pub suggested_id: String,
}

/// Known API key patterns for major providers.
///
/// Each pattern is tuned for high precision (low false-positive rate).
/// We prefer strict prefix matching over loose regex to avoid flagging
/// random hex strings.
pub struct SecretDetector {
    patterns: Vec<ProviderPattern>,
}

struct ProviderPattern {
    provider: String,
    regex: Regex,
    confidence: f64,
    suggested_id: String,
}

impl SecretDetector {
    /// Create a detector with patterns for all major providers.
    pub fn new() -> Self {
        let patterns = vec![
            // Anthropic: sk-ant-api03-... (40+ chars)
            ProviderPattern {
                provider: "anthropic".into(),
                regex: Regex::new(r"^sk-ant-[a-zA-Z0-9\-_]{30,}$").unwrap(),
                confidence: 0.95,
                suggested_id: "default".into(),
            },
            // OpenAI: sk-... (48+ chars, sometimes sk-proj-...)
            ProviderPattern {
                provider: "openai".into(),
                regex: Regex::new(r"^sk-(?:proj-)?[a-zA-Z0-9]{32,}$").unwrap(),
                confidence: 0.90,
                suggested_id: "default".into(),
            },
            // Google AI: AIza... (39 chars total)
            ProviderPattern {
                provider: "google".into(),
                regex: Regex::new(r"^AIza[a-zA-Z0-9\-_]{35}$").unwrap(),
                confidence: 0.92,
                suggested_id: "default".into(),
            },
            // Azure OpenAI: 32-char hex
            ProviderPattern {
                provider: "azure".into(),
                regex: Regex::new(r"^[a-f0-9]{32}$").unwrap(),
                confidence: 0.60, // Low confidence — could be any hex hash
                suggested_id: "default".into(),
            },
            // Cohere: varies, often starts with specific prefixes
            ProviderPattern {
                provider: "cohere".into(),
                regex: Regex::new(r"^[a-zA-Z0-9]{40}$").unwrap(),
                confidence: 0.50,
                suggested_id: "default".into(),
            },
            // Generic bearer/API key (longer alphanumeric strings)
            ProviderPattern {
                provider: "unknown".into(),
                regex: Regex::new(r"^[a-zA-Z0-9\-_]{40,}$").unwrap(),
                confidence: 0.30,
                suggested_id: "detected".into(),
            },
        ];

        Self { patterns }
    }

    /// Detect if a value looks like a plaintext API key.
    ///
    /// Returns the best-matching provider pattern, or None if the value
    /// doesn't look like a secret.
    pub fn detect(&self, value: &str) -> Option<DetectedSecret> {
        // Skip vault references
        if SecretRef::is_vault_ref(value) {
            return None;
        }

        // Skip very short values
        if value.len() < 20 {
            return None;
        }

        // Find the highest-confidence match
        let mut best: Option<DetectedSecret> = None;

        for pattern in &self.patterns {
            if pattern.regex.is_match(value) {
                let detected = DetectedSecret {
                    provider: pattern.provider.clone(),
                    value: value.to_string(),
                    confidence: pattern.confidence,
                    suggested_id: pattern.suggested_id.clone(),
                };

                if best
                    .as_ref()
                    .map_or(true, |b| detected.confidence > b.confidence)
                {
                    best = Some(detected);
                }
            }
        }

        best
    }
}

impl Default for SecretDetector {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Migration helper — auto-migrate env vars to vault
// ═══════════════════════════════════════════════════════════════════════════

/// Known environment variable → provider mappings.
pub const ENV_VAR_MAPPINGS: &[(&str, &str, &str)] = &[
    ("ANTHROPIC_API_KEY", "anthropic", "default"),
    ("OPENAI_API_KEY", "openai", "default"),
    ("GOOGLE_API_KEY", "google", "default"),
    ("AZURE_OPENAI_API_KEY", "azure", "default"),
    ("COHERE_API_KEY", "cohere", "default"),
    ("VERTEX_PROJECT_ID", "vertex", "project_id"),
    ("TELEGRAM_BOT_TOKEN", "telegram", "bot_token"),
    ("DISCORD_TOKEN", "discord", "bot_token"),
    ("SLACK_BOT_TOKEN", "slack", "bot_token"),
];

/// Result of migrating env vars to the vault.
#[derive(Debug, Default)]
pub struct MigrationResult {
    /// Successfully migrated env vars.
    pub migrated: Vec<MigratedVar>,
    /// Env vars that were already vault references (no migration needed).
    pub already_refs: Vec<String>,
    /// Env vars that were not set.
    pub not_set: Vec<String>,
}

/// A single migrated environment variable.
#[derive(Debug, Clone)]
pub struct MigratedVar {
    pub env_var: String,
    pub provider: String,
    pub credential_id: String,
    pub vault_ref: String,
}

/// Migrate all known env var API keys into the vault.
///
/// For each mapping in `ENV_VAR_MAPPINGS`:
/// 1. Check if the env var is set.
/// 2. If it's already a `$vault:` reference, skip.
/// 3. Otherwise, store the value in the vault and return the ref string.
///
/// The caller is responsible for updating config files to use the returned
/// vault references instead of raw env vars.
pub fn migrate_env_to_vault(vault: &CredentialVault) -> MigrationResult {
    let mut result = MigrationResult::default();

    for (env_var, provider, credential_id) in ENV_VAR_MAPPINGS {
        match std::env::var(env_var) {
            Ok(value) if !value.is_empty() => {
                if SecretRef::is_vault_ref(&value) {
                    result.already_refs.push(env_var.to_string());
                    continue;
                }

                match vault.store_secret(provider, credential_id, &value) {
                    Ok(()) => {
                        let vault_ref =
                            SecretRef::new(*provider, *credential_id).to_ref_string();
                        info!(
                            env_var = %env_var,
                            provider = %provider,
                            "migrated env var to vault"
                        );
                        result.migrated.push(MigratedVar {
                            env_var: env_var.to_string(),
                            provider: provider.to_string(),
                            credential_id: credential_id.to_string(),
                            vault_ref,
                        });
                    }
                    Err(e) => {
                        warn!(
                            env_var = %env_var,
                            error = %e,
                            "failed to migrate env var to vault"
                        );
                    }
                }
            }
            _ => {
                result.not_set.push(env_var.to_string());
            }
        }
    }

    result
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential_vault::VaultBackend;

    #[test]
    fn parse_valid_ref() {
        let r = SecretRef::parse("$vault:anthropic:default").unwrap();
        assert_eq!(r.provider, "anthropic");
        assert_eq!(r.credential_id, "default");
    }

    #[test]
    fn parse_invalid_refs() {
        assert!(SecretRef::parse("plain-text").is_none());
        assert!(SecretRef::parse("$vault:").is_none());
        assert!(SecretRef::parse("$vault:provider:").is_none());
        assert!(SecretRef::parse("$vault::id").is_none());
        assert!(SecretRef::parse("$vault:a:b:c").is_none()); // nested colons
    }

    #[test]
    fn roundtrip_ref_string() {
        let r = SecretRef::new("openai", "org-key-2");
        let s = r.to_ref_string();
        assert_eq!(s, "$vault:openai:org-key-2");
        let parsed = SecretRef::parse(&s).unwrap();
        assert_eq!(parsed, r);
    }

    #[test]
    fn is_vault_ref() {
        assert!(SecretRef::is_vault_ref("$vault:anthropic:default"));
        assert!(!SecretRef::is_vault_ref("sk-ant-12345"));
        assert!(!SecretRef::is_vault_ref("$vault:invalid"));
    }

    #[test]
    fn resolve_vault_ref() {
        let vault = CredentialVault::with_backend("test", VaultBackend::InMemory);
        vault.store_secret("anthropic", "default", "sk-ant-real-key").unwrap();

        let resolved = resolve_or_passthrough("$vault:anthropic:default", &vault).unwrap();
        assert_eq!(resolved, "sk-ant-real-key");
    }

    #[test]
    fn resolve_passthrough() {
        let vault = CredentialVault::with_backend("test", VaultBackend::InMemory);

        let result = resolve_or_passthrough("just-a-string", &vault).unwrap();
        assert_eq!(result, "just-a-string");
    }

    #[test]
    fn resolve_missing_returns_error() {
        let vault = CredentialVault::with_backend("test", VaultBackend::InMemory);

        let result = resolve_or_passthrough("$vault:openai:missing", &vault);
        assert!(result.is_err());
    }

    #[test]
    fn resolve_fallback_on_miss() {
        let vault = CredentialVault::with_backend("test", VaultBackend::InMemory);

        let result = resolve_or_fallback("$vault:openai:missing", &vault);
        assert_eq!(result, "$vault:openai:missing");
    }

    #[test]
    fn detect_anthropic_key() {
        let detector = SecretDetector::new();
        let detected = detector
            .detect("sk-ant-api03-abcdefghijklmnopqrstuvwxyz12345678")
            .unwrap();
        assert_eq!(detected.provider, "anthropic");
        assert!(detected.confidence > 0.9);
    }

    #[test]
    fn detect_openai_key() {
        let detector = SecretDetector::new();
        let detected = detector
            .detect("sk-abcdefghijklmnopqrstuvwxyz1234567890abcdefghijklmnop")
            .unwrap();
        assert_eq!(detected.provider, "openai");
    }

    #[test]
    fn detect_google_key() {
        let detector = SecretDetector::new();
        let detected = detector
            .detect("AIzaSyA12345678901234567890123456789012")
            .unwrap();
        assert_eq!(detected.provider, "google");
    }

    #[test]
    fn detect_skips_vault_refs() {
        let detector = SecretDetector::new();
        assert!(detector.detect("$vault:anthropic:default").is_none());
    }

    #[test]
    fn detect_skips_short_values() {
        let detector = SecretDetector::new();
        assert!(detector.detect("short").is_none());
    }

    #[test]
    fn env_var_mappings_cover_all_providers() {
        let providers: Vec<&str> = ENV_VAR_MAPPINGS.iter().map(|(_, p, _)| *p).collect();
        assert!(providers.contains(&"anthropic"));
        assert!(providers.contains(&"openai"));
        assert!(providers.contains(&"google"));
        assert!(providers.contains(&"azure"));
        assert!(providers.contains(&"cohere"));
    }
}
