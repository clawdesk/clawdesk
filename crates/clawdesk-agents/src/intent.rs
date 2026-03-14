//! # Intent Engineering — What to want while doing
//!
//! Explicit goal tracking, intent preservation, and task decomposition
//! that keeps the agent on-track across multi-turn loops.
//!
//! ## Architecture
//!
//! The `IntentTracker` sits between the runner and the LLM. Before each turn:
//! 1. It evaluates goal drift by comparing the current trajectory against
//!    the original intent embedding.
//! 2. If drift exceeds a threshold, it injects a "re-centering" system
//!    message that reminds the agent of the original goal.
//! 3. It tracks task decomposition progress and marks subtasks as complete.
//!
//! ## Integration
//!
//! ```text
//! User prompt
//!   ↓
//! IntentTracker::register_goal(prompt)
//!   ↓
//! ┌─── Agent Loop ──────────────────────────┐
//! │  IntentTracker::pre_turn(history) →      │
//! │    inject re-centering if drift > θ      │
//! │  LLM call                                │
//! │  IntentTracker::post_turn(response) →    │
//! │    update subtask progress               │
//! └──────────────────────────────────────────┘
//!   ↓
//! IntentTracker::summary() → GoalReport
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ─── Goal ────────────────────────────────────────────────────────────────────

/// A top-level goal derived from the user's initial prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    /// Unique goal identifier.
    pub id: String,
    /// The original user intent, verbatim.
    pub original_prompt: String,
    /// One-sentence distilled intent (for re-centering prompts).
    pub distilled_intent: String,
    /// Decomposed subtasks (populated during planning or on-the-fly).
    pub subtasks: Vec<Subtask>,
    /// Current overall status.
    pub status: GoalStatus,
    /// Turn at which the goal was registered.
    pub registered_at_turn: u64,
}

/// Status of a top-level goal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GoalStatus {
    /// Goal is actively being pursued.
    Active,
    /// All subtasks complete.
    Completed,
    /// Agent explicitly abandoned or user cancelled.
    Abandoned,
    /// Goal was superseded by a new user message.
    Superseded,
}

/// A decomposed subtask within a goal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subtask {
    pub id: String,
    pub description: String,
    pub status: SubtaskStatus,
    /// Tool calls associated with this subtask.
    pub tool_calls: Vec<String>,
    /// The turn at which this subtask was completed (if any).
    pub completed_at_turn: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubtaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Skipped,
}

// ─── Drift Detection ─────────────────────────────────────────────────────────

/// Result of a drift check: how far has the agent wandered from the goal?
#[derive(Debug, Clone, Serialize)]
pub struct DriftReport {
    /// Cosine similarity between current trajectory and original intent.
    /// 1.0 = perfectly on track, 0.0 = completely off.
    pub alignment_score: f64,
    /// Whether a re-centering injection is recommended.
    pub needs_recentering: bool,
    /// The number of consecutive turns with low alignment.
    pub drift_streak: u32,
}

// ─── Intent Tracker ──────────────────────────────────────────────────────────

/// Tracks goals, subtasks, and drift across the lifetime of an agent run.
pub struct IntentTracker {
    /// The active goal for this run.
    goal: Option<Goal>,
    /// Drift detection threshold (cosine similarity below this triggers re-centering).
    drift_threshold: f64,
    /// Maximum consecutive drifted turns before escalation.
    max_drift_streak: u32,
    /// Current consecutive drift count.
    drift_streak: u32,
    /// Per-turn alignment scores for post-mortem analysis.
    alignment_history: Vec<f64>,
    /// Current turn counter.
    turn: u64,
}

impl IntentTracker {
    /// Create a new tracker with default thresholds.
    pub fn new() -> Self {
        Self {
            goal: None,
            drift_threshold: 0.65,
            max_drift_streak: 3,
            drift_streak: 0,
            alignment_history: Vec::new(),
            turn: 0,
        }
    }

