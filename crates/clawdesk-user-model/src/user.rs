//! Per-user model — the top-level coordinator.

use crate::expertise::{Domain, ExpertiseLevel, ExpertiseProfile};
use crate::frustration::{FrustrationDetector, FrustrationLevel};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Hints for how the agent should style its response to this user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StyleHints {
    /// How much to explain (0.0 = just answer, 1.0 = full tutorial).
    pub explanation_depth: f64,
    /// Whether to use concise responses (frustrated user → be direct).
    pub prefer_concise: bool,
    /// Whether to show code examples.
    pub show_examples: bool,
    /// Formality level (0.0 = casual, 1.0 = formal).
    pub formality: f64,
    /// Frustration level (for adapting tone).
    pub frustration: FrustrationLevel,
}

/// A predicted need the user hasn't explicitly asked for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferredNeed {
    /// What we think the user needs.
    pub description: String,
    /// Confidence in this inference.
    pub confidence: f64,
    /// Domain this need relates to.
    pub domain: Domain,
}

/// Per-user model tracking expertise, frustration, and interaction patterns.
pub struct UserModel {
    /// User identifier (opaque string — could be session_id or user_id).
    pub user_id: String,
    /// Expertise profile per domain.
    pub expertise: ExpertiseProfile,
    /// Frustration detector.
    pub frustration: FrustrationDetector,
    /// Satisfaction EWMA (0.0–1.0).
    pub satisfaction_ewma: f64,
    /// Total messages from this user.
    pub message_count: u64,
    /// When this model was created.
    pub created: DateTime<Utc>,
    /// When this model was last updated.
    pub last_updated: DateTime<Utc>,
}

