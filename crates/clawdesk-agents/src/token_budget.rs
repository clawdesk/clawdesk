//! Per-agent token budget enforcement with sliding window counters.
//!
//! ## Design
//!
//! Each agent has an independent token budget tracked over a sliding time window.
//! The counter uses a ring buffer of `(timestamp, input_tokens, output_tokens)`
//! entries. When a new observation is recorded, expired entries are evicted.
//!
//! ## Budget hierarchy
//!
//! ```text
//! 1. Per-agent TOML budget  (agent.toml → token_budget section)
//! 2. Global default budget  (gateway config)
//! 3. No limit (∞)           (if neither is set)
//! ```
//!
//! ## Enforcement points
//!
//! - **Pre-run check**: Before `AgentRunner::run()`, check if the agent
//!   has remaining budget. If exhausted, reject with `BudgetExhausted`.
//! - **Post-run record**: After `run()` returns, record consumed tokens.
//!
//! ## Thread safety
//!
//! `TokenBudgetManager` uses `DashMap` for lock-free per-agent access.
//! Each `AgentBudget` entry uses a `Mutex<VecDeque>` for the ring buffer
//! (contention is low — one agent has at most a few concurrent runs).

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

/// Global token budget manager — holds per-agent sliding window counters.
pub struct TokenBudgetManager {
    /// Per-agent budget state.
    agents: DashMap<String, AgentBudget>,
    /// Default budget applied when an agent has no explicit config.
    default_config: BudgetConfig,
}

/// Configuration for a single agent's token budget.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// Maximum total tokens (input + output) allowed within the window.
    /// `None` = unlimited.
    pub max_tokens: Option<u64>,
    /// Maximum input tokens within the window. `None` = unlimited.
    pub max_input_tokens: Option<u64>,
    /// Maximum output tokens within the window. `None` = unlimited.
    pub max_output_tokens: Option<u64>,
    /// Sliding window duration.
    #[serde(with = "humantime_duration", default = "default_window")]
    pub window: Duration,
}

fn default_window() -> Duration {
    Duration::from_secs(3600) // 1 hour
}

mod humantime_duration {
    use serde::{self, Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(duration.as_secs())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        let secs = u64::deserialize(deserializer)?;
        Ok(Duration::from_secs(secs))
    }
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            max_tokens: None,
            max_input_tokens: None,
            max_output_tokens: None,
            window: default_window(),
        }
    }
}

/// Outcome of a budget check.
#[derive(Debug, Clone)]
pub enum BudgetVerdict {
    /// Budget allows the operation.
    Allowed {
        remaining_tokens: Option<u64>,
        window_used: u64,
    },
    /// Budget exhausted — operation should be rejected.
    Exhausted {
        limit: u64,
        used: u64,
        window_secs: u64,
        resets_in_secs: u64,
    },
}

impl BudgetVerdict {
    pub fn is_allowed(&self) -> bool {
        matches!(self, BudgetVerdict::Allowed { .. })
    }
}

/// Per-agent budget tracking state.
struct AgentBudget {
    config: BudgetConfig,
    /// Ring buffer of token observations: (timestamp, input, output).
    observations: Mutex<VecDeque<TokenObservation>>,
}

#[derive(Debug, Clone)]
struct TokenObservation {
    timestamp: Instant,
    input_tokens: u64,
    output_tokens: u64,
}

impl TokenBudgetManager {
    /// Create a new budget manager with the given default configuration.
    pub fn new(default_config: BudgetConfig) -> Arc<Self> {
        Arc::new(Self {
            agents: DashMap::new(),
            default_config,
        })
    }

    /// Create a budget manager with no default limits (unlimited).
    pub fn unlimited() -> Arc<Self> {
        Self::new(BudgetConfig::default())
    }

    /// Set the budget configuration for a specific agent.
    ///
    /// If the agent already has observations, they are preserved —
    /// only the limits change.
    pub fn set_agent_budget(&self, agent_id: &str, config: BudgetConfig) {
        if let Some(mut entry) = self.agents.get_mut(agent_id) {
            entry.config = config;
        } else {
            self.agents.insert(
                agent_id.to_string(),
                AgentBudget {
                    config,
                    observations: Mutex::new(VecDeque::new()),
                },
            );
        }
        info!(%agent_id, "agent token budget updated");
    }

    /// Remove a specific agent's budget (reverts to default).
    pub fn remove_agent_budget(&self, agent_id: &str) {
        self.agents.remove(agent_id);
    }

