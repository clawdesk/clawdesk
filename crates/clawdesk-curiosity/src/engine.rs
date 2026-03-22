//! Curiosity engine — the core that identifies gaps and plans explorations.

use crate::budget::ExplorationBudget;
use crate::gaps::{GapPriority, GapSource, InformationGap};
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// Configuration for the curiosity engine.
#[derive(Debug, Clone)]
pub struct CuriosityConfig {
    /// Maximum gaps to track simultaneously.
    pub max_tracked_gaps: usize,
    /// Maximum explorations per tick.
    pub max_explorations_per_tick: usize,
    /// Minimum urgency score to trigger exploration.
    pub min_urgency_threshold: f64,
    /// Whether the user is currently active (suppresses exploration).
    pub user_active: bool,
}

impl Default for CuriosityConfig {
    fn default() -> Self {
        Self {
            max_tracked_gaps: 50,
            max_explorations_per_tick: 3,
            min_urgency_threshold: 0.3,
            user_active: false,
        }
    }
}

/// A concrete exploration task to be executed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplorationTask {
    /// Which gap this exploration addresses.
    pub gap_id: String,
    /// The question to investigate.
    pub question: String,
    /// Suggested tool calls to answer the question.
    pub suggested_tools: Vec<String>,
    /// Estimated token cost.
    pub estimated_tokens: u64,
    /// Priority level inherited from the gap.
    pub priority: GapPriority,
}

/// The curiosity engine.
pub struct CuriosityEngine {
    config: CuriosityConfig,
    /// All tracked information gaps.
    gaps: Vec<InformationGap>,
    /// Exploration budget.
    budget: ExplorationBudget,
    /// Counter for gap IDs.
    next_gap_id: u64,
}

impl CuriosityEngine {
    pub fn new(config: CuriosityConfig, budget: ExplorationBudget) -> Self {
        Self {
            config,
            gaps: Vec::new(),
            budget,
            next_gap_id: 0,
        }
    }

    /// Register a new information gap.
    pub fn register_gap(&mut self, question: impl Into<String>, priority: GapPriority, source: GapSource) -> String {
        self.next_gap_id += 1;
        let id = format!("gap_{}", self.next_gap_id);
        let gap = InformationGap::new(id.clone(), question, priority, source);
        self.gaps.push(gap);

        // Evict lowest-priority if over capacity
        if self.gaps.len() > self.config.max_tracked_gaps {
            self.gaps.sort_by(|a, b| b.urgency_score().partial_cmp(&a.urgency_score()).unwrap_or(std::cmp::Ordering::Equal));
            self.gaps.truncate(self.config.max_tracked_gaps);
        }

        id
    }

    /// Mark a gap as resolved.
    pub fn resolve_gap(&mut self, gap_id: &str) {
        self.gaps.retain(|g| g.id != gap_id);
    }

    /// Main tick — evaluate gaps and produce exploration tasks.
    /// Called from the idle scanner or cron system.
    pub fn tick(&mut self) -> Vec<ExplorationTask> {
        // Reset cycle if needed
        self.budget.maybe_reset_cycle();

        // Don't explore while user is active (unless critical)
        if self.config.user_active {
            return self.critical_only();
        }

        // Sort gaps by urgency
        self.gaps.sort_by(|a, b| b.urgency_score().partial_cmp(&a.urgency_score()).unwrap_or(std::cmp::Ordering::Equal));

        // Collect indices of gaps to explore
        let mut explore_indices = Vec::new();
        for (i, gap) in self.gaps.iter().enumerate() {
            if explore_indices.len() >= self.config.max_explorations_per_tick {
                break;
            }
            if gap.in_progress {
                continue;
            }
            if gap.urgency_score() < self.config.min_urgency_threshold {
                break; // sorted — all remaining are below threshold
            }
            if !self.budget.can_explore(gap.estimated_cost_tokens) {
                debug!(gap_id = %gap.id, "curiosity: budget exhausted, skipping");
                break;
            }
            explore_indices.push(i);
        }

        // Build tasks and mark gaps as in-progress
        let mut tasks = Vec::new();
        for idx in explore_indices {
            let gap = &self.gaps[idx];
            let task = Self::plan_exploration_for(gap);
            if self.budget.reserve(gap.estimated_cost_tokens) {
                info!(gap_id = %gap.id, question = %gap.question, "curiosity: exploring");
                self.gaps[idx].in_progress = true;
                tasks.push(task);
            }
        }

        tasks
    }

    /// Only return critical explorations (for when user is active).
    fn critical_only(&mut self) -> Vec<ExplorationTask> {
        let mut explore_indices = Vec::new();
        for (i, gap) in self.gaps.iter().enumerate() {
            if gap.priority == GapPriority::Critical && !gap.in_progress {
                explore_indices.push(i);
            }
        }

        let mut tasks = Vec::new();
        for idx in explore_indices {
            let gap = &self.gaps[idx];
            let task = Self::plan_exploration_for(gap);
            if self.budget.reserve(gap.estimated_cost_tokens) {
                self.gaps[idx].in_progress = true;
                tasks.push(task);
            }
        }
        tasks
    }

