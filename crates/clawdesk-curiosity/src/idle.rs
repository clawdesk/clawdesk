//! Idle scanner — the autonomous loop that runs during agent downtime.
//!
//! Combines curiosity engine with procedural memory consolidation
//! and channel monitoring into a single idle-time scan.

use crate::engine::CuriosityEngine;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// Configuration for the idle scanner.
#[derive(Debug, Clone)]
pub struct IdleConfig {
    /// Minimum idle time before triggering a scan (seconds).
    pub min_idle_secs: u64,
    /// Maximum actions per idle tick.
    pub max_actions_per_tick: usize,
}

impl Default for IdleConfig {
    fn default() -> Self {
        Self {
            min_idle_secs: 300, // 5 minutes of inactivity
            max_actions_per_tick: 5,
        }
    }
}

/// An action the idle scanner wants to perform.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum IdleAction {
    /// Explore an information gap.
    Explore {
        gap_id: String,
        question: String,
        suggested_tools: Vec<String>,
    },
    /// Consolidate procedural memory patterns.
    ConsolidateMemory,
    /// Check for unread messages in a channel.
    CheckChannel {
        channel_name: String,
    },
    /// Run a health check on a system component.
    HealthCheck {
        component: String,
    },
}

/// The idle scanner — orchestrates what happens when the agent has no tasks.
pub struct IdleScanner {
    config: IdleConfig,
}

impl IdleScanner {
    pub fn new(config: IdleConfig) -> Self {
        Self { config }
    }

    /// Run a single idle tick. Returns actions to perform.
    ///
    /// The caller (typically the cron executor) is responsible for
    /// actually executing these actions.
    ///
    /// # Arguments
    /// - `curiosity`: the curiosity engine for gap-based exploration
    /// - `idle_secs`: how long the agent has been idle
    /// - `has_procedural_memory`: whether procedural memory needs consolidation
    /// - `unread_channels`: channels with unread messages
    pub fn tick(
        &self,
        curiosity: &mut CuriosityEngine,
        idle_secs: u64,
        has_procedural_memory: bool,
        unread_channels: &[String],
    ) -> Vec<IdleAction> {
        if idle_secs < self.config.min_idle_secs {
            return vec![];
        }

        let mut actions = Vec::new();

        // 1. Curiosity explorations (highest priority idle action)
        let explorations = curiosity.tick();
        for task in explorations {
            if actions.len() >= self.config.max_actions_per_tick {
                break;
            }
            actions.push(IdleAction::Explore {
                gap_id: task.gap_id,
                question: task.question,
                suggested_tools: task.suggested_tools,
            });
        }

        // 2. Procedural memory consolidation (like sleep consolidation)
        if has_procedural_memory && actions.len() < self.config.max_actions_per_tick {
            actions.push(IdleAction::ConsolidateMemory);
        }

        // 3. Check unread channels
        for channel in unread_channels {
            if actions.len() >= self.config.max_actions_per_tick {
                break;
            }
            actions.push(IdleAction::CheckChannel {
                channel_name: channel.clone(),
            });
        }

        if !actions.is_empty() {
            info!(
                action_count = actions.len(),
                idle_secs,
                "idle scanner: running tick"
            );
        }

        actions
    }
}

impl Default for IdleScanner {
    fn default() -> Self {
        Self::new(IdleConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::budget::ExplorationBudget;
    use crate::engine::CuriosityConfig;
    use crate::gaps::{GapPriority, GapSource};

    #[test]
    fn no_action_when_not_idle_enough() {
        let scanner = IdleScanner::default();
        let mut curiosity = CuriosityEngine::default();
        let actions = scanner.tick(&mut curiosity, 60, false, &[]); // only 1 min idle
        assert!(actions.is_empty());
    }

    #[test]
    fn consolidation_during_idle() {
        let scanner = IdleScanner::default();
        let mut curiosity = CuriosityEngine::default();
        let actions = scanner.tick(&mut curiosity, 600, true, &[]); // 10 min idle
        assert!(actions.iter().any(|a| matches!(a, IdleAction::ConsolidateMemory)));
    }

    #[test]
    fn channel_check_during_idle() {
        let scanner = IdleScanner::default();
        let mut curiosity = CuriosityEngine::default();
        let actions = scanner.tick(&mut curiosity, 600, false, &["slack".into()]);
        assert!(actions.iter().any(|a| matches!(a, IdleAction::CheckChannel { .. })));
    }

    #[test]
    fn exploration_during_idle() {
        let scanner = IdleScanner::default();
        let mut curiosity = CuriosityEngine::default();
        curiosity.register_gap(
            "Is the build passing?",
            GapPriority::High,
            GapSource::StaleEntity { entity_id: "ci".into() },
        );
        let actions = scanner.tick(&mut curiosity, 600, false, &[]);
        assert!(actions.iter().any(|a| matches!(a, IdleAction::Explore { .. })));
    }
}
