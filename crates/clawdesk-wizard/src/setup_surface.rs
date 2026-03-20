//! Channel setup surface — declarative onboarding for channel plugins.
//!
//! ## Problem
//!
//! Channel onboarding mixed imperative setup code with credential
//! collection, making each channel's setup bespoke and untestable.
//! Adding a new channel required writing a full setup flow from scratch.
//!
//! ## Design
//!
//! Each channel declares its setup steps as pure data — credential inputs,
//! text inputs, env shortcuts, status checks. The wizard queries this
//! surface and renders generically.
//!
//! This is a functor from channel-specific setup declarations to a
//! uniform rendering:
//!   `F: (CredentialSteps × TextInputs × EnvShortcuts) → WizardUI`
//!
//! The functor preserves composition: multi-step setups compose
//! sequentially, and conditional steps compose via `should_prompt`.
//!
//! ## Lifecycle
//!
//! 1. Channel implements `ChannelSetupSurface`
//! 2. Wizard queries `credentials()`, `text_inputs()`, `env_shortcuts()`
//! 3. For each step: call `should_prompt()` → render if true → `apply()`
//! 4. `status()` gives progress/completion state

use serde::{Deserialize, Serialize};

/// Setup status — whether a channel is fully configured.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupStatus {
    /// Overall setup state.
    pub state: SetupState,
    /// Per-credential status (which are configured, which are missing).
    pub credential_status: Vec<CredentialStatus>,
    /// Human-readable summary (e.g., "Bot token configured, webhook URL missing").
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupState {
    /// Not configured at all.
    NotConfigured,
    /// Partially configured (some credentials present).
    Partial,
    /// Fully configured and ready.
    Configured,
    /// Configured but unhealthy (e.g., token expired).
    Degraded,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialStatus {
    pub key: String,
    pub label: String,
    pub configured: bool,
    pub from_env: bool,
}

/// A credential step in the setup wizard.
///
/// Each step represents one credential the channel needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialStep {
    /// Unique key for this credential (e.g., "bot_token").
    pub input_key: String,
    /// Display label (e.g., "Bot Token").
    pub label: String,
    /// Description / help text.
    pub description: String,
    /// URL where the user can obtain this credential.
    pub provider_hint: Option<String>,
    /// Env var to auto-fill from (e.g., "DISCORD_BOT_TOKEN").
    pub preferred_env_var: Option<String>,
    /// Whether this credential is required (vs optional).
    pub required: bool,
    /// Whether to mask input (for secrets).
    pub secret: bool,
    /// Validation regex pattern (applied to user input).
    pub validation_pattern: Option<String>,
    /// Validation error message.
    pub validation_message: Option<String>,
}

/// A text input step in the setup wizard.
///
/// For non-secret configuration values (guild IDs, phone numbers, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextInputStep {
    /// Unique key.
    pub input_key: String,
    /// Prompt message.
    pub message: String,
    /// Description / help text.
    pub description: Option<String>,
    /// Whether this input is required.
    pub required: bool,
    /// Validation regex pattern.
    pub validation_pattern: Option<String>,
    /// Validation error message.
    pub validation_message: Option<String>,
    /// Default value.
    pub default_value: Option<String>,
}

/// An environment variable shortcut.
///
/// Allows quick "use this env var" actions in the wizard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvShortcut {
    /// The credential key this shortcut fills.
    pub target_key: String,
    /// Env var name.
    pub env_var: String,
    /// Display label (e.g., "Use DISCORD_BOT_TOKEN from environment").
    pub label: String,
}

/// A contextual note shown during setup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupNote {
    /// Note content (supports markdown).
    pub content: String,
    /// When to show this note.
    pub condition: NoteCondition,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteCondition {
    /// Always show.
    Always,
    /// Show only if a specific credential is missing.
    MissingCredential(String),
    /// Show only if a binary is not found.
    MissingBinary(String),
    /// Show on first setup only.
    FirstSetup,
}

