//! Dynamic Skill Orchestrator — per-turn skill selection integrated with
//! the trigger evaluator and token-budgeted selector.
//!
//! ## Dynamic Skills Selection
//!
//! OpenClaw skills are static per-agent: loaded once at startup, never
//! changed during a conversation. ClawDesk's `TriggerEvaluator` + `SkillSelector`
//! already enable per-turn dynamic selection, but the integration was missing.
//!
//! This module provides `SkillOrchestrator` — the glue between:
//! - `TriggerEvaluator`: evaluates which skills are relevant to the current turn
//! - `SkillSelector`: greedy knapsack fitting within token budget
//! - Turn context: user message + conversation history
//!
//! ## Per-turn selection flow
//!
//! ```text
//! User message → TriggerEvaluator (relevance filter)
//!                      ↓
//!              Candidate skills (scored)
//!                      ↓
//!              SkillSelector (token budget fit)
//!                      ↓
//!              Selected skills → System prompt injection
//! ```

use crate::definition::{Skill, SkillId, SkillTrigger};
use crate::selector::{SelectionResult, SkillSelector};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info};

// ═══════════════════════════════════════════════════════════════════════════
// Turn context
// ═══════════════════════════════════════════════════════════════════════════

/// Context for a single conversation turn, used for skill matching.
#[derive(Debug, Clone)]
pub struct TurnContext {
    /// The user's message text.
    pub user_message: String,
    /// Recent conversation history (last N messages).
    pub recent_history: Vec<String>,
    /// Active channel ID.
    pub channel_id: Option<String>,
    /// Current session ID.
    pub session_id: String,
    /// Turn number within the session.
    pub turn_number: u32,
    /// Timestamp of this turn.
    pub timestamp: DateTime<Utc>,
    /// Custom attributes for trigger evaluation.
    pub attributes: HashMap<String, String>,
    /// Memory-derived signal terms for memory→skill feedback.
    ///
    /// Populated from memory recall results before skill selection.
    /// Example: if memory_search found entries about "rust project" and
    /// "database design", those terms boost skills whose triggers match them.
    pub memory_signals: Vec<String>,
}

impl TurnContext {
    pub fn new(session_id: impl Into<String>, user_message: impl Into<String>) -> Self {
        Self {
            user_message: user_message.into(),
            recent_history: vec![],
            channel_id: None,
            session_id: session_id.into(),
            turn_number: 1,
            timestamp: Utc::now(),
            attributes: HashMap::new(),
            memory_signals: vec![],
        }
    }

    pub fn with_turn_number(mut self, n: u32) -> Self {
        self.turn_number = n;
        self
    }

    pub fn with_channel(mut self, ch: impl Into<String>) -> Self {
        self.channel_id = Some(ch.into());
        self
    }

    pub fn with_history(mut self, history: Vec<String>) -> Self {
        self.recent_history = history;
        self
    }

    pub fn with_attribute(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }

    /// Set memory-derived signal terms for memory→skill feedback.
    ///
    /// These terms are matched against skill triggers to boost relevance
    /// when memory context aligns with a skill's domain.
    pub fn with_memory_signals(mut self, signals: Vec<String>) -> Self {
        self.memory_signals = signals;
        self
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Skill selection record (for auditing)
// ═══════════════════════════════════════════════════════════════════════════

/// Record of a per-turn skill selection decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectionRecord {
    /// Session ID.
    pub session_id: String,
    /// Turn number.
    pub turn_number: u32,
    /// Skills that were selected.
    pub selected_skills: Vec<String>,
    /// Skills that were considered but excluded.
    pub excluded_skills: Vec<String>,
    /// Total token cost of selected skills.
    pub total_tokens: usize,
    /// Token budget that was available.
    pub budget: usize,
    /// Number of candidate skills from trigger evaluation.
    pub trigger_candidates: usize,
    /// Timestamp.
    pub timestamp: DateTime<Utc>,
}

// ═══════════════════════════════════════════════════════════════════════════
// Skill orchestrator
// ═══════════════════════════════════════════════════════════════════════════

/// Orchestrates per-turn dynamic skill selection.
///
/// Combines trigger evaluation (which skills are relevant?) with
/// token-budgeted selection (which skills fit?) to produce the
/// optimal skill set for each conversation turn.
pub struct SkillOrchestrator {
    /// All available skills (the full registry).
    skills: Vec<Arc<Skill>>,
    /// Token budget for skill prompt fragments.
    token_budget: usize,
    /// Maximum skills per turn (from TriggerEvaluator).
    max_skills_per_turn: usize,
    /// History of selection decisions (for auditing and adaptation).
    history: Vec<SelectionRecord>,
}

impl SkillOrchestrator {
    /// Create a new orchestrator with available skills.
    pub fn new(skills: Vec<Arc<Skill>>, token_budget: usize) -> Self {
        Self {
            skills,
            token_budget,
            max_skills_per_turn: 8,
            history: Vec::new(),
        }
    }