    /// Check if an agent has budget remaining for a new run.
    ///
    /// Call this **before** `AgentRunner::run()`.
    pub fn check(&self, agent_id: &str) -> BudgetVerdict {
        let config = match self.agents.get(agent_id) {
            Some(entry) => entry.config.clone(),
            None => self.default_config.clone(),
        };

        // No limits configured → always allowed.
        if config.max_tokens.is_none()
            && config.max_input_tokens.is_none()
            && config.max_output_tokens.is_none()
        {
            return BudgetVerdict::Allowed {
                remaining_tokens: None,
                window_used: 0,
            };
        }

        let (total_input, total_output, earliest) =
            self.window_totals(agent_id, config.window);
        let total = total_input + total_output;

        // Check total tokens limit.
        if let Some(max) = config.max_tokens {
            if total >= max {
                let resets_in = earliest
                    .map(|e| config.window.saturating_sub(e.elapsed()).as_secs())
                    .unwrap_or(0);
                return BudgetVerdict::Exhausted {
                    limit: max,
                    used: total,
                    window_secs: config.window.as_secs(),
                    resets_in_secs: resets_in,
                };
            }
        }

        // Check input tokens limit.
        if let Some(max) = config.max_input_tokens {
            if total_input >= max {
                let resets_in = earliest
                    .map(|e| config.window.saturating_sub(e.elapsed()).as_secs())
                    .unwrap_or(0);
                return BudgetVerdict::Exhausted {
                    limit: max,
                    used: total_input,
                    window_secs: config.window.as_secs(),
                    resets_in_secs: resets_in,
                };
            }
        }

        // Check output tokens limit.
        if let Some(max) = config.max_output_tokens {
            if total_output >= max {
                let resets_in = earliest
                    .map(|e| config.window.saturating_sub(e.elapsed()).as_secs())
                    .unwrap_or(0);
                return BudgetVerdict::Exhausted {
                    limit: max,
                    used: total_output,
                    window_secs: config.window.as_secs(),
                    resets_in_secs: resets_in,
                };
            }
        }

        let remaining = config.max_tokens.map(|max| max.saturating_sub(total));
        BudgetVerdict::Allowed {
            remaining_tokens: remaining,
            window_used: total,
        }
    }

    /// Record token usage after a run completes.
    ///
    /// Call this **after** `AgentRunner::run()` returns.
    pub fn record(
        &self,
        agent_id: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) {
        let obs = TokenObservation {
            timestamp: Instant::now(),
            input_tokens,
            output_tokens,
        };

        self.agents
            .entry(agent_id.to_string())
            .or_insert_with(|| AgentBudget {
                config: self.default_config.clone(),
                observations: Mutex::new(VecDeque::new()),
            })
            .observations
            .lock()
            .unwrap()
            .push_back(obs);

        debug!(
            %agent_id,
            input_tokens,
            output_tokens,
            "token usage recorded"
        );
    }

    /// Get usage statistics for an agent within the current window.
    pub fn usage(&self, agent_id: &str) -> AgentUsage {
        let config = match self.agents.get(agent_id) {
            Some(entry) => entry.config.clone(),
            None => self.default_config.clone(),
        };

        let (total_input, total_output, _) =
            self.window_totals(agent_id, config.window);

        AgentUsage {
            agent_id: agent_id.to_string(),
            window_secs: config.window.as_secs(),
            input_tokens: total_input,
            output_tokens: total_output,
            total_tokens: total_input + total_output,
            max_tokens: config.max_tokens,
            max_input_tokens: config.max_input_tokens,
            max_output_tokens: config.max_output_tokens,
        }
    }

    /// Get usage for all tracked agents.
    pub fn all_usage(&self) -> Vec<AgentUsage> {
        self.agents
            .iter()
            .map(|entry| self.usage(entry.key()))
            .collect()
    }

    /// Evict expired observations and return (total_input, total_output, earliest_timestamp).
    fn window_totals(
        &self,
        agent_id: &str,
        window: Duration,
    ) -> (u64, u64, Option<Instant>) {
        let entry = match self.agents.get(agent_id) {
            Some(e) => e,
            None => return (0, 0, None),
        };

        let mut obs = entry.observations.lock().unwrap();
        let cutoff = Instant::now() - window;

        // Evict expired entries from the front.
        while obs.front().map_or(false, |o| o.timestamp < cutoff) {
            obs.pop_front();
        }

        let mut total_input = 0u64;
        let mut total_output = 0u64;
        let earliest = obs.front().map(|o| o.timestamp);

        for o in obs.iter() {
            total_input += o.input_tokens;
            total_output += o.output_tokens;
        }

        (total_input, total_output, earliest)
    }
}

/// Usage statistics for a single agent.
#[derive(Debug, Clone, Serialize)]
pub struct AgentUsage {
    pub agent_id: String,
    pub window_secs: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub max_tokens: Option<u64>,
    pub max_input_tokens: Option<u64>,
    pub max_output_tokens: Option<u64>,
}

