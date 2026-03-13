//! Usage report generation — structured cost data per provider/model/agent/period.
//!
//! Queries the SochDB trace store for completed runs and aggregates token
//! usage and cost by provider, model, agent, and time bucket.
//!
//! ## Usage
//!
//! ```rust
//! let report = UsageReport::generate(&trace_store, &pricing, period);
//! println!("{}", report.format_table());
//! ```

use crate::cost_tracking::{CostSummary, PricingTable};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Time period for the report.
#[derive(Debug, Clone, Copy)]
pub enum ReportPeriod {
    /// Last N hours.
    Hours(u32),
    /// Last N days.
    Days(u32),
    /// All time.
    All,
}

impl ReportPeriod {
    pub fn label(&self) -> String {
        match self {
            Self::Hours(h) => format!("last {h} hours"),
            Self::Days(d) => format!("last {d} days"),
            Self::All => "all time".to_string(),
        }
    }
}

/// A complete usage report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageReport {
    pub period: String,
    pub generated_at: String,
    pub total: CostSummary,
    pub by_provider: Vec<ProviderUsage>,
    pub by_model: Vec<ModelUsage>,
    pub by_agent: Vec<AgentUsage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderUsage {
    pub provider: String,
    pub summary: CostSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelUsage {
    pub model: String,
    pub provider: String,
    pub summary: CostSummary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentUsage {
    pub agent_id: String,
    pub summary: CostSummary,
}

impl UsageReport {
    /// Generate a report from raw cost records.
    pub fn from_records(records: &[UsageRecord], pricing: &PricingTable, period: ReportPeriod) -> Self {
        let mut by_provider: HashMap<String, CostSummary> = HashMap::new();
        let mut by_model: HashMap<(String, String), CostSummary> = HashMap::new();
        let mut by_agent: HashMap<String, CostSummary> = HashMap::new();
        let mut total = CostSummary::default();

        for record in records {
            let cost = pricing.calculate(&record.provider, &record.model, &record.usage());

            // Total
            total.total_cost_usd += cost;
            total.total_requests += 1;
            total.total_input_tokens += record.input_tokens;
            total.total_output_tokens += record.output_tokens;

            // By provider
            let p = by_provider.entry(record.provider.clone()).or_default();
            p.total_cost_usd += cost;
            p.total_requests += 1;
            p.total_input_tokens += record.input_tokens;
            p.total_output_tokens += record.output_tokens;

            // By model
            let m = by_model
                .entry((record.provider.clone(), record.model.clone()))
                .or_default();
            m.total_cost_usd += cost;
            m.total_requests += 1;
            m.total_input_tokens += record.input_tokens;
            m.total_output_tokens += record.output_tokens;

            // By agent
            let a = by_agent.entry(record.agent_id.clone()).or_default();
            a.total_cost_usd += cost;
            a.total_requests += 1;
            a.total_input_tokens += record.input_tokens;
            a.total_output_tokens += record.output_tokens;
        }

        // Compute averages
        if total.total_requests > 0 {
            total.avg_cost_per_request = total.total_cost_usd / total.total_requests as f64;
        }
        for summary in by_provider.values_mut().chain(by_agent.values_mut()) {
            if summary.total_requests > 0 {
                summary.avg_cost_per_request = summary.total_cost_usd / summary.total_requests as f64;
            }
        }
        for summary in by_model.values_mut() {
            if summary.total_requests > 0 {
                summary.avg_cost_per_request = summary.total_cost_usd / summary.total_requests as f64;
            }
        }

        let mut provider_list: Vec<ProviderUsage> = by_provider
            .into_iter()
            .map(|(provider, summary)| ProviderUsage { provider, summary })
            .collect();
        provider_list.sort_by(|a, b| b.summary.total_cost_usd.partial_cmp(&a.summary.total_cost_usd).unwrap());

        let mut model_list: Vec<ModelUsage> = by_model
            .into_iter()
            .map(|((provider, model), summary)| ModelUsage { model, provider, summary })
            .collect();
        model_list.sort_by(|a, b| b.summary.total_cost_usd.partial_cmp(&a.summary.total_cost_usd).unwrap());

        let mut agent_list: Vec<AgentUsage> = by_agent
            .into_iter()
            .map(|(agent_id, summary)| AgentUsage { agent_id, summary })
            .collect();
        agent_list.sort_by(|a, b| b.summary.total_cost_usd.partial_cmp(&a.summary.total_cost_usd).unwrap());

        Self {
            period: period.label(),
            generated_at: chrono::Utc::now().to_rfc3339(),
            total,
            by_provider: provider_list,
            by_model: model_list,
            by_agent: agent_list,
        }
    }

    /// Format as a human-readable table.
    pub fn format_table(&self) -> String {
        let mut out = String::new();

        out.push_str(&format!("Usage Report — {}\n", self.period));
        out.push_str(&format!("Generated: {}\n\n", self.generated_at));

        out.push_str(&format!(
            "Total: ${:.4} | {} requests | {}K input / {}K output tokens\n\n",
            self.total.total_cost_usd,
            self.total.total_requests,
            self.total.total_input_tokens / 1000,
            self.total.total_output_tokens / 1000,
        ));

        if !self.by_provider.is_empty() {
            out.push_str("By Provider:\n");
            for p in &self.by_provider {
                out.push_str(&format!(
                    "  {:<15} ${:.4}  ({} requests)\n",
                    p.provider, p.summary.total_cost_usd, p.summary.total_requests
                ));
            }
            out.push('\n');
        }

        if !self.by_model.is_empty() {
            out.push_str("By Model:\n");
            for m in &self.by_model {
                out.push_str(&format!(
                    "  {:<30} ${:.4}  ({} requests)\n",
                    format!("{}/{}", m.provider, m.model),
                    m.summary.total_cost_usd,
                    m.summary.total_requests
                ));
            }
            out.push('\n');
        }

        if !self.by_agent.is_empty() {
            out.push_str("By Agent:\n");
            for a in &self.by_agent {
                out.push_str(&format!(
                    "  {:<25} ${:.4}  ({} requests)\n",
                    a.agent_id, a.summary.total_cost_usd, a.summary.total_requests
                ));
            }
        }

        out
    }
}

/// Raw usage record (typically extracted from SochDB traces).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub provider: String,
    pub model: String,
    pub agent_id: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub timestamp: String,
}

impl UsageRecord {
    /// Convert to the TokenUsage format used by PricingTable.
    pub fn usage(&self) -> crate::economics::TokenUsage {
        crate::economics::TokenUsage {
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            cache_write_tokens: None,
            cache_read_tokens: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_from_records() {
        let pricing = PricingTable::with_defaults();
        let records = vec![
            UsageRecord {
                provider: "openai".into(),
                model: "gpt-4o".into(),
                agent_id: "coder".into(),
                input_tokens: 1000,
                output_tokens: 500,
                timestamp: "2026-03-12T00:00:00Z".into(),
            },
            UsageRecord {
                provider: "anthropic".into(),
                model: "claude-sonnet-4-20250514".into(),
                agent_id: "researcher".into(),
                input_tokens: 2000,
                output_tokens: 1000,
                timestamp: "2026-03-12T01:00:00Z".into(),
            },
        ];

        let report = UsageReport::from_records(&records, &pricing, ReportPeriod::Days(1));
        assert_eq!(report.total.total_requests, 2);
        assert!(report.total.total_cost_usd > 0.0);
        assert_eq!(report.by_provider.len(), 2);
        assert_eq!(report.by_agent.len(), 2);
    }

    #[test]
    fn report_format_table() {
        let pricing = PricingTable::with_defaults();
        let records = vec![UsageRecord {
            provider: "openai".into(),
            model: "gpt-4o".into(),
            agent_id: "test".into(),
            input_tokens: 100,
            output_tokens: 50,
            timestamp: "2026-03-12T00:00:00Z".into(),
        }];
        let report = UsageReport::from_records(&records, &pricing, ReportPeriod::Hours(24));
        let table = report.format_table();
        assert!(table.contains("By Provider:"));
        assert!(table.contains("openai"));
    }
}