impl UserModel {
    pub fn new(user_id: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            user_id: user_id.into(),
            expertise: ExpertiseProfile::new(),
            frustration: FrustrationDetector::new(),
            satisfaction_ewma: 0.5, // neutral prior
            message_count: 0,
            created: now,
            last_updated: now,
        }
    }

    /// Process a new user message — updates expertise, frustration, etc.
    pub fn update_from_message(&mut self, text: &str) {
        self.message_count += 1;
        self.last_updated = Utc::now();

        // Update expertise profile
        self.expertise.observe_message(text);

        // Update frustration detector
        let frustration = self.frustration.observe(text);

        debug!(
            user = %self.user_id,
            frustration = ?frustration,
            messages = self.message_count,
            "user model updated"
        );
    }

    /// Record a positive interaction (user said thanks, expressed satisfaction).
    pub fn record_positive_feedback(&mut self) {
        const ALPHA: f64 = 0.2;
        self.satisfaction_ewma = ALPHA * 1.0 + (1.0 - ALPHA) * self.satisfaction_ewma;
        self.frustration.reset();
    }

    /// Record a negative interaction (user corrected agent, expressed dissatisfaction).
    pub fn record_negative_feedback(&mut self) {
        const ALPHA: f64 = 0.2;
        self.satisfaction_ewma = ALPHA * 0.0 + (1.0 - ALPHA) * self.satisfaction_ewma;
    }

    /// Whether the user likely needs an explanation for a topic.
    pub fn should_explain(&self, domain: &Domain) -> bool {
        self.expertise.should_explain(domain)
    }

    /// Get style hints for the current response.
    pub fn response_style(&self) -> StyleHints {
        let frustration = self.frustration.level();
        let general_level = self.expertise.level_for(&Domain::General);

        // When frustrated → be more concise and direct
        let (prefer_concise, depth_modifier) = match frustration {
            FrustrationLevel::Critical => (true, 0.3),
            FrustrationLevel::Frustrated => (true, 0.5),
            FrustrationLevel::Rising => (false, 0.8),
            FrustrationLevel::Calm => (false, 1.0),
        };

        StyleHints {
            explanation_depth: general_level.explanation_depth() * depth_modifier,
            prefer_concise,
            show_examples: general_level <= ExpertiseLevel::Intermediate,
            formality: if self.message_count < 5 { 0.6 } else { 0.3 }, // relax over time
            frustration,
        }
    }

    /// Predict what the user might need next based on their profile.
    pub fn predict_needs(&self) -> Vec<InferredNeed> {
        let mut needs = Vec::new();

        // If they're frustrated, they probably need a simpler approach
        if self.frustration.level() >= FrustrationLevel::Frustrated {
            needs.push(InferredNeed {
                description: "Try a simpler or alternative approach — the user is frustrated with the current one.".into(),
                confidence: 0.8,
                domain: Domain::General,
            });
        }

        // If they keep asking about the same domain, they might need a tutorial
        for (domain, expertise) in &self.expertise.domains {
            if expertise.level == ExpertiseLevel::Novice && expertise.interaction_count >= 3 {
                needs.push(InferredNeed {
                    description: format!(
                        "Consider offering a brief overview of {:?} concepts — \
                         this user has asked {} questions at novice level.",
                        domain, expertise.interaction_count,
                    ),
                    confidence: 0.6,
                    domain: domain.clone(),
                });
            }
        }

        needs
    }

    /// Generate a system prompt fragment for user-adapted responses.
    pub fn to_prompt_fragment(&self) -> Option<String> {
        let style = self.response_style();
        let frustration = self.frustration.level();

        // Only inject if there's something non-default to say
        if self.message_count < 3 && frustration == FrustrationLevel::Calm {
            return None; // not enough data yet
        }

        let mut lines = vec!["<user_model>".to_string()];

        match frustration {
            FrustrationLevel::Critical => {
                lines.push("  USER IS VERY FRUSTRATED. Be extremely concise and direct.".into());
                lines.push("  Skip explanations. Just provide the solution.".into());
            }
            FrustrationLevel::Frustrated => {
                lines.push("  User seems frustrated. Be more concise than usual.".into());
            }
            FrustrationLevel::Rising => {
                lines.push("  User may be getting impatient. Keep responses focused.".into());
            }
            FrustrationLevel::Calm => {}
        }

        // Add expertise-based hints
        let expert_domains: Vec<String> = self.expertise.domains.iter()
            .filter(|(_, e)| e.level >= ExpertiseLevel::Advanced)
            .map(|(d, _)| format!("{:?}", d))
            .collect();
        if !expert_domains.is_empty() {
            lines.push(format!(
                "  User is experienced in: {}. Skip basic explanations in these areas.",
                expert_domains.join(", ")
            ));
        }

        let novice_domains: Vec<String> = self.expertise.domains.iter()
            .filter(|(_, e)| e.level == ExpertiseLevel::Novice)
            .map(|(d, _)| format!("{:?}", d))
            .collect();
        if !novice_domains.is_empty() {
            lines.push(format!(
                "  User is new to: {}. Explain concepts clearly with examples.",
                novice_domains.join(", ")
            ));
        }

        lines.push("</user_model>".into());
        Some(lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_user_has_neutral_model() {
        let user = UserModel::new("user-1");
        assert_eq!(user.frustration.level(), FrustrationLevel::Calm);
        assert!((user.satisfaction_ewma - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn expertise_updates_from_messages() {
        let mut user = UserModel::new("user-1");
        user.update_from_message("Help me understand how the borrow checker works with lifetime annotations in Rust");
        assert!(user.expertise.domains.contains_key(&Domain::Rust));
    }

    #[test]
    fn frustration_adapts_style() {
        let mut user = UserModel::new("user-1");
        for _ in 0..3 {
            user.update_from_message("This doesn't work! Still broken!!");
        }
        let style = user.response_style();
        assert!(style.prefer_concise);
        assert!(style.explanation_depth < 0.5);
    }

    #[test]
    fn prompt_fragment_for_expert() {
        let mut user = UserModel::new("user-1");
        for _ in 0..5 {
            user.update_from_message("The monomorphization of this generic with the vtable dispatch causes lifetime variance issues");
        }
        let fragment = user.to_prompt_fragment();
        assert!(fragment.is_some());
        let text = fragment.unwrap();
        assert!(text.contains("experienced"));
    }

    #[test]
    fn no_fragment_for_new_user() {
        let user = UserModel::new("user-1");
        assert!(user.to_prompt_fragment().is_none());
    }

    #[test]
    fn positive_feedback_resets_frustration() {
        let mut user = UserModel::new("user-1");
        user.update_from_message("WRONG!! NOT WHAT I ASKED!!");
        user.update_from_message("Still broken!!");
        assert!(user.frustration.level() >= FrustrationLevel::Rising);
        user.record_positive_feedback();
        assert_eq!(user.frustration.level(), FrustrationLevel::Calm);
    }
}
