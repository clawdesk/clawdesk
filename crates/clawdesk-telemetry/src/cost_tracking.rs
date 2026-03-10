//! Comprehensive cost tracking and budgeting for LLM operations.
//!
//! Extends the base economics module with:
//! - Per-model cost tables with input/output/cached pricing
//! - Budget enforcement with configurable thresholds
//! - Cost aggregation by agent, model, and time window
//! - Cost alerts when approaching budget limits

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{info, warn};

use super::economics::{CostRecord, TokenUsage, WorkflowEconomics};

// ─────────────────────────────────────────────────────────────────────────────
// Model pricing
// ─────────────────────────────────────────────────────────────────────────────

/// Pricing for a specific model (USD per 1M tokens).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPricing {
    pub provider: String,
    pub model: String,
    /// USD per 1M input tokens.
    pub input_per_million: f64,
    /// USD per 1M output tokens.
    pub output_per_million: f64,
    /// USD per 1M cached input tokens (optional, for prompt caching).
    #[serde(default)]
    pub cached_per_million: Option<f64>,
}

impl ModelPricing {
    /// Calculate cost for a given token usage.
    pub fn calculate_cost(&self, usage: &TokenUsage) -> f64 {
        let input_cost = (usage.input_tokens as f64) * self.input_per_million / 1_000_000.0;
        let output_cost = (usage.output_tokens as f64) * self.output_per_million / 1_000_000.0;
        let cached_cost = if let (Some(cached_rate), Some(cached_tokens)) =
            (self.cached_per_million, usage.cache_read_tokens)
        {
            (cached_tokens as f64) * cached_rate / 1_000_000.0
        } else {
            0.0
        };
        input_cost + output_cost + cached_cost
    }
}

/// Registry of model pricing data.
#[derive(Debug, Clone, Default)]
pub struct PricingTable {
    /// Map of "{provider}/{model}" → pricing.
    models: HashMap<String, ModelPricing>,
}

impl PricingTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load pricing from a list of model pricing entries.
    pub fn load(&mut self, entries: Vec<ModelPricing>) {
        for entry in entries {
            let key = format!("{}/{}", entry.provider, entry.model);
            self.models.insert(key, entry);
        }
    }

    /// Look up pricing for a provider/model pair.
    pub fn get(&self, provider: &str, model: &str) -> Option<&ModelPricing> {
        let key = format!("{provider}/{model}");
        self.models.get(&key)
    }

    /// Calculate cost for a given provider/model/usage triple.
    pub fn calculate(&self, provider: &str, model: &str, usage: &TokenUsage) -> f64 {
        self.get(provider, model)
            .map(|p| p.calculate_cost(usage))
            .unwrap_or(0.0)
    }

    /// Load default pricing for common models.
    pub fn with_defaults() -> Self {
        let mut table = Self::new();
        table.load(vec![
            ModelPricing {
                provider: "openai".into(),
                model: "gpt-4o".into(),
                input_per_million: 2.50,
                output_per_million: 10.00,
                cached_per_million: Some(1.25),
            },
            ModelPricing {
                provider: "openai".into(),
                model: "gpt-4o-mini".into(),
                input_per_million: 0.15,
                output_per_million: 0.60,
                cached_per_million: Some(0.075),
            },
            ModelPricing {
                provider: "anthropic".into(),
                model: "claude-sonnet-4-20250514".into(),
                input_per_million: 3.00,
                output_per_million: 15.00,
                cached_per_million: Some(0.30),
            },
            ModelPricing {
                provider: "anthropic".into(),
                model: "claude-haiku-3.5".into(),
                input_per_million: 0.80,
                output_per_million: 4.00,
                cached_per_million: Some(0.08),
            },
            ModelPricing {
                provider: "google".into(),
                model: "gemini-2.0-flash".into(),
                input_per_million: 0.10,
                output_per_million: 0.40,
                cached_per_million: None,
            },
        ]);
        table
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Budget enforcement
// ─────────────────────────────────────────────────────────────────────────────

/// Budget configuration for cost control.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetConfig {
    /// Maximum spend per rolling window (USD).
    pub max_spend_usd: f64,
    /// Rolling window duration.
    pub window: Duration,
    /// Warning threshold (fraction of max, e.g. 0.8 for 80%).
    pub warn_threshold: f64,
    /// Hard limit threshold — reject requests above this.
    pub hard_limit_threshold: f64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            max_spend_usd: 10.0,
            window: Duration::from_secs(86400), // 24 hours
            warn_threshold: 0.8,
            hard_limit_threshold: 1.0,
        }
    }
}

