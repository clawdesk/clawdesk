//! Environment variable & config injection pipeline.
//!
//! ## Env Injection (P1)
//!
//! Bridges ClawDesk's config system to OpenClaw's env-var-based skill
//! configuration. When a skill declares `primaryEnv: "OPENAI_API_KEY"` and
//! the user has `skills.entries.openai-image-gen.apiKey` in their config,
//! this module maps the config value to the environment variable before
//! skill execution.
//!
//! This enables ~20% of OpenClaw skills (the "ContextPatch" tier) that
//! require API keys or config values to function.
//!
//! ## Security
//!
//! - Env values wrap in `SecretString` — `Display` and `Debug` show `***`
//! - Secrets never appear in tracing spans or serialized output
//! - Env injection is scoped — restored after skill execution

use crate::openclaw_adapter::OpenClawMetadata;
use std::collections::HashMap;
use tracing::{debug, warn};

/// A string that redacts its value in Display and Debug output.
///
/// Prevents accidental logging of API keys, tokens, and other secrets.
#[derive(Clone)]
pub struct SecretString(String);

impl SecretString {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Access the raw value — use sparingly.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Check if the secret is empty.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl std::fmt::Display for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "***")
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretString(***)")
    }
}

/// Resolved environment context for a skill execution.
#[derive(Debug, Clone)]
pub struct SkillEnvContext {
    /// Env vars to inject before execution.
    pub env_vars: HashMap<String, SecretString>,
    /// Env vars that were in the OS environment (for restoration).
    original_values: HashMap<String, Option<String>>,
}

impl SkillEnvContext {
    /// Create an empty context.
    pub fn empty() -> Self {
        Self {
            env_vars: HashMap::new(),
            original_values: HashMap::new(),
        }
    }

    /// Whether there are any env vars to inject.
    pub fn is_empty(&self) -> bool {
        self.env_vars.is_empty()
    }

    /// Apply the env vars to the current process environment.
    ///
    /// Saves original values for later restoration.
    pub fn apply(&mut self) {
        for (key, value) in &self.env_vars {
            // Save original value
            self.original_values
                .insert(key.clone(), std::env::var(key).ok());
            // Set the new value
            std::env::set_var(key, value.expose());
        }
        debug!(
            count = self.env_vars.len(),
            "applied skill env context"
        );
    }

    /// Restore the original environment after skill execution.
    pub fn restore(&self) {
        for (key, original) in &self.original_values {
            match original {
                Some(val) => std::env::set_var(key, val),
                None => std::env::remove_var(key),
            }
        }
        debug!(
            count = self.original_values.len(),
            "restored original env"
        );
    }
}

/// Skill config entry from the ClawDesk config file.
///
/// Represents `skills.entries.<name>` in the config.
#[derive(Debug, Clone, Default)]
pub struct SkillConfigEntry {
    /// API key for the skill's primary service.
    pub api_key: Option<SecretString>,
    /// Additional env var overrides.
    pub env: HashMap<String, SecretString>,
    /// Whether the skill is enabled (default: true).
    pub enabled: Option<bool>,
}

/// Environment injection resolver.
///
/// Resolves the full env context for a skill by merging:
/// 1. OS environment (lowest priority)
/// 2. Global config env overrides
/// 3. Per-skill config entries (highest priority)
pub struct EnvResolver {
    /// Global env overrides from config.
    global_env: HashMap<String, SecretString>,
    /// Per-skill config entries.
    skill_configs: HashMap<String, SkillConfigEntry>,
}

impl EnvResolver {
    /// Create a new resolver.
    pub fn new() -> Self {
        Self {
            global_env: HashMap::new(),
            skill_configs: HashMap::new(),
        }
    }

    /// Add a global env override.
    pub fn add_global_env(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.global_env
            .insert(key.into(), SecretString::new(value.into()));
    }

    /// Add a per-skill config entry.
    pub fn add_skill_config(&mut self, skill_name: impl Into<String>, config: SkillConfigEntry) {
        self.skill_configs.insert(skill_name.into(), config);
    }

    /// Resolve the env context for a skill.
    ///
    /// Merging precedence:
    /// 1. OS env vars (already set — no injection needed)
    /// 2. Global config env → injected
    /// 3. Per-skill apiKey → mapped to metadata.primaryEnv
    /// 4. Per-skill env overrides → injected directly
    pub fn resolve(&self, skill_name: &str, meta: &OpenClawMetadata) -> SkillEnvContext {
        let mut ctx = SkillEnvContext::empty();

        // Level 2: Global env overrides
        for (key, value) in &self.global_env {
            // Only inject if the OS env doesn't already have it
            if std::env::var(key).is_err() {
                ctx.env_vars.insert(key.clone(), value.clone());
            }
        }

        // Level 3 & 4: Per-skill config
        if let Some(skill_config) = self.skill_configs.get(skill_name) {
            // Map apiKey to primaryEnv
            if let (Some(api_key), Some(primary_env)) =
                (&skill_config.api_key, &meta.primary_env)
            {
                if !api_key.is_empty() {
                    ctx.env_vars
                        .insert(primary_env.clone(), api_key.clone());
                    debug!(
                        skill = %skill_name,
                        env = %primary_env,
                        "mapped apiKey to primaryEnv"
                    );
                }
            }

            // Direct env overrides
            for (key, value) in &skill_config.env {
                ctx.env_vars.insert(key.clone(), value.clone());
            }
        }

        // Check for missing required env vars and warn
        for required_var in &meta.required_env {
            let has_it = std::env::var(required_var).is_ok()
                || ctx.env_vars.contains_key(required_var);
            if !has_it {
                warn!(
                    skill = %skill_name,
                    var = %required_var,
                    "required env var not available from OS, config, or injection"
                );
            }
        }

        ctx
    }

