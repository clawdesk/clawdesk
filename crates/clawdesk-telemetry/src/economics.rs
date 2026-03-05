//! Token usage and cost economics tracking.
//!
//! Provides [`WorkflowEconomics`] for aggregating per-workflow LLM costs.
//!
//! Uses the canonical [`clawdesk_types::TokenUsage`] for token counts.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Re-export the canonical token usage type.
pub use clawdesk_types::TokenUsage;

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
        self.total_usage.add(&record.usage);
        self.records.push(record);
    }
}