/// Result of a budget check.
#[derive(Debug, Clone, PartialEq)]
pub enum BudgetStatus {
    /// Under budget, no issues.
    Ok { spent_usd: f64, remaining_usd: f64 },
    /// Approaching budget limit.
    Warning { spent_usd: f64, remaining_usd: f64 },
    /// At or over budget — requests should be throttled or rejected.
    Exceeded { spent_usd: f64, over_by_usd: f64 },
}

impl BudgetStatus {
    pub fn is_exceeded(&self) -> bool {
        matches!(self, Self::Exceeded { .. })
    }
}

/// Tracks spending within a rolling time window.
pub struct BudgetTracker {
    config: BudgetConfig,
    /// Timestamped cost entries within the window.
    entries: Vec<(Instant, f64)>,
}

impl BudgetTracker {
    pub fn new(config: BudgetConfig) -> Self {
        Self {
            config,
            entries: Vec::new(),
        }
    }

    /// Record a cost event.
    pub fn record(&mut self, cost_usd: f64) {
        self.entries.push((Instant::now(), cost_usd));
    }

    /// Evict entries outside the rolling window.
    fn evict_expired(&mut self) {
        let cutoff = Instant::now() - self.config.window;
        self.entries.retain(|(t, _)| *t >= cutoff);
    }

    /// Get current spending within the window.
    pub fn current_spend(&mut self) -> f64 {
        self.evict_expired();
        self.entries.iter().map(|(_, c)| c).sum()
    }