/// The channel setup surface trait.
///
/// Each channel that supports wizard-based setup implements this trait.
/// The wizard runtime calls these methods to discover what steps to present.
///
/// ## Design invariants
///
/// 1. **Pure data**: `credentials()`, `text_inputs()`, `env_shortcuts()` are
///    pure functions — no I/O, no side effects. They declare what the channel needs.
///
/// 2. **Inspection is cheap**: `status()` may do light I/O (check if config exists)
///    but must not make network calls or block.
///
/// 3. **Application is atomic**: `apply_credential()` and `apply_text_input()`
///    either succeed fully or return an error — no partial state.
#[async_trait::async_trait]
pub trait ChannelSetupSurface: Send + Sync {
    /// Channel identifier (e.g., "discord", "telegram").
    fn channel_id(&self) -> &str;

    /// Display name for the wizard.
    fn display_name(&self) -> &str;

    /// Credential steps required by this channel.
    fn credentials(&self) -> Vec<CredentialStep>;

    /// Text input steps required by this channel.
    fn text_inputs(&self) -> Vec<TextInputStep>;

    /// Env var shortcuts available.
    fn env_shortcuts(&self) -> Vec<EnvShortcut>;

    /// Contextual notes.
    fn notes(&self) -> Vec<SetupNote> {
        Vec::new()
    }

    /// Current setup status (inspect existing config).
    async fn status(&self) -> SetupStatus;

    /// Apply a credential value from user input.
    async fn apply_credential(&self, key: &str, value: &str) -> Result<(), String>;

    /// Apply a text input value.
    async fn apply_text_input(&self, key: &str, value: &str) -> Result<(), String>;

    /// Apply an env var shortcut (read from env, store in config).
    async fn apply_env_shortcut(&self, target_key: &str) -> Result<(), String>;
}

/// Registry of channel setup surfaces.
pub struct SetupSurfaceRegistry {
    surfaces: Vec<Box<dyn ChannelSetupSurface>>,
}

impl SetupSurfaceRegistry {
    pub fn new() -> Self {
        Self { surfaces: Vec::new() }
    }

    pub fn register(&mut self, surface: Box<dyn ChannelSetupSurface>) {
        tracing::info!(
            channel = surface.channel_id(),
            "registered channel setup surface"
        );
        self.surfaces.push(surface);
    }

    pub fn get(&self, channel_id: &str) -> Option<&dyn ChannelSetupSurface> {
        self.surfaces
            .iter()
            .find(|s| s.channel_id() == channel_id)
            .map(|s| s.as_ref())
    }

    pub fn list(&self) -> Vec<&str> {
        self.surfaces.iter().map(|s| s.channel_id()).collect()
    }

    /// Get status of all channels.
    pub async fn all_status(&self) -> Vec<(String, SetupStatus)> {
        let mut result = Vec::new();
        for surface in &self.surfaces {
            let status = surface.status().await;
            result.push((surface.channel_id().to_string(), status));
        }
        result
    }
}

impl Default for SetupSurfaceRegistry {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_state_ordering() {
        // Not a formal ordering, but verify the states exist and are distinct.
        assert_ne!(SetupState::NotConfigured, SetupState::Configured);
        assert_ne!(SetupState::Partial, SetupState::Degraded);
    }

    #[test]
    fn credential_step_builder() {
        let step = CredentialStep {
            input_key: "bot_token".into(),
            label: "Bot Token".into(),
            description: "Your Discord bot token".into(),
            provider_hint: Some("https://discord.com/developers/applications".into()),
            preferred_env_var: Some("DISCORD_BOT_TOKEN".into()),
            required: true,
            secret: true,
            validation_pattern: Some(r"^[A-Za-z0-9._-]+$".into()),
            validation_message: Some("Invalid token format".into()),
        };
        assert!(step.required);
        assert!(step.secret);
    }
}
