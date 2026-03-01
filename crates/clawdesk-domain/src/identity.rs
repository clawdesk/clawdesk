//! # Identity & Soul Configuration — Bayesian Preference Evolution
//!
//! Structured `Identity` model with channel-conditional personality parameters
//! that evolve via Bayesian updates from conversation feedback signals.
//!
//! ## Model
//!
//! Each personality parameter θ_k ~ Beta(α_k, β_k):
//! - User sends long message → α_verbosity += 1
//! - User says "too long" → β_verbosity += 1
//! - Posterior mean: E[θ_k] = α_k / (α_k + β_k)
//!
//! ## Forgetting Factor
//!
//! ```text
//! α_k ← γ · α_k + (1 - γ) · α_prior
//! β_k ← γ · β_k + (1 - γ) · β_prior
//! ```
//!
//! γ = 0.995 → effective sample size ≈ 200 interactions.
//!
//! ## Channel-Conditional
//!
//! Separate (α, β) pairs per (parameter, channel).
//! 5 params × 22 channels × 2 = 220 floats — one SochDB row.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Personality Parameters ──────────────────────────────────────────────────

/// The set of personality dimensions modeled as Beta distributions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PersonalityDimension {
    /// How detailed/long responses should be (0=terse, 1=verbose)
    Verbosity,
    /// How much humor to inject (0=serious, 1=humorous)
    Humor,
    /// Communication register (0=casual, 1=formal)
    Formality,
    /// How much detail/explanation (0=just answer, 1=teach)
    Didacticism,
    /// Emotional tone (0=neutral/clinical, 1=warm/empathetic)
    Warmth,
}

impl PersonalityDimension {
    /// All dimensions for iteration.
    pub const ALL: [PersonalityDimension; 5] = [
        PersonalityDimension::Verbosity,
        PersonalityDimension::Humor,
        PersonalityDimension::Formality,
        PersonalityDimension::Didacticism,
        PersonalityDimension::Warmth,
    ];
}

/// Beta distribution parameters for a single personality dimension.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BetaParam {
    /// Alpha (positive evidence count)
    pub alpha: f64,
    /// Beta (negative evidence count)
    pub beta: f64,
}

impl BetaParam {
    /// Default uninformative prior: Beta(2, 2) centered at 0.5.
    pub fn uninformative() -> Self {
        Self {
            alpha: 2.0,
            beta: 2.0,
        }
    }

    /// Posterior mean: E[θ] = α / (α + β).
    pub fn mean(&self) -> f64 {
        if self.alpha + self.beta <= 0.0 {
            return 0.5;
        }
        self.alpha / (self.alpha + self.beta)
    }

    /// Posterior variance: Var[θ] = αβ / ((α+β)²(α+β+1)).
    pub fn variance(&self) -> f64 {
        let sum = self.alpha + self.beta;
        if sum <= 0.0 {
            return 0.25;
        }
        (self.alpha * self.beta) / (sum * sum * (sum + 1.0))
    }

    /// Record a positive observation (user appreciates this trait).
    pub fn observe_positive(&mut self) {
        self.alpha += 1.0;
    }

    /// Record a negative observation (user dislikes this trait).
    pub fn observe_negative(&mut self) {
        self.beta += 1.0;
    }

    /// Apply forgetting factor to prevent distribution from freezing.
    ///
    /// ```text
    /// α ← γ · α + (1 - γ) · α_prior
    /// β ← γ · β + (1 - γ) · β_prior
    /// ```
    pub fn apply_forgetting(&mut self, gamma: f64, prior: &BetaParam) {
        self.alpha = gamma * self.alpha + (1.0 - gamma) * prior.alpha;
        self.beta = gamma * self.beta + (1.0 - gamma) * prior.beta;
    }

    /// Effective sample size: n_eff ≈ α + β - α_prior - β_prior.
    pub fn effective_sample_size(&self, prior: &BetaParam) -> f64 {
        (self.alpha - prior.alpha) + (self.beta - prior.beta)
    }
}

impl Default for BetaParam {
    fn default() -> Self {
        Self::uninformative()
    }
}

// ── Identity Model ──────────────────────────────────────────────────────────

