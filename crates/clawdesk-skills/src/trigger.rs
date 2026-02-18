//! Skill trigger evaluation — runtime activation logic.
//!
//! Evaluates `SkillTrigger` conditions against `TurnContext` to determine
//! which skills should be activated for a given agent turn. This is the
//! missing wire between the skill lifecycle (`SkillState::Active`) and
//! the prompt builder's knapsack selection.
//!
//! ## Design
//!
//! Each `SkillTrigger` variant maps to a pure predicate function:
//!
//! ```text
//! Always    → true
//! Command   → turn contains the exact command
//! Keywords  → keyword overlap ≥ threshold
//! Channel   → turn channel ∈ trigger channel set
//! Schedule  → current time matches cron expression
//! OnDemand  → skill ID ∈ explicitly requested set
//! ```
//!
//! Returns a `TriggerResult` with match status and a relevance score
//! (0.0–1.0) that feeds into the `PromptBuilder`'s knapsack.

use crate::definition::{Skill, SkillId, SkillTrigger};
use chrono::{DateTime, Datelike, Timelike, Utc};
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Turn context
// ---------------------------------------------------------------------------

/// Context for the current agent turn — used to evaluate skill triggers.
///
/// Constructed once per inbound message and passed to trigger evaluation.
/// Intentionally cheap to build (no allocations beyond existing data).
#[derive(Debug, Clone)]
pub struct TurnContext {
    /// Channel identifier (e.g., "telegram", "discord", "webchat").
    pub channel_id: Option<String>,
    /// Extracted keywords from the inbound message (lowercased).
    pub message_keywords: Vec<String>,
    /// The raw message text (for command matching).
    pub message_text: String,
    /// Current UTC timestamp.
    pub current_time: DateTime<Utc>,
    /// Skill IDs explicitly requested by another skill or the user.
    pub requested_skill_ids: Vec<SkillId>,
}

impl TurnContext {
    /// Extract keywords from a message using simple whitespace tokenization.
    ///
    /// Lowercases, removes punctuation, deduplicates. This is intentionally
    /// simple — semantic matching belongs in the embedding layer, not here.
    pub fn extract_keywords(text: &str) -> Vec<String> {
        let mut keywords: Vec<String> = text
            .split_whitespace()
            .map(|w| {
                w.chars()
                    .filter(|c| c.is_alphanumeric())
                    .collect::<String>()
                    .to_lowercase()
            })
            .filter(|w| w.len() > 2) // ignore very short words
            .collect();
        keywords.sort();
        keywords.dedup();
        keywords
    }
}

// ---------------------------------------------------------------------------
// Trigger result
// ---------------------------------------------------------------------------

/// Result of evaluating a skill's triggers against a turn context.
#[derive(Debug, Clone)]
pub struct TriggerResult {
    /// Whether at least one trigger matched.
    pub matched: bool,
    /// Relevance score (0.0–1.0). Higher = more relevant to this turn.
    /// - `Always` → 0.5 (moderate — it's always relevant but not specifically)
    /// - `Command` → 1.0 (exact match)
    /// - `Keywords` → keyword_overlap_ratio (0.0–1.0)
    /// - `Channel` → 0.8 (channel-specific)
    /// - `Schedule` → 0.7 (time-specific)
    /// - `OnDemand` → 1.0 (explicitly requested)
    pub relevance: f64,
    /// Which trigger(s) matched (for debugging/tracing).
    pub matched_triggers: Vec<String>,
}

impl TriggerResult {
    fn no_match() -> Self {
        Self {
            matched: false,
            relevance: 0.0,
            matched_triggers: vec![],
        }
    }
}

// ---------------------------------------------------------------------------
// Trigger evaluator
// ---------------------------------------------------------------------------

/// Evaluates skill triggers against a turn context.
pub struct TriggerEvaluator;

impl TriggerEvaluator {
    /// Evaluate all triggers for a skill against the current turn context.
    ///
    /// Returns `TriggerResult` with the highest relevance score from any
    /// matching trigger. If no triggers match, `matched` is `false`.
    pub fn evaluate(skill: &Skill, ctx: &TurnContext) -> TriggerResult {
        if skill.manifest.triggers.is_empty() {
            // No triggers defined → treat as Always.
            return TriggerResult {
                matched: true,
                relevance: 0.5,
                matched_triggers: vec!["implicit_always".into()],
            };
        }

        let mut best_relevance = 0.0f64;
        let mut matched_triggers = Vec::new();

        for trigger in &skill.manifest.triggers {
            if let Some((relevance, label)) = Self::evaluate_single(trigger, ctx) {
                if relevance > best_relevance {
                    best_relevance = relevance;
                }
                matched_triggers.push(label);
            }
        }

        if matched_triggers.is_empty() {
            TriggerResult::no_match()
        } else {
            TriggerResult {
                matched: true,
                relevance: best_relevance,
                matched_triggers,
            }
        }
    }