    /// Set the maximum skills per turn.
    pub fn with_max_skills(mut self, max: usize) -> Self {
        self.max_skills_per_turn = max;
        self
    }

    /// Update the available skill set (e.g., when new skills are loaded).
    pub fn update_skills(&mut self, skills: Vec<Arc<Skill>>) {
        self.skills = skills;
    }

    /// Select skills for a conversation turn.
    ///
    /// ## Algorithm
    /// 1. Evaluate triggers against the turn context to get candidate
    ///    skills with relevance scores.
    /// 2. Sort by relevance, truncate to `max_skills_per_turn`.
    /// 3. Run token-budgeted selection (greedy knapsack) on candidates.
    /// 4. Record the selection decision for auditing.
    pub fn select_for_turn(&mut self, context: &TurnContext) -> SelectionResult {
        // Step 1: Evaluate triggers to find relevant skills
        let candidates = self.evaluate_triggers(context);

        debug!(
            session = %context.session_id,
            turn = context.turn_number,
            candidates = candidates.len(),
            total_skills = self.skills.len(),
            "trigger evaluation complete"
        );

        // Step 2: Truncate to max per turn
        let candidates: Vec<Arc<Skill>> = candidates
            .into_iter()
            .take(self.max_skills_per_turn)
            .collect();

        let trigger_candidates = candidates.len();

        // Step 3: Token-budgeted selection
        let result = SkillSelector::select(&candidates, self.token_budget);

        // Step 4: Record decision
        let record = SelectionRecord {
            session_id: context.session_id.clone(),
            turn_number: context.turn_number,
            selected_skills: result
                .selected
                .iter()
                .map(|s| s.skill.manifest.id.as_str().to_string())
                .collect(),
            excluded_skills: result
                .excluded
                .iter()
                .map(|(id, _)| id.as_str().to_string())
                .collect(),
            total_tokens: result.total_tokens,
            budget: self.token_budget,
            trigger_candidates,
            timestamp: context.timestamp,
        };

        info!(
            session = %context.session_id,
            turn = context.turn_number,
            selected = record.selected_skills.len(),
            tokens = result.total_tokens,
            "skill selection complete"
        );

        self.history.push(record);

        result
    }

    /// Evaluate triggers to find relevant skills for the current turn.
    ///
    /// Returns skills sorted by relevance (highest first).
    fn evaluate_triggers(&self, context: &TurnContext) -> Vec<Arc<Skill>> {
        let mut scored: Vec<(f64, Arc<Skill>)> = self
            .skills
            .iter()
            .filter_map(|skill| {
                let relevance = self.compute_relevance(skill, context);
                if relevance > 0.0 {
                    Some((relevance, Arc::clone(skill)))
                } else {
                    None
                }
            })
            .collect();

        // Sort by relevance descending
        scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        scored.into_iter().map(|(_, skill)| skill).collect()
    }

    /// Compute relevance of a skill to the current turn context.
    ///
    /// Checks each trigger in the skill's manifest against the context.
    /// Returns the maximum trigger match score, boosted by memory signals
    /// when memory-derived terms match the skill's keyword triggers.
    ///
    /// Memory feedback: `final = max(msg_score, α × memory_score)` where α = 0.6.
    /// This ensures memory context can surface skills that the user message
    /// alone wouldn't trigger (e.g., user says "continue" but memory recalls
    /// they were working on a database design task → boosts the database skill).
    fn compute_relevance(&self, skill: &Skill, context: &TurnContext) -> f64 {
        let mut max_score = 0.0f64;
        let msg_lower = context.user_message.to_lowercase();
        let memory_alpha = 0.6; // Memory signal weight relative to direct match

        for trigger in &skill.manifest.triggers {
            let score = match trigger {
                SkillTrigger::Keywords { words, threshold } => {
                    // Primary: match against user message
                    let msg_matched = words
                        .iter()
                        .filter(|w| msg_lower.contains(&w.to_lowercase()))
                        .count();
                    let msg_ratio = if words.is_empty() {
                        0.0
                    } else {
                        msg_matched as f64 / words.len() as f64
                    };
                    let msg_score = if msg_ratio >= *threshold { msg_ratio } else { 0.0 };

                    // Memory feedback: match memory signals against trigger keywords
                    let mem_score = if !context.memory_signals.is_empty() && !words.is_empty() {
                        let mem_text = context.memory_signals.join(" ").to_lowercase();
                        let mem_matched = words
                            .iter()
                            .filter(|w| mem_text.contains(&w.to_lowercase()))
                            .count();
                        let mem_ratio = mem_matched as f64 / words.len() as f64;
                        if mem_ratio >= *threshold {
                            memory_alpha * mem_ratio
                        } else {
                            0.0
                        }
                    } else {
                        0.0
                    };

                    msg_score.max(mem_score)
                }
                SkillTrigger::Command { command } => {
                    if msg_lower.starts_with(&format!("/{}", command.to_lowercase())) {
                        1.0
                    } else {
                        0.0
                    }
                }
                SkillTrigger::Channel { channel_ids } => {
                    if context
                        .channel_id
                        .as_ref()
                        .map_or(false, |ch| channel_ids.contains(ch))
                    {
                        0.8
                    } else {
                        0.0
                    }
                }
                SkillTrigger::Always => 0.5,
                SkillTrigger::Schedule { .. } => 0.0,
                SkillTrigger::OnDemand => 0.0,
            };
            max_score = max_score.max(score);
        }

        max_score
    }