    /// Plan a concrete exploration task for a gap (static to avoid borrow issues).
    fn plan_exploration_for(gap: &InformationGap) -> ExplorationTask {
        let suggested_tools = match &gap.source {
            GapSource::StaleEntity { .. } => vec!["read_file".into(), "list_directory".into()],
            GapSource::UnresolvedQuery { .. } => vec!["web_search".into(), "search_files".into()],
            GapSource::AmbiguousResult { tool_name, .. } => vec![tool_name.clone()],
            GapSource::FailedDependency { .. } => vec!["execute_command".into()],
            GapSource::UnreadChannel { .. } => vec!["check_messages".into()],
            GapSource::PredictedNeed { .. } => vec!["search_files".into(), "read_file".into()],
        };

        ExplorationTask {
            gap_id: gap.id.clone(),
            question: gap.question.clone(),
            suggested_tools,
            estimated_tokens: gap.estimated_cost_tokens,
            priority: gap.priority,
        }
    }

    /// Report exploration result — marks gap as resolved or adjusts priority.
    pub fn exploration_completed(&mut self, gap_id: &str, resolved: bool, actual_tokens: u64) {
        self.budget.release(actual_tokens);

        if resolved {
            self.resolve_gap(gap_id);
        } else {
            // Demote priority — if we tried and failed, it's less urgent
            if let Some(gap) = self.gaps.iter_mut().find(|g| g.id == gap_id) {
                gap.in_progress = false;
                if gap.priority > GapPriority::Low {
                    gap.priority = match gap.priority {
                        GapPriority::Critical => GapPriority::High,
                        GapPriority::High => GapPriority::Medium,
                        GapPriority::Medium => GapPriority::Low,
                        GapPriority::Low => GapPriority::Low,
                    };
                }
            }
        }
    }

    /// Set user activity state (suppresses non-critical exploration).
    pub fn set_user_active(&mut self, active: bool) {
        self.config.user_active = active;
    }

    pub fn gap_count(&self) -> usize {
        self.gaps.len()
    }

    pub fn budget_remaining(&self) -> u64 {
        self.budget.remaining()
    }

    pub fn budget_utilization(&self) -> f64 {
        self.budget.utilization()
    }
}

impl Default for CuriosityEngine {
    fn default() -> Self {
        Self::new(CuriosityConfig::default(), ExplorationBudget::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn registers_and_resolves_gaps() {
        let mut engine = CuriosityEngine::default();
        let id = engine.register_gap(
            "Is the CI pipeline passing?",
            GapPriority::Medium,
            GapSource::StaleEntity { entity_id: "ci".into() },
        );
        assert_eq!(engine.gap_count(), 1);
        engine.resolve_gap(&id);
        assert_eq!(engine.gap_count(), 0);
    }

    #[test]
    fn tick_produces_tasks() {
        let mut engine = CuriosityEngine::default();
        engine.register_gap(
            "What's the deployment status?",
            GapPriority::High,
            GapSource::StaleEntity { entity_id: "deploy".into() },
        );
        let tasks = engine.tick();
        assert!(!tasks.is_empty());
        assert_eq!(tasks[0].priority, GapPriority::High);
    }

    #[test]
    fn user_active_suppresses_non_critical() {
        let mut engine = CuriosityEngine::default();
        engine.set_user_active(true);
        engine.register_gap(
            "nice to know",
            GapPriority::Medium,
            GapSource::PredictedNeed { pattern_description: "test".into() },
        );
        let tasks = engine.tick();
        assert!(tasks.is_empty(), "medium priority should be suppressed when user is active");
    }

    #[test]
    fn critical_passes_through_when_active() {
        let mut engine = CuriosityEngine::default();
        engine.set_user_active(true);
        engine.register_gap(
            "server is down!",
            GapPriority::Critical,
            GapSource::StaleEntity { entity_id: "srv".into() },
        );
        let tasks = engine.tick();
        assert!(!tasks.is_empty(), "critical should pass through even when user active");
    }

    #[test]
    fn budget_limits_exploration() {
        let budget = ExplorationBudget::new(10_000); // budget: 1000 tokens (10% of 10k)
        let mut engine = CuriosityEngine::new(CuriosityConfig::default(), budget);

        // Register a gap that fits within budget
        let gap = InformationGap::new(
            "affordable", "simple question", GapPriority::High,
            GapSource::UnresolvedQuery { query: "small".into() },
        ).with_cost(500);
        engine.gaps.push(gap);

        let tasks = engine.tick();
        assert_eq!(tasks.len(), 1, "500 tokens should fit in 1000 token budget");

        // Now register a gap that exceeds remaining budget
        let expensive = InformationGap::new(
            "expensive", "complex question", GapPriority::High,
            GapSource::UnresolvedQuery { query: "big".into() },
        ).with_cost(600);
        engine.gaps.push(expensive);

        let tasks2 = engine.tick();
        assert_eq!(tasks2.len(), 0, "600 tokens should not fit in remaining 500 token budget");
    }

    #[test]
    fn failed_exploration_demotes_priority() {
        let mut engine = CuriosityEngine::default();
        let id = engine.register_gap(
            "question",
            GapPriority::High,
            GapSource::StaleEntity { entity_id: "x".into() },
        );
        engine.exploration_completed(&id, false, 100);
        // Gap should still exist but at Medium
        let gap = engine.gaps.iter().find(|g| g.id == id).unwrap();
        assert_eq!(gap.priority, GapPriority::Medium);
    }
}