    /// Evaluate a single trigger condition. Returns `Some((relevance, label))`
    /// if matched, `None` if not.
    fn evaluate_single(trigger: &SkillTrigger, ctx: &TurnContext) -> Option<(f64, String)> {
        match trigger {
            SkillTrigger::Always => Some((0.5, "always".into())),

            SkillTrigger::Command { command } => {
                // Check if the message starts with the command (e.g., "/search").
                let text = ctx.message_text.trim();
                if text.starts_with(command)
                    || text.starts_with(&format!("/{command}"))
                {
                    Some((1.0, format!("command:{command}")))
                } else {
                    None
                }
            }

            SkillTrigger::Keywords { words, threshold } => {
                if words.is_empty() || ctx.message_keywords.is_empty() {
                    return None;
                }
                let matched_count = words
                    .iter()
                    .filter(|w| {
                        ctx.message_keywords
                            .iter()
                            .any(|k| k.eq_ignore_ascii_case(w))
                    })
                    .count();
                let ratio = matched_count as f64 / words.len() as f64;
                if ratio >= *threshold {
                    Some((ratio, format!("keywords:{matched_count}/{}", words.len())))
                } else {
                    None
                }
            }

            SkillTrigger::Channel { channel_ids } => {
                if let Some(ref ch) = ctx.channel_id {
                    if channel_ids.iter().any(|c| c == ch) {
                        Some((0.8, format!("channel:{ch}")))
                    } else {
                        None
                    }
                } else {
                    None
                }
            }

            SkillTrigger::Schedule { cron_expression } => {
                // Simple cron matching: "H M D" format or "*" wildcards.
                // For production, use a proper cron parser crate.
                if Self::matches_simple_cron(cron_expression, &ctx.current_time) {
                    Some((0.7, format!("schedule:{cron_expression}")))
                } else {
                    None
                }
            }

            SkillTrigger::OnDemand => {
                // Only activate if explicitly requested.
                None
            }
        }
    }

    /// Simple cron matching — supports "H M" (hour minute) and "*" wildcards.
    ///
    /// Format: "minute hour day_of_month month day_of_week"
    /// Each field is a number or "*" (any). Subset of standard cron.
    fn matches_simple_cron(expr: &str, time: &DateTime<Utc>) -> bool {
        let parts: Vec<&str> = expr.split_whitespace().collect();
        if parts.len() < 2 {
            return false;
        }

        let checks: Vec<(u32, &str)> = vec![
            (time.minute(), parts[0]),
            (time.hour(), parts[1]),
        ];

        // Optional day/month/dow fields
        let optional: Vec<(u32, Option<&&str>)> = vec![
            (time.day(), parts.get(2).map(|p| p)),
            (time.month(), parts.get(3).map(|p| p)),
            (time.weekday().num_days_from_sunday(), parts.get(4).map(|p| p)),
        ];

        for (val, pattern) in &checks {
            if *pattern != "*" {
                if let Ok(expected) = pattern.parse::<u32>() {
                    if *val != expected {
                        return false;
                    }
                }
            }
        }

        for (val, maybe_pattern) in &optional {
            if let Some(pattern) = maybe_pattern {
                if **pattern != "*" {
                    if let Ok(expected) = pattern.parse::<u32>() {
                        if *val != expected {
                            return false;
                        }
                    }
                }
            }
        }

        true
    }

    /// Evaluate on-demand triggers — check if the skill was explicitly requested.
    pub fn evaluate_on_demand(skill: &Skill, ctx: &TurnContext) -> TriggerResult {
        let is_requested = ctx
            .requested_skill_ids
            .iter()
            .any(|id| id.as_str() == skill.manifest.id.as_str());

        if is_requested {
            TriggerResult {
                matched: true,
                relevance: 1.0,
                matched_triggers: vec!["on_demand:explicit".into()],
            }
        } else {
            TriggerResult::no_match()
        }
    }