/// The agent's full identity configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    /// Agent's name
    pub name: String,
    /// Background description (equivalent to the identity.md)
    pub background: String,
    /// Soul description — personality narrative
    pub soul: String,
    /// Global (channel-agnostic) personality parameters
    pub global_personality: HashMap<PersonalityDimension, BetaParam>,
    /// Channel-specific personality overrides
    /// Key: channel name (e.g., "telegram", "slack")
    pub channel_personality: HashMap<String, HashMap<PersonalityDimension, BetaParam>>,
    /// Forgetting factor γ (default: 0.995 → ~200 interaction memory)
    pub forgetting_factor: f64,
    /// Custom key-value personality traits (free-form)
    pub custom_traits: HashMap<String, String>,
    /// Topics the agent should never discuss
    pub guardrails: Vec<String>,
    /// Preferred language
    pub language: String,
}

impl Identity {
    /// Create a new identity with default uninformative priors.
    pub fn new(name: impl Into<String>, background: impl Into<String>) -> Self {
        let mut global = HashMap::new();
        for dim in PersonalityDimension::ALL {
            global.insert(dim, BetaParam::uninformative());
        }

        Self {
            name: name.into(),
            background: background.into(),
            soul: String::new(),
            global_personality: global,
            channel_personality: HashMap::new(),
            forgetting_factor: 0.995,
            custom_traits: HashMap::new(),
            guardrails: Vec::new(),
            language: "en".into(),
        }
    }

    /// Get the effective personality parameter for a dimension,
    /// optionally conditioned on channel.
    pub fn personality(&self, dim: PersonalityDimension, channel: Option<&str>) -> f64 {
        if let Some(ch) = channel {
            if let Some(ch_params) = self.channel_personality.get(ch) {
                if let Some(param) = ch_params.get(&dim) {
                    return param.mean();
                }
            }
        }
        self.global_personality
            .get(&dim)
            .map(|p| p.mean())
            .unwrap_or(0.5)
    }

    /// Record a feedback signal for a personality dimension.
    ///
    /// Updates the channel-specific parameter if channel is provided,
    /// otherwise updates the global parameter.
    pub fn record_feedback(
        &mut self,
        dim: PersonalityDimension,
        positive: bool,
        channel: Option<&str>,
    ) {
        let param = if let Some(ch) = channel {
            self.channel_personality
                .entry(ch.to_string())
                .or_insert_with(|| {
                    let mut m = HashMap::new();
                    for d in PersonalityDimension::ALL {
                        m.insert(d, BetaParam::uninformative());
                    }
                    m
                })
                .entry(dim)
                .or_insert_with(BetaParam::uninformative)
        } else {
            self.global_personality
                .entry(dim)
                .or_insert_with(BetaParam::uninformative)
        };

        if positive {
            param.observe_positive();
        } else {
            param.observe_negative();
        }
    }

    /// Apply forgetting factor across all parameters.
    /// Call periodically (e.g., daily) to prevent distribution freezing.
    pub fn apply_forgetting(&mut self) {
        let prior = BetaParam::uninformative();
        let gamma = self.forgetting_factor;

        for param in self.global_personality.values_mut() {
            param.apply_forgetting(gamma, &prior);
        }
        for ch_params in self.channel_personality.values_mut() {
            for param in ch_params.values_mut() {
                param.apply_forgetting(gamma, &prior);
            }
        }
    }

    /// Generate a prompt fragment describing the current personality.
    ///
    /// ```text
    /// "Respond with verbosity 0.6/1.0, humor 0.3/1.0, formality 0.8/1.0, ..."
    /// ```
    pub fn prompt_fragment(&self, channel: Option<&str>) -> String {
        let v = self.personality(PersonalityDimension::Verbosity, channel);
        let h = self.personality(PersonalityDimension::Humor, channel);
        let f = self.personality(PersonalityDimension::Formality, channel);
        let d = self.personality(PersonalityDimension::Didacticism, channel);
        let w = self.personality(PersonalityDimension::Warmth, channel);

        format!(
            "Adapt your response style: verbosity {v:.1}/1.0, humor {h:.1}/1.0, \
             formality {f:.1}/1.0, detail {d:.1}/1.0, warmth {w:.1}/1.0",
        )
    }

    /// Full system prompt identity section.
    pub fn system_prompt_section(&self, channel: Option<&str>) -> String {
        let mut parts = Vec::new();

        if !self.name.is_empty() {
            parts.push(format!("You are {}.", self.name));
        }
        if !self.background.is_empty() {
            parts.push(self.background.clone());
        }
        if !self.soul.is_empty() {
            parts.push(self.soul.clone());
        }
        parts.push(self.prompt_fragment(channel));

        if !self.guardrails.is_empty() {
            parts.push(format!(
                "Never discuss: {}.",
                self.guardrails.join(", ")
            ));
        }

        parts.join("\n\n")
    }
}

