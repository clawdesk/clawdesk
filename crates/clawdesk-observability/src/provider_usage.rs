//! # Provider Usage Aggregation
//!
//! Fetches real usage data from provider dashboards (Anthropic, OpenAI,
//! Gemini, etc.) and unifies it into a single cost/token view.
//!
//! ClawDesk already tracks local usage via `state.rs::record_usage()`,
//! but doesn't reconcile against the provider's own billing data.
//! This module adds that reconciliation.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Unified usage record from a provider's billing API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderUsageRecord {
    pub provider: String,
    pub period_start: String,
    pub period_end: String,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub total_cost_usd: f64,
    pub model_breakdown: Vec<ModelUsage>,
    pub fetched_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelUsage {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    pub requests: u64,
}

/// Aggregated view across all providers.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AggregatedUsage {
    pub total_cost_usd: f64,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub by_provider: HashMap<String, ProviderUsageRecord>,
    /// Local tracking vs provider-reported discrepancy.
    pub local_cost_usd: f64,
    pub discrepancy_usd: f64,
}

impl AggregatedUsage {
    pub fn add_provider(&mut self, record: ProviderUsageRecord) {
        self.total_cost_usd += record.total_cost_usd;
        self.total_input_tokens += record.total_input_tokens;
        self.total_output_tokens += record.total_output_tokens;
        self.by_provider.insert(record.provider.clone(), record);
    }

    pub fn set_local_cost(&mut self, local: f64) {
        self.local_cost_usd = local;
        self.discrepancy_usd = (self.total_cost_usd - local).abs();
    }
}

/// Provider-specific usage fetcher trait.
///
/// Each provider implements this to pull from their billing API.
/// The fetch is async because it makes HTTP calls.
#[async_trait::async_trait]
pub trait UsageFetcher: Send + Sync {
    fn provider_name(&self) -> &str;
    async fn fetch_usage(
        &self,
        api_key: &str,
        period_days: u32,
    ) -> Result<ProviderUsageRecord, String>;
}