    /// Create a tracker with custom thresholds.
    pub fn with_thresholds(drift_threshold: f64, max_drift_streak: u32) -> Self {
        Self {
            drift_threshold,
            max_drift_streak,
            ..Self::new()
        }
    }

    /// Register a new goal from the user's prompt.
    pub fn register_goal(&mut self, id: String, prompt: String, distilled: String) {
        self.goal = Some(Goal {
            id,
            original_prompt: prompt,
            distilled_intent: distilled,
            subtasks: Vec::new(),
            status: GoalStatus::Active,
            registered_at_turn: self.turn,
        });
        self.drift_streak = 0;
        self.alignment_history.clear();
    }

    /// Add a subtask to the current goal.
    pub fn add_subtask(&mut self, id: String, description: String) {
        if let Some(ref mut goal) = self.goal {
            goal.subtasks.push(Subtask {
                id,
                description,
                status: SubtaskStatus::Pending,
                tool_calls: Vec::new(),
                completed_at_turn: None,
            });
        }
    }

    /// Mark a subtask as completed.
    pub fn complete_subtask(&mut self, subtask_id: &str) {
        if let Some(ref mut goal) = self.goal {
            if let Some(st) = goal.subtasks.iter_mut().find(|s| s.id == subtask_id) {
                st.status = SubtaskStatus::Completed;
                st.completed_at_turn = Some(self.turn);
            }
            // Check if all subtasks are done.
            if goal.subtasks.iter().all(|s| {
                matches!(s.status, SubtaskStatus::Completed | SubtaskStatus::Skipped)
            }) {
                goal.status = GoalStatus::Completed;
            }
        }
    }

    /// Called before each LLM turn. Returns an optional re-centering message
    /// to inject into the system prompt if the agent has drifted.
    pub fn pre_turn_injection(&mut self, alignment_score: f64) -> Option<String> {
        self.turn += 1;
        self.alignment_history.push(alignment_score);

        if alignment_score < self.drift_threshold {
            self.drift_streak += 1;
        } else {
            self.drift_streak = 0;
            return None;
        }

        let goal = self.goal.as_ref()?;
        if goal.status != GoalStatus::Active {
            return None;
        }

        // Build re-centering message.
        let pending: Vec<&str> = goal
            .subtasks
            .iter()
            .filter(|s| matches!(s.status, SubtaskStatus::Pending | SubtaskStatus::InProgress))
            .map(|s| s.description.as_str())
            .collect();

        let severity = if self.drift_streak >= self.max_drift_streak {
            "CRITICAL"
        } else {
            "NOTICE"
        };

        let remaining = if pending.is_empty() {
            String::new()
        } else {
            format!("\n\nRemaining subtasks:\n{}", pending.iter()
                .enumerate()
                .map(|(i, d)| format!("  {}. {}", i + 1, d))
                .collect::<Vec<_>>()
                .join("\n"))
        };

        Some(format!(
            "<intent_recentering severity=\"{}\">\n\
             You are drifting from the original goal.\n\
             Original intent: {}\n\
             Alignment: {:.0}% (threshold: {:.0}%)\n\
             Drift streak: {} consecutive turns{}\n\
             \n\
             Re-focus on the original intent before continuing.\n\
             </intent_recentering>",
            severity,
            goal.distilled_intent,
            alignment_score * 100.0,
            self.drift_threshold * 100.0,
            self.drift_streak,
            remaining,
        ))
    }

    /// Called after each LLM turn with the tool calls made.
    pub fn post_turn(&mut self, tool_names: &[String]) {
        if let Some(ref mut goal) = self.goal {
            // Associate tool calls with in-progress subtasks.
            for st in goal.subtasks.iter_mut() {
                if st.status == SubtaskStatus::InProgress {
                    st.tool_calls.extend(tool_names.iter().cloned());
                }
            }
        }
    }

    /// Get a drift report for the current state.
    pub fn drift_report(&self) -> DriftReport {
        let latest = self.alignment_history.last().copied().unwrap_or(1.0);
        DriftReport {
            alignment_score: latest,
            needs_recentering: latest < self.drift_threshold,
            drift_streak: self.drift_streak,
        }
    }