    /// Check if a skill is explicitly disabled via config.
    pub fn is_skill_disabled(&self, skill_name: &str) -> bool {
        self.skill_configs
            .get(skill_name)
            .and_then(|c| c.enabled)
            .map(|e| !e)
            .unwrap_or(false)
    }
}

impl Default for EnvResolver {
    fn default() -> Self {
        Self::new()
    }
}

/// RAII guard that applies env context on creation and restores on drop.
pub struct EnvGuard {
    ctx: SkillEnvContext,
}

impl EnvGuard {
    /// Apply the env context and return a guard that restores on drop.
    pub fn apply(mut ctx: SkillEnvContext) -> Self {
        ctx.apply();
        Self { ctx }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        self.ctx.restore();
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_string_redacts() {
        let s = SecretString::new("my-api-key");
        assert_eq!(format!("{}", s), "***");
        assert_eq!(format!("{:?}", s), "SecretString(***)");
        assert_eq!(s.expose(), "my-api-key");
    }

    #[test]
    fn resolve_empty_meta_no_injection() {
        let resolver = EnvResolver::new();
        let meta = OpenClawMetadata::default();
        let ctx = resolver.resolve("test-skill", &meta);
        assert!(ctx.is_empty());
    }

    #[test]
    fn resolve_primary_env_from_api_key() {
        let mut resolver = EnvResolver::new();
        resolver.add_skill_config(
            "openai-image-gen",
            SkillConfigEntry {
                api_key: Some(SecretString::new("sk-test-123")),
                ..Default::default()
            },
        );

        let meta = OpenClawMetadata {
            primary_env: Some("OPENAI_API_KEY".to_string()),
            ..Default::default()
        };

        let ctx = resolver.resolve("openai-image-gen", &meta);
        assert!(!ctx.is_empty());
        let val = ctx.env_vars.get("OPENAI_API_KEY").unwrap();
        assert_eq!(val.expose(), "sk-test-123");
    }

    #[test]
    fn resolve_direct_env_overrides() {
        let mut resolver = EnvResolver::new();
        let mut env = HashMap::new();
        env.insert("CUSTOM_VAR".to_string(), SecretString::new("custom-value"));
        resolver.add_skill_config(
            "my-skill",
            SkillConfigEntry {
                env,
                ..Default::default()
            },
        );

        let meta = OpenClawMetadata::default();
        let ctx = resolver.resolve("my-skill", &meta);
        assert_eq!(ctx.env_vars.get("CUSTOM_VAR").unwrap().expose(), "custom-value");
    }

    #[test]
    fn global_env_injected() {
        let mut resolver = EnvResolver::new();
        resolver.add_global_env("GLOBAL_TEST_VAR_98765", "global-value");

        let meta = OpenClawMetadata::default();
        let ctx = resolver.resolve("any-skill", &meta);

        // Should be injected since it's not in OS env
        assert_eq!(
            ctx.env_vars.get("GLOBAL_TEST_VAR_98765").unwrap().expose(),
            "global-value"
        );
    }

    #[test]
    fn skill_disabled_check() {
        let mut resolver = EnvResolver::new();
        resolver.add_skill_config(
            "disabled-skill",
            SkillConfigEntry {
                enabled: Some(false),
                ..Default::default()
            },
        );

        assert!(resolver.is_skill_disabled("disabled-skill"));
        assert!(!resolver.is_skill_disabled("other-skill"));
    }

    #[test]
    fn env_context_apply_and_restore() {
        let unique_var = "CLAWDESK_TEST_ENV_INJECTION_42";

        // Ensure var is not set
        std::env::remove_var(unique_var);
        assert!(std::env::var(unique_var).is_err());

        let mut ctx = SkillEnvContext::empty();
        ctx.env_vars
            .insert(unique_var.to_string(), SecretString::new("injected"));

        // Apply
        ctx.apply();
        assert_eq!(std::env::var(unique_var).unwrap(), "injected");

        // Restore
        ctx.restore();
        assert!(std::env::var(unique_var).is_err());
    }

    #[test]
    fn env_guard_restores_on_drop() {
        let unique_var = "CLAWDESK_TEST_ENV_GUARD_42";
        std::env::remove_var(unique_var);

        let mut ctx = SkillEnvContext::empty();
        ctx.env_vars
            .insert(unique_var.to_string(), SecretString::new("guard-test"));

        {
            let _guard = EnvGuard::apply(ctx);
            assert_eq!(std::env::var(unique_var).unwrap(), "guard-test");
        } // guard dropped → env restored

        assert!(std::env::var(unique_var).is_err());
    }

    #[test]
    fn per_skill_overrides_global() {
        let mut resolver = EnvResolver::new();
        resolver.add_global_env("SHARED_KEY", "global");

        let mut env = HashMap::new();
        env.insert("SHARED_KEY".to_string(), SecretString::new("per-skill"));
        resolver.add_skill_config(
            "my-skill",
            SkillConfigEntry {
                env,
                ..Default::default()
            },
        );

        let ctx = resolver.resolve("my-skill", &OpenClawMetadata::default());
        // Per-skill should win over global
        assert_eq!(ctx.env_vars.get("SHARED_KEY").unwrap().expose(), "per-skill");
    }
}