impl AgentUsage {
    /// How much of the budget has been consumed (0.0 - 1.0+).
    /// Returns `None` if unlimited.
    pub fn utilization(&self) -> Option<f64> {
        self.max_tokens
            .map(|max| self.total_tokens as f64 / max as f64)
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_always_allowed() {
        let mgr = TokenBudgetManager::unlimited();
        mgr.record("agent-1", 1000, 500);
        mgr.record("agent-1", 1000, 500);
        let verdict = mgr.check("agent-1");
        assert!(verdict.is_allowed());
    }

    #[test]
    fn budget_enforced() {
        let config = BudgetConfig {
            max_tokens: Some(5000),
            max_input_tokens: None,
            max_output_tokens: None,
            window: Duration::from_secs(3600),
        };
        let mgr = TokenBudgetManager::new(config);

        mgr.record("agent-1", 2000, 1000);
        assert!(mgr.check("agent-1").is_allowed());

        mgr.record("agent-1", 1500, 800);
        // Total: 5300 > 5000
        assert!(!mgr.check("agent-1").is_allowed());
    }

    #[test]
    fn per_agent_config_overrides_default() {
        let mgr = TokenBudgetManager::new(BudgetConfig {
            max_tokens: Some(1000),
            ..Default::default()
        });

        // Agent-2 gets a higher limit.
        mgr.set_agent_budget("agent-2", BudgetConfig {
            max_tokens: Some(10000),
            ..Default::default()
        });

        mgr.record("agent-1", 600, 500);
        mgr.record("agent-2", 600, 500);

        // agent-1 over default (1100 > 1000), agent-2 under its own limit.
        assert!(!mgr.check("agent-1").is_allowed());
        assert!(mgr.check("agent-2").is_allowed());
    }

    #[test]
    fn input_output_separate_limits() {
        let config = BudgetConfig {
            max_tokens: None,
            max_input_tokens: Some(5000),
            max_output_tokens: Some(2000),
            window: Duration::from_secs(3600),
        };
        let mgr = TokenBudgetManager::new(config);

        mgr.record("agent-1", 4000, 1000);
        assert!(mgr.check("agent-1").is_allowed());

        mgr.record("agent-1", 1500, 500);
        // Input: 5500 > 5000 → exhausted.
        assert!(!mgr.check("agent-1").is_allowed());
    }

    #[test]
    fn usage_stats_correct() {
        let mgr = TokenBudgetManager::new(BudgetConfig {
            max_tokens: Some(10000),
            ..Default::default()
        });

        mgr.record("agent-1", 1000, 500);
        mgr.record("agent-1", 2000, 800);

        let usage = mgr.usage("agent-1");
        assert_eq!(usage.input_tokens, 3000);
        assert_eq!(usage.output_tokens, 1300);
        assert_eq!(usage.total_tokens, 4300);
        assert_eq!(usage.max_tokens, Some(10000));
    }

    #[test]
    fn all_usage_lists_agents() {
        let mgr = TokenBudgetManager::unlimited();
        mgr.record("alpha", 100, 50);
        mgr.record("beta", 200, 100);
        mgr.record("gamma", 300, 150);

        let all = mgr.all_usage();
        assert_eq!(all.len(), 3);
    }

    #[test]
    fn utilization_percentage() {
        let mgr = TokenBudgetManager::new(BudgetConfig {
            max_tokens: Some(10000),
            ..Default::default()
        });

        mgr.record("agent-1", 3000, 2000);

        let usage = mgr.usage("agent-1");
        let util = usage.utilization().unwrap();
        assert!((util - 0.5).abs() < 0.001);
    }

    #[test]
    fn exhausted_verdict_has_reset_info() {
        let config = BudgetConfig {
            max_tokens: Some(100),
            max_input_tokens: None,
            max_output_tokens: None,
            window: Duration::from_secs(3600),
        };
        let mgr = TokenBudgetManager::new(config);
        mgr.record("agent-1", 100, 50);

        match mgr.check("agent-1") {
            BudgetVerdict::Exhausted {
                limit,
                used,
                window_secs,
                resets_in_secs,
            } => {
                assert_eq!(limit, 100);
                assert_eq!(used, 150);
                assert_eq!(window_secs, 3600);
                assert!(resets_in_secs <= 3600);
            }
            _ => panic!("expected exhausted"),
        }
    }

    #[test]
    fn remove_agent_reverts_to_default() {
        let mgr = TokenBudgetManager::new(BudgetConfig {
            max_tokens: Some(100),
            ..Default::default()
        });

        mgr.set_agent_budget("agent-1", BudgetConfig {
            max_tokens: Some(999999),
            ..Default::default()
        });

        mgr.record("agent-1", 500, 500);
        assert!(mgr.check("agent-1").is_allowed()); // 1000 < 999999

        mgr.remove_agent_budget("agent-1");

        // Now uses default: 1000 > 100 → exhausted.
        // Note: observations were cleared with the entry.
        // New agent entry via record will use default config.
        mgr.record("agent-1", 500, 500);
        assert!(!mgr.check("agent-1").is_allowed());
    }
}
