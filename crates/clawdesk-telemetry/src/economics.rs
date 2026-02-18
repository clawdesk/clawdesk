//! Token usage and cost economics tracking.
//!
//! Provides [`WorkflowEconomics`] for aggregating per-workflow LLM costs.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Token usage for a single LLM invocation.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}

impl TokenUsage {
    pub fn new(prompt_tokens: u64, completion_tokens: u64) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
        }
    }

    /// Accumulate another usage record.
    pub fn add(&mut self, other: TokenUsage) {
        self.prompt_tokens += other.prompt_tokens;
        self.completion_tokens += other.completion_tokens;
        self.total_tokens += other.total_tokens;
    }
}

/// A single cost record for an LLM call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CostRecord {
    pub id: String,
    pub provider: String,
    pub model: String,
    pub usage: TokenUsage,
    pub cost_usd: f64,
    pub timestamp: u64,
    pub metadata: HashMap<String, String>,
}

/// Aggregate cost/usage tracker for a workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowEconomics {
    pub workflow_id: String,
    pub total_cost_usd: f64,
    pub total_usage: TokenUsage,
    pub records: Vec<CostRecord>,
}

impl WorkflowEconomics {
    pub fn new(workflow_id: impl Into<String>) -> Self {
        Self {
            workflow_id: workflow_id.into(),
            total_cost_usd: 0.0,
            total_usage: TokenUsage::default(),
            records: Vec::new(),
        }
    }

    /// Add a cost record to this workflow.
    pub fn record(&mut self, record: CostRecord) {
        self.total_cost_usd += record.cost_usd;
        self.total_usage.add(record.usage);
        self.records.push(record);
    }
}
