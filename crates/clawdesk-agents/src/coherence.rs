//! # Coherence Engineering — What to become while doing
//!
//! Persona-level adaptation and semantic channel-aware behavior that goes
//! beyond formatting to actual personality/style adjustment.
//!
//! ## Persona Adaptation
//!
//! While `IdentityContract` (Layer 4) ensures the persona hash is immutable
//! and agent-unwritable, the *expression* of that persona can adapt:
//!
//! - **Formality level** adjusts per channel (Slack = casual, Email = formal)
//! - **Verbosity** adjusts per context (debugging = verbose, status = terse)
//! - **Expertise framing** adjusts per audience (developer = technical, PM = high-level)
//!
//! The core persona (goals, values, boundaries) is fixed; only the
//! surface expression varies.
//!
//! ## Channel Semantic Adaptation
//!
//! Beyond markup formatting (already in `ChannelContext`), this module
//! handles behavioral differences:
//!
//! ```text
//! Slack → proactive, emoji, threaded, brief
//! Email → structured, formal, complete, no emoji
//! Discord → community-aware, casual, reaction-friendly
//! Desktop → full-featured, verbose OK, tool-heavy
//! CLI → terse, machine-parseable when piped, color when tty
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Persona Adaptation ─────────────────────────────────────────────────────