    /// Filter a set of skills to those matching the current turn context.
    ///
    /// Returns `(Arc<Skill>, relevance_score)` pairs for skills that matched
    /// at least one trigger, sorted by relevance descending.
    pub fn filter_matching(
        skills: &[Arc<Skill>],
        ctx: &TurnContext,
    ) -> Vec<(Arc<Skill>, f64)> {
        let mut matched: Vec<(Arc<Skill>, f64)> = skills
            .iter()
            .filter_map(|skill| {
                // First check standard triggers.
                let result = Self::evaluate(skill, ctx);
                if result.matched {
                    return Some((Arc::clone(skill), result.relevance));
                }
                // Then check on-demand.
                let od = Self::evaluate_on_demand(skill, ctx);
                if od.matched {
                    return Some((Arc::clone(skill), od.relevance));
                }
                None
            })
            .collect();

        matched.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        matched
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::*;

    fn make_skill_with_triggers(id: &str, triggers: Vec<SkillTrigger>) -> Skill {
        Skill {
            manifest: SkillManifest {
                id: SkillId::from(id),
                display_name: id.to_string(),
                description: "test".into(),
                version: "0.1.0".into(),
                author: None,
                dependencies: vec![],
                required_tools: vec![],
                parameters: vec![],
                triggers,
                estimated_tokens: 50,
                priority_weight: 1.0,
                tags: vec![],
                signature: None,
                publisher_key: None,
                content_hash: None,
                schema_version: 1,
            },
            prompt_fragment: "Test prompt.".into(),
            provided_tools: vec![],
            parameter_values: serde_json::Value::Null,
            source_path: None,
        }
    }

    fn make_context(text: &str) -> TurnContext {
        TurnContext {
            channel_id: Some("telegram".into()),
            message_keywords: TurnContext::extract_keywords(text),
            message_text: text.into(),
            current_time: Utc::now(),
            requested_skill_ids: vec![],
        }
    }

    #[test]
    fn always_trigger_matches() {
        let skill = make_skill_with_triggers("test/always", vec![SkillTrigger::Always]);
        let ctx = make_context("hello world");
        let result = TriggerEvaluator::evaluate(&skill, &ctx);
        assert!(result.matched);
        assert!((result.relevance - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn command_trigger_matches_exact() {
        let skill = make_skill_with_triggers(
            "test/cmd",
            vec![SkillTrigger::Command {
                command: "search".into(),
            }],
        );
        let ctx = make_context("/search query");
        let result = TriggerEvaluator::evaluate(&skill, &ctx);
        assert!(result.matched);
        assert!((result.relevance - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn command_trigger_no_match() {
        let skill = make_skill_with_triggers(
            "test/cmd",
            vec![SkillTrigger::Command {
                command: "search".into(),
            }],
        );
        let ctx = make_context("hello world");
        let result = TriggerEvaluator::evaluate(&skill, &ctx);
        assert!(!result.matched);
    }

    #[test]
    fn keyword_trigger_above_threshold() {
        let skill = make_skill_with_triggers(
            "test/kw",
            vec![SkillTrigger::Keywords {
                words: vec!["code".into(), "review".into(), "rust".into()],
                threshold: 0.5,
            }],
        );
        let ctx = make_context("please review this rust code");
        let result = TriggerEvaluator::evaluate(&skill, &ctx);
        assert!(result.matched);
        assert!(result.relevance >= 0.5);
    }

    #[test]
    fn keyword_trigger_below_threshold() {
        let skill = make_skill_with_triggers(
            "test/kw",
            vec![SkillTrigger::Keywords {
                words: vec!["code".into(), "review".into(), "rust".into()],
                threshold: 0.9,
            }],
        );
        let ctx = make_context("hello world");
        let result = TriggerEvaluator::evaluate(&skill, &ctx);
        assert!(!result.matched);
    }

    #[test]
    fn channel_trigger_matches() {
        let skill = make_skill_with_triggers(
            "test/ch",
            vec![SkillTrigger::Channel {
                channel_ids: vec!["telegram".into(), "discord".into()],
            }],
        );
        let ctx = make_context("hello");
        let result = TriggerEvaluator::evaluate(&skill, &ctx);
        assert!(result.matched);
        assert!((result.relevance - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn channel_trigger_no_match() {
        let skill = make_skill_with_triggers(
            "test/ch",
            vec![SkillTrigger::Channel {
                channel_ids: vec!["slack".into()],
            }],
        );
        let ctx = make_context("hello");
        let result = TriggerEvaluator::evaluate(&skill, &ctx);
        assert!(!result.matched);
    }

    #[test]
    fn on_demand_trigger_explicit() {
        let skill = make_skill_with_triggers("test/od", vec![SkillTrigger::OnDemand]);
        let mut ctx = make_context("hello");
        ctx.requested_skill_ids.push(SkillId::from("test/od"));
        let result = TriggerEvaluator::evaluate_on_demand(&skill, &ctx);
        assert!(result.matched);
        assert!((result.relevance - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn filter_matching_returns_sorted() {
        let skills: Vec<Arc<Skill>> = vec![
            Arc::new(make_skill_with_triggers(
                "test/always",
                vec![SkillTrigger::Always],
            )),
            Arc::new(make_skill_with_triggers(
                "test/cmd",
                vec![SkillTrigger::Command {
                    command: "search".into(),
                }],
            )),
        ];
        let ctx = make_context("/search hello");
        let matched = TriggerEvaluator::filter_matching(&skills, &ctx);

        assert_eq!(matched.len(), 2);
        // Command match (1.0) should rank before Always (0.5).
        assert_eq!(matched[0].0.manifest.id.as_str(), "test/cmd");
        assert_eq!(matched[1].0.manifest.id.as_str(), "test/always");
    }

    #[test]
    fn extract_keywords_deduplicates() {
        let kw = TurnContext::extract_keywords("hello hello world WORLD test");
        assert!(kw.contains(&"hello".to_string()));
        assert!(kw.contains(&"world".to_string()));
        assert!(kw.contains(&"test".to_string()));
        // Should be deduplicated
        assert_eq!(kw.iter().filter(|k| *k == "hello").count(), 1);
    }
}