    /// Get the selection history for a session.
    pub fn history_for_session(&self, session_id: &str) -> Vec<&SelectionRecord> {
        self.history
            .iter()
            .filter(|r| r.session_id == session_id)
            .collect()
    }

    /// Get the most recent selection record.
    pub fn last_selection(&self) -> Option<&SelectionRecord> {
        self.history.last()
    }

    /// Clear history older than the given number of records.
    pub fn gc_history(&mut self, max_records: usize) {
        if self.history.len() > max_records {
            let drain = self.history.len() - max_records;
            self.history.drain(..drain);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::SkillManifest;

    fn make_skill(id: &str, keywords: Vec<&str>, token_estimate: usize) -> Arc<Skill> {
        Arc::new(Skill {
            manifest: SkillManifest {
                id: SkillId::from(id),
                display_name: id.to_string(),
                description: format!("Skill {}", id),
                version: "0.1.0".into(),
                author: None,
                triggers: vec![SkillTrigger::Keywords {
                    words: keywords.into_iter().map(String::from).collect(),
                    threshold: 0.3,
                }],
                parameters: vec![],
                dependencies: vec![],
                tags: vec![],
                estimated_tokens: token_estimate,
                priority_weight: 1.0,
                required_tools: vec![],
                signature: None,
                publisher_key: None,
                content_hash: None,
                schema_version: 1,
            },
            prompt_fragment: "x".repeat(token_estimate * 4),
            provided_tools: vec![],
            parameter_values: serde_json::Value::Null,
            source_path: None,
        })
    }

    #[test]
    fn selects_relevant_skills() {
        let skills = vec![
            make_skill("test/search", vec!["search", "find", "look"], 100),
            make_skill("test/code", vec!["code", "program", "implement"], 100),
            make_skill("test/email", vec!["email", "send", "mail"], 100),
        ];

        let mut orchestrator = SkillOrchestrator::new(skills, 5000);
        let context = TurnContext::new("sess-1", "please search for Rust tutorials");

        let result = orchestrator.select_for_turn(&context);

        assert!(!result.selected.is_empty());
        assert!(result
            .selected
            .iter()
            .any(|s| s.skill.manifest.id.as_str() == "test/search"));
    }

    #[test]
    fn respects_token_budget() {
        let skills = vec![
            make_skill("test/a", vec!["test"], 300),
            make_skill("test/b", vec!["test"], 300),
            make_skill("test/c", vec!["test"], 300),
        ];

        let mut orchestrator = SkillOrchestrator::new(skills, 500);
        let context = TurnContext::new("sess-1", "test message");

        let result = orchestrator.select_for_turn(&context);

        // Can't fit all 3 skills (900 tokens) in 500 budget
        assert!(result.total_tokens <= 500);
    }

    #[test]
    fn records_selection_history() {
        let skills = vec![make_skill("test/search", vec!["search"], 100)];
        let mut orchestrator = SkillOrchestrator::new(skills, 5000);

        let ctx1 = TurnContext::new("sess-1", "search something").with_turn_number(1);
        let ctx2 = TurnContext::new("sess-1", "search again").with_turn_number(2);

        orchestrator.select_for_turn(&ctx1);
        orchestrator.select_for_turn(&ctx2);

        let history = orchestrator.history_for_session("sess-1");
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].turn_number, 1);
        assert_eq!(history[1].turn_number, 2);
    }

    #[test]
    fn always_trigger_activates() {
        let skill = Arc::new(Skill {
            manifest: SkillManifest {
                id: SkillId::from("test/always"),
                display_name: "Always On".into(),
                description: "Always active".into(),
                version: "0.1.0".into(),
                author: None,
                triggers: vec![SkillTrigger::Always],
                parameters: vec![],
                dependencies: vec![],
                tags: vec![],
                estimated_tokens: 50,
                priority_weight: 1.0,
                required_tools: vec![],
                signature: None,
                publisher_key: None,
                content_hash: None,
                schema_version: 1,
            },
            prompt_fragment: "Always active".into(),
            provided_tools: vec![],
            parameter_values: serde_json::Value::Null,
            source_path: None,
        });

        let mut orchestrator = SkillOrchestrator::new(vec![skill], 5000);
        let context = TurnContext::new("sess-1", "any message at all");

        let result = orchestrator.select_for_turn(&context);
        assert_eq!(result.selected.len(), 1);
    }
}