    /// Check budget status.
    pub fn check(&mut self) -> BudgetStatus {
        let spent = self.current_spend();
        let max = self.config.max_spend_usd;
        let hard = max * self.config.hard_limit_threshold;
        let warn = max * self.config.warn_threshold;

        if spent >= hard {
            warn!(spent_usd = spent, limit = hard, "budget exceeded");
            BudgetStatus::Exceeded {
                spent_usd: spent,
                over_by_usd: spent - hard,
            }
        } else if spent >= warn {
            info!(spent_usd = spent, warn_at = warn, "approaching budget limit");
            BudgetStatus::Warning {
                spent_usd: spent,
                remaining_usd: hard - spent,
            }
        } else {
            BudgetStatus::Ok {
                spent_usd: spent,
                remaining_usd: hard - spent,
            }
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Cost aggregation
// ─────────────────────────────────────────────────────────────────────────────

/// Aggregated cost summary for a dimension (agent, model, provider).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CostSummary {
    pub total_cost_usd: f64,
    pub total_requests: usize,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub avg_cost_per_request: f64,
}

/// Aggregates cost records by multiple dimensions.
#[derive(Debug, Default)]
pub struct CostAggregator {
    by_agent: HashMap<String, CostSummary>,
    by_model: HashMap<String, CostSummary>,
    by_provider: HashMap<String, CostSummary>,
    total: CostSummary,
}

impl CostAggregator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest a cost record.
    pub fn ingest(&mut self, record: &CostRecord) {
        let agent_id = record
            .metadata
            .get("agent_id")
            .cloned()
            .unwrap_or_else(|| "unknown".into());

        self.update_summary(&mut self.by_agent.clone(), &agent_id, record);
        self.update_summary(&mut self.by_model.clone(), &record.model, record);
        self.update_summary(&mut self.by_provider.clone(), &record.provider, record);

        // Update maps
        Self::update_entry(&mut self.by_agent, &agent_id, record);
        Self::update_entry(&mut self.by_model, &record.model, record);
        Self::update_entry(&mut self.by_provider, &record.provider, record);

        // Update total
        self.total.total_cost_usd += record.cost_usd;
        self.total.total_requests += 1;
        self.total.total_input_tokens += record.usage.input_tokens;
        self.total.total_output_tokens += record.usage.output_tokens;
        if self.total.total_requests > 0 {
            self.total.avg_cost_per_request =
                self.total.total_cost_usd / self.total.total_requests as f64;
        }
    }

    fn update_entry(map: &mut HashMap<String, CostSummary>, key: &str, record: &CostRecord) {
        let summary = map.entry(key.to_string()).or_default();
        summary.total_cost_usd += record.cost_usd;
        summary.total_requests += 1;
        summary.total_input_tokens += record.usage.input_tokens;
        summary.total_output_tokens += record.usage.output_tokens;
        if summary.total_requests > 0 {
            summary.avg_cost_per_request =
                summary.total_cost_usd / summary.total_requests as f64;
        }
    }

    fn update_summary(
        &self,
        _map: &mut HashMap<String, CostSummary>,
        _key: &str,
        _record: &CostRecord,
    ) {
        // Handled by update_entry
    }

    /// Get aggregation by agent.
    pub fn by_agent(&self) -> &HashMap<String, CostSummary> {
        &self.by_agent
    }

    /// Get aggregation by model.
    pub fn by_model(&self) -> &HashMap<String, CostSummary> {
        &self.by_model
    }

    /// Get aggregation by provider.
    pub fn by_provider(&self) -> &HashMap<String, CostSummary> {
        &self.by_provider
    }

    /// Get overall total.
    pub fn total(&self) -> &CostSummary {
        &self.total
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pricing_calculation() {
        let pricing = ModelPricing {
            provider: "openai".into(),
            model: "gpt-4o".into(),
            input_per_million: 2.50,
            output_per_million: 10.00,
            cached_per_million: None,
        };
        let usage = TokenUsage {
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: None,
            cache_write_tokens: None,
        };
        let cost = pricing.calculate_cost(&usage);
        // 1000 * 2.50/1M + 500 * 10.00/1M = 0.0025 + 0.005 = 0.0075
        assert!((cost - 0.0075).abs() < 1e-10);
    }

    #[test]
    fn budget_under_limit() {
        let config = BudgetConfig {
            max_spend_usd: 10.0,
            window: Duration::from_secs(3600),
            warn_threshold: 0.8,
            hard_limit_threshold: 1.0,
        };
        let mut tracker = BudgetTracker::new(config);
        tracker.record(1.0);
        let status = tracker.check();
        assert!(matches!(status, BudgetStatus::Ok { .. }));
    }

    #[test]
    fn budget_warning() {
        let config = BudgetConfig {
            max_spend_usd: 10.0,
            window: Duration::from_secs(3600),
            warn_threshold: 0.8,
            hard_limit_threshold: 1.0,
        };
        let mut tracker = BudgetTracker::new(config);
        tracker.record(8.5);
        let status = tracker.check();
        assert!(matches!(status, BudgetStatus::Warning { .. }));
    }

    #[test]
    fn budget_exceeded() {
        let config = BudgetConfig {
            max_spend_usd: 10.0,
            window: Duration::from_secs(3600),
            warn_threshold: 0.8,
            hard_limit_threshold: 1.0,
        };
        let mut tracker = BudgetTracker::new(config);
        tracker.record(11.0);
        assert!(tracker.check().is_exceeded());
    }

    #[test]
    fn default_pricing_table() {
        let table = PricingTable::with_defaults();
        assert!(table.get("openai", "gpt-4o").is_some());
        assert!(table.get("anthropic", "claude-sonnet-4-20250514").is_some());
    }

    #[test]
    fn cost_aggregator() {
        let mut agg = CostAggregator::new();
        let mut meta = HashMap::new();
        meta.insert("agent_id".to_string(), "coder".to_string());

        agg.ingest(&CostRecord {
            id: "1".into(),
            provider: "openai".into(),
            model: "gpt-4o".into(),
            usage: TokenUsage {
                input_tokens: 1000,
                output_tokens: 500,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
            cost_usd: 0.0075,
            timestamp: 0,
            metadata: meta,
        });

        assert_eq!(agg.total().total_requests, 1);
        assert!((agg.total().total_cost_usd - 0.0075).abs() < 1e-10);
        assert!(agg.by_agent().contains_key("coder"));
        assert!(agg.by_model().contains_key("gpt-4o"));
    }
}