// ── Feedback Signal Detection ───────────────────────────────────────────────

/// Heuristic signals extracted from user messages that update personality.
#[derive(Debug, Clone)]
pub struct FeedbackSignal {
    pub dimension: PersonalityDimension,
    pub positive: bool,
    pub confidence: f64,
}

/// Extract personality feedback signals from a user message.
///
/// Heuristic rules (extensible):
/// - "too long" / "tldr" → Verbosity negative
/// - Long user message (>200 chars) → Verbosity positive
/// - "be more formal" → Formality positive
/// - "relax" / "chill" → Formality negative
/// - "lol" / "haha" → Humor positive
/// - "be serious" → Humor negative
pub fn extract_feedback_signals(user_message: &str) -> Vec<FeedbackSignal> {
    let lower = user_message.to_lowercase();
    let mut signals = Vec::new();

    // Verbosity signals
    if lower.contains("too long")
        || lower.contains("tldr")
        || lower.contains("tl;dr")
        || lower.contains("shorter")
        || lower.contains("be brief")
    {
        signals.push(FeedbackSignal {
            dimension: PersonalityDimension::Verbosity,
            positive: false,
            confidence: 0.9,
        });
    }
    if user_message.len() > 200 {
        signals.push(FeedbackSignal {
            dimension: PersonalityDimension::Verbosity,
            positive: true,
            confidence: 0.3,
        });
    }

    // Formality signals
    if lower.contains("be more formal") || lower.contains("professionally") {
        signals.push(FeedbackSignal {
            dimension: PersonalityDimension::Formality,
            positive: true,
            confidence: 0.8,
        });
    }
    if lower.contains("relax") || lower.contains("chill") || lower.contains("casual") {
        signals.push(FeedbackSignal {
            dimension: PersonalityDimension::Formality,
            positive: false,
            confidence: 0.7,
        });
    }

    // Humor signals
    if lower.contains("lol") || lower.contains("haha") || lower.contains("😂") {
        signals.push(FeedbackSignal {
            dimension: PersonalityDimension::Humor,
            positive: true,
            confidence: 0.4,
        });
    }
    if lower.contains("be serious") || lower.contains("not funny") {
        signals.push(FeedbackSignal {
            dimension: PersonalityDimension::Humor,
            positive: false,
            confidence: 0.8,
        });
    }

    // Warmth signals
    if lower.contains("thanks") || lower.contains("thank you") || lower.contains("helpful") {
        signals.push(FeedbackSignal {
            dimension: PersonalityDimension::Warmth,
            positive: true,
            confidence: 0.3,
        });
    }

    signals
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bayesian_update() {
        let mut id = Identity::new("TestBot", "A test agent");
        assert!((id.personality(PersonalityDimension::Verbosity, None) - 0.5).abs() < 0.01);

        // 10 positive signals → verbosity should increase
        for _ in 0..10 {
            id.record_feedback(PersonalityDimension::Verbosity, true, None);
        }
        assert!(id.personality(PersonalityDimension::Verbosity, None) > 0.7);

        // Channel-specific override
        id.record_feedback(PersonalityDimension::Formality, true, Some("slack"));
        id.record_feedback(PersonalityDimension::Formality, true, Some("slack"));
        id.record_feedback(PersonalityDimension::Formality, true, Some("slack"));
        let slack_f = id.personality(PersonalityDimension::Formality, Some("slack"));
        let global_f = id.personality(PersonalityDimension::Formality, None);
        assert!(slack_f > global_f, "Slack formality should be higher");
    }

    #[test]
    fn forgetting_factor() {
        let mut id = Identity::new("TestBot", "");
        for _ in 0..100 {
            id.record_feedback(PersonalityDimension::Verbosity, true, None);
        }
        let before = id.personality(PersonalityDimension::Verbosity, None);

        // Apply forgetting many times → should drift back toward 0.5
        for _ in 0..1000 {
            id.apply_forgetting();
        }
        let after = id.personality(PersonalityDimension::Verbosity, None);
        assert!(after < before, "Should decay toward prior");
    }

    #[test]
    fn feedback_extraction() {
        let signals = extract_feedback_signals("This is way too long, give me tldr");
        assert!(!signals.is_empty());
        assert!(signals.iter().any(|s| matches!(s.dimension, PersonalityDimension::Verbosity) && !s.positive));
    }
}