/// Adaptive expression parameters. These don't change *who* the agent is,
/// only *how* it expresses itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpressionProfile {
    /// e.g. "casual", "professional", "academic"
    pub formality: FormalityLevel,
    /// e.g. "terse", "normal", "verbose"
    pub verbosity: VerbosityLevel,
    /// e.g. "developer", "manager", "non-technical"
    pub audience: AudienceType,
    /// Whether to use emoji/reactions.
    pub use_emoji: bool,
    /// Whether to proactively suggest next steps.
    pub proactive: bool,
    /// Maximum response length hint (0 = no limit).
    pub max_response_chars: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FormalityLevel {
    Casual,
    Professional,
    Academic,
    Legal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VerbosityLevel {
    Terse,
    Normal,
    Verbose,
    Exhaustive,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AudienceType {
    Developer,
    Manager,
    NonTechnical,
    Mixed,
}

impl Default for ExpressionProfile {
    fn default() -> Self {
        Self {
            formality: FormalityLevel::Professional,
            verbosity: VerbosityLevel::Normal,
            audience: AudienceType::Developer,
            use_emoji: false,
            proactive: true,
            max_response_chars: 0,
        }
    }
}

// ─── Channel Semantic Profiles ───────────────────────────────────────────────

/// Semantic behavior profile for a specific channel type.
/// Goes beyond formatting to actual behavioral adaptation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelBehavior {
    pub channel_type: String,
    pub expression: ExpressionProfile,
    /// Behavioral directives injected into the system prompt.
    pub directives: Vec<String>,
}

/// Registry of channel-specific semantic profiles.
pub struct ChannelAdaptationRegistry {
    profiles: HashMap<String, ChannelBehavior>,
    fallback: ChannelBehavior,
}

impl ChannelAdaptationRegistry {
    /// Create a registry with built-in profiles for all standard channels.
    pub fn with_defaults() -> Self {
        let mut profiles = HashMap::new();

        profiles.insert("slack".into(), ChannelBehavior {
            channel_type: "slack".into(),
            expression: ExpressionProfile {
                formality: FormalityLevel::Casual,
                verbosity: VerbosityLevel::Terse,
                audience: AudienceType::Developer,
                use_emoji: true,
                proactive: true,
                max_response_chars: 3000,
            },
            directives: vec![
                "Keep messages concise — Slack threads are skimmable.".into(),
                "Use emoji reactions (✅, ⚠️, 🔍) to convey status at a glance.".into(),
                "Thread long responses. Don't wall-of-text the main channel.".into(),
                "Tag users with @mention only when action is needed from them.".into(),
            ],
        });

        profiles.insert("email".into(), ChannelBehavior {
            channel_type: "email".into(),
            expression: ExpressionProfile {
                formality: FormalityLevel::Professional,
                verbosity: VerbosityLevel::Verbose,
                audience: AudienceType::Mixed,
                use_emoji: false,
                proactive: false,
                max_response_chars: 0,
            },
            directives: vec![
                "Structure with subject line, greeting, body, and sign-off.".into(),
                "Be complete — the recipient may not have conversational context.".into(),
                "No emoji. No markdown unless the recipient's client renders it.".into(),
                "Include action items at the top, detail below.".into(),
            ],
        });

        profiles.insert("discord".into(), ChannelBehavior {
            channel_type: "discord".into(),
            expression: ExpressionProfile {
                formality: FormalityLevel::Casual,
                verbosity: VerbosityLevel::Normal,
                audience: AudienceType::Developer,
                use_emoji: true,
                proactive: true,
                max_response_chars: 2000,
            },
            directives: vec![
                "Community-friendly tone. Be welcoming to newcomers.".into(),
                "Use code blocks (```lang) for any code.".into(),
                "Respect channel topics — don't go off-topic.".into(),
            ],
        });

        profiles.insert("telegram".into(), ChannelBehavior {
            channel_type: "telegram".into(),
            expression: ExpressionProfile {
                formality: FormalityLevel::Casual,
                verbosity: VerbosityLevel::Terse,
                audience: AudienceType::NonTechnical,
                use_emoji: true,
                proactive: false,
                max_response_chars: 4000,
            },
            directives: vec![
                "Short, mobile-friendly messages.".into(),
                "Use Telegram MarkdownV2 formatting.".into(),
            ],
        });

        profiles.insert("cli".into(), ChannelBehavior {
            channel_type: "cli".into(),
            expression: ExpressionProfile {
                formality: FormalityLevel::Professional,
                verbosity: VerbosityLevel::Terse,
                audience: AudienceType::Developer,
                use_emoji: false,
                proactive: false,
                max_response_chars: 0,
            },
            directives: vec![
                "When stdout is piped, output machine-parseable text (no color, no progress bars).".into(),
                "When stdout is a TTY, use ANSI colors for emphasis.".into(),
                "Prefer structured output (JSON) when the user asks for data.".into(),
            ],
        });

        profiles.insert("desktop".into(), ChannelBehavior {
            channel_type: "desktop".into(),
            expression: ExpressionProfile {
                formality: FormalityLevel::Professional,
                verbosity: VerbosityLevel::Normal,
                audience: AudienceType::Developer,
                use_emoji: false,
                proactive: true,
                max_response_chars: 0,
            },
            directives: vec![
                "Full-featured mode. Use markdown, code blocks, and structured output freely.".into(),
                "Offer next-step suggestions after completing a task.".into(),
                "Use tool calls liberally — the desktop UI renders them well.".into(),
            ],
        });

        let fallback = profiles.get("desktop").cloned().unwrap();

        Self { profiles, fallback }
    }

    /// Get the semantic profile for a channel type.
    pub fn profile_for(&self, channel_type: &str) -> &ChannelBehavior {
        self.profiles.get(channel_type).unwrap_or(&self.fallback)
    }

    /// Generate the system prompt fragment for channel-specific behavior.
    pub fn prompt_fragment(&self, channel_type: &str) -> String {
        let profile = self.profile_for(channel_type);
        let directives = profile.directives.iter()
            .map(|d| format!("- {}", d))
            .collect::<Vec<_>>()
            .join("\n");

        format!(
            "<channel_adaptation channel=\"{}\">\n\
             Formality: {:?}\n\
             Verbosity: {:?}\n\
             Audience: {:?}\n\
             Emoji: {}\n\
             Proactive suggestions: {}\n\
             {}\n\
             \n\
             Behavioral directives:\n\
             {}\n\
             </channel_adaptation>",
            channel_type,
            profile.expression.formality,
            profile.expression.verbosity,
            profile.expression.audience,
            if profile.expression.use_emoji { "yes" } else { "no" },
            if profile.expression.proactive { "yes" } else { "no" },
            if profile.expression.max_response_chars > 0 {
                format!("Max response: {} chars", profile.expression.max_response_chars)
            } else {
                "No response length limit".into()
            },
            directives,
        )
    }
}

impl Default for ChannelAdaptationRegistry {
    fn default() -> Self {
        Self::with_defaults()
    }
}

// ─── Persona Rotation ────────────────────────────────────────────────────────

/// Manages rotating between multiple expression profiles for the same
/// core identity. This is NOT credential rotation (that's ProfileRotator)
/// — this is *behavioral* rotation for variety and engagement.
///
/// Example: an agent might alternate between "concise bullet-point" and
/// "narrative explanation" styles to avoid monotony in long sessions.
#[derive(Debug, Clone)]
pub struct PersonaRotator {
    profiles: Vec<ExpressionProfile>,
    current_index: usize,
    /// Rotate every N turns.
    rotate_every_n_turns: usize,
    turn_count: usize,
}

impl PersonaRotator {
    pub fn new(profiles: Vec<ExpressionProfile>, rotate_every_n_turns: usize) -> Self {
        Self {
            profiles,
            current_index: 0,
            rotate_every_n_turns: rotate_every_n_turns.max(1),
            turn_count: 0,
        }
    }

    /// Get the current active profile.
    pub fn current(&self) -> &ExpressionProfile {
        &self.profiles[self.current_index]
    }

    /// Advance the turn counter and rotate if needed.
    pub fn tick(&mut self) -> &ExpressionProfile {
        self.turn_count += 1;
        if self.turn_count % self.rotate_every_n_turns == 0 && self.profiles.len() > 1 {
            self.current_index = (self.current_index + 1) % self.profiles.len();
        }
        self.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_channel_profiles() {
        let registry = ChannelAdaptationRegistry::with_defaults();
        let slack = registry.profile_for("slack");
        assert_eq!(slack.expression.formality, FormalityLevel::Casual);
        assert!(slack.expression.use_emoji);

        let email = registry.profile_for("email");
        assert_eq!(email.expression.formality, FormalityLevel::Professional);
        assert!(!email.expression.use_emoji);
    }

    #[test]
    fn test_prompt_fragment_generation() {
        let registry = ChannelAdaptationRegistry::with_defaults();
        let frag = registry.prompt_fragment("slack");
        assert!(frag.contains("Casual"));
        assert!(frag.contains("emoji"));
        assert!(frag.contains("channel_adaptation"));
    }

    #[test]
    fn test_persona_rotation() {
        let profiles = vec![
            ExpressionProfile { verbosity: VerbosityLevel::Terse, ..Default::default() },
            ExpressionProfile { verbosity: VerbosityLevel::Verbose, ..Default::default() },
        ];
        let mut rotator = PersonaRotator::new(profiles, 2);
        assert_eq!(rotator.current().verbosity, VerbosityLevel::Terse);
        rotator.tick(); // turn 1
        assert_eq!(rotator.current().verbosity, VerbosityLevel::Terse);
        rotator.tick(); // turn 2 — rotate
        assert_eq!(rotator.current().verbosity, VerbosityLevel::Verbose);
        rotator.tick(); // turn 3
        assert_eq!(rotator.current().verbosity, VerbosityLevel::Verbose);
        rotator.tick(); // turn 4 — rotate back
        assert_eq!(rotator.current().verbosity, VerbosityLevel::Terse);
    }
}