    /// Get the current goal (if any).
    pub fn goal(&self) -> Option<&Goal> {
        self.goal.as_ref()
    }

    /// Get the full alignment history for post-mortem analysis.
    pub fn alignment_history(&self) -> &[f64] {
        &self.alignment_history
    }

    /// Progress percentage: completed subtasks / total subtasks.
    pub fn progress_pct(&self) -> f64 {
        match &self.goal {
            Some(g) if !g.subtasks.is_empty() => {
                let done = g.subtasks.iter()
                    .filter(|s| matches!(s.status, SubtaskStatus::Completed | SubtaskStatus::Skipped))
                    .count();
                done as f64 / g.subtasks.len() as f64
            }
            _ => 0.0,
        }
    }
}

impl Default for IntentTracker {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Task Decomposition ──────────────────────────────────────────────────────

/// A structured task decomposition request that can be sent to the LLM
/// to break a complex goal into subtasks.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecompositionRequest {
    pub goal: String,
    pub max_subtasks: usize,
    pub context_hints: Vec<String>,
}

/// The LLM's response to a decomposition request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecompositionResult {
    pub subtasks: Vec<DecomposedStep>,
    pub estimated_turns: usize,
    pub confidence: f64,
}

/// A single step in a decomposed plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DecomposedStep {
    pub id: String,
    pub description: String,
    pub depends_on: Vec<String>,
    pub estimated_tool_calls: Vec<String>,
    pub parallelizable: bool,
}

/// Build the system prompt fragment that instructs the LLM to decompose a task.
pub fn decomposition_prompt(request: &DecompositionRequest) -> String {
    let hints = if request.context_hints.is_empty() {
        String::new()
    } else {
        format!(
            "\n\nContext:\n{}",
            request.context_hints.iter()
                .map(|h| format!("- {}", h))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    format!(
        "<task_decomposition>\n\
         Break the following goal into at most {} concrete, actionable subtasks.\n\
         For each subtask, specify:\n\
         - A short description\n\
         - Dependencies (which subtasks must complete first)\n\
         - Which tools you expect to use\n\
         - Whether it can run in parallel with other subtasks\n\
         \n\
         Goal: {}{}\n\
         \n\
         Respond with a JSON array of subtasks.\n\
         </task_decomposition>",
        request.max_subtasks, request.goal, hints,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_drift_detection() {
        let mut tracker = IntentTracker::with_thresholds(0.7, 3);
        tracker.register_goal(
            "g1".into(),
            "Build a todo app".into(),
            "Create a complete todo application with CRUD operations".into(),
        );
        tracker.add_subtask("s1".into(), "Create package.json".into());
        tracker.add_subtask("s2".into(), "Create server.js".into());

        // Good alignment — no injection.
        assert!(tracker.pre_turn_injection(0.9).is_none());

        // Drift — should inject.
        let msg = tracker.pre_turn_injection(0.4);
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("NOTICE"));

        // Continued drift — escalate.
        let _ = tracker.pre_turn_injection(0.3);
        let msg = tracker.pre_turn_injection(0.2);
        assert!(msg.unwrap().contains("CRITICAL"));
    }

    #[test]
    fn test_subtask_progress() {
        let mut tracker = IntentTracker::new();
        tracker.register_goal("g1".into(), "test".into(), "test".into());
        tracker.add_subtask("s1".into(), "step 1".into());
        tracker.add_subtask("s2".into(), "step 2".into());

        assert_eq!(tracker.progress_pct(), 0.0);
        tracker.complete_subtask("s1");
        assert!((tracker.progress_pct() - 0.5).abs() < f64::EPSILON);
        tracker.complete_subtask("s2");
        assert!((tracker.progress_pct() - 1.0).abs() < f64::EPSILON);
        assert_eq!(tracker.goal().unwrap().status, GoalStatus::Completed);
    }
}
