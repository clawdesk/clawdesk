//! # Prompt Trace — Reasoning transparency for prompt assembly
//!
//! Records *why* each section was included or excluded from the system
//! prompt, enabling debugging, A/B testing, and prompt optimization.
//!
//! ## Gap Closed
//!
//! The audit found: "No prompt reasoning traces (why skill A included vs B)".
//! This module adds full decision logging to the prompt assembly pipeline.

use serde::{Deserialize, Serialize};
use std::time::Instant;

/// A single decision made during prompt assembly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptDecision {
    /// Which section was considered (e.g. "skill:tdd-workflow", "memory:recall").
    pub section_id: String,
    /// Whether the section was included in the final prompt.
    pub included: bool,
    /// Cost in tokens.
    pub token_cost: usize,
    /// Priority at which it was evaluated.
    pub priority: u32,
    /// Why it was included or excluded.
    pub reason: String,
    /// Budget remaining after this decision.
    pub budget_remaining: usize,
}

/// Full trace of the prompt assembly process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptTrace {
    /// Every decision made, in order.
    pub decisions: Vec<PromptDecision>,
    /// Total tokens used in the final prompt.
    pub total_tokens_used: usize,
    /// Total budget available.
    pub total_budget: usize,
    /// How long the assembly took.
    pub assembly_duration_ms: u64,
    /// The variant ID if running an A/B test.
    pub ab_variant: Option<String>,
}

impl PromptTrace {
    pub fn new(total_budget: usize) -> Self {
        Self {
            decisions: Vec::new(),
            total_tokens_used: 0,
            total_budget,
            assembly_duration_ms: 0,
            ab_variant: None,
        }
    }

    pub fn record(&mut self, decision: PromptDecision) {
        if decision.included {
            self.total_tokens_used += decision.token_cost;
        }
        self.decisions.push(decision);
    }

    /// Sections that were excluded due to budget limits.
    pub fn excluded_by_budget(&self) -> Vec<&PromptDecision> {
        self.decisions.iter()
            .filter(|d| !d.included && d.reason.contains("budget"))
            .collect()
    }

    /// Utilization percentage — how much of the budget was used.
    pub fn utilization(&self) -> f64 {
        if self.total_budget == 0 { return 0.0; }
        self.total_tokens_used as f64 / self.total_budget as f64
    }
}

// ─── A/B Testing Framework ──────────────────────────────────────────────────

/// Configuration for a prompt A/B test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptExperiment {
    /// Unique experiment identifier.
    pub id: String,
    /// Human-readable description.
    pub description: String,
    /// Variants to test.
    pub variants: Vec<PromptVariant>,
    /// Traffic allocation (must sum to 1.0).
    pub traffic_splits: Vec<f64>,
    /// Metric to optimize.
    pub primary_metric: String,
    /// Whether the experiment is active.
    pub active: bool,
}

/// A single variant in a prompt experiment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptVariant {
    pub id: String,
    pub label: String,
    /// Overrides to apply when this variant is selected.
    pub overrides: VariantOverrides,
}

/// What a variant changes in the prompt assembly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariantOverrides {
    /// Override system prompt prefix.
    pub system_prefix: Option<String>,
    /// Override skill selection order.
    pub skill_priority_boost: Vec<String>,
    /// Override verbosity.
    pub verbosity: Option<String>,
    /// Override temperature.
    pub temperature: Option<f64>,
}

/// Selects a variant for a given session using deterministic hashing.
pub fn select_variant<'a>(experiment: &'a PromptExperiment, session_id: &str) -> Option<&'a PromptVariant> {
    if !experiment.active || experiment.variants.is_empty() {
        return None;
    }
    // Deterministic: same session always gets same variant.
    let hash = simple_hash(session_id, &experiment.id);
    let bucket = (hash % 1000) as f64 / 1000.0;

    let mut cumulative = 0.0;
    for (i, split) in experiment.traffic_splits.iter().enumerate() {
        cumulative += split;
        if bucket < cumulative {
            return experiment.variants.get(i);
        }
    }
    experiment.variants.last()
}

fn simple_hash(a: &str, b: &str) -> u64 {
    let mut h: u64 = 5381;
    for byte in a.bytes().chain(b.bytes()) {
        h = h.wrapping_mul(33).wrapping_add(byte as u64);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prompt_trace_utilization() {
        let mut trace = PromptTrace::new(10_000);
        trace.record(PromptDecision {
            section_id: "identity".into(),
            included: true,
            token_cost: 500,
            priority: 100,
            reason: "required".into(),
            budget_remaining: 9500,
        });
        trace.record(PromptDecision {
            section_id: "skill:tdd".into(),
            included: true,
            token_cost: 1500,
            priority: 75,
            reason: "trigger match".into(),
            budget_remaining: 8000,
        });
        trace.record(PromptDecision {
            section_id: "skill:deploy".into(),
            included: false,
            token_cost: 2000,
            priority: 50,
            reason: "excluded: budget insufficient".into(),
            budget_remaining: 8000,
        });
        assert_eq!(trace.total_tokens_used, 2000);
        assert!((trace.utilization() - 0.2).abs() < 0.01);
        assert_eq!(trace.excluded_by_budget().len(), 1);
    }

    #[test]
    fn test_ab_variant_selection_deterministic() {
        let exp = PromptExperiment {
            id: "exp1".into(),
            description: "test".into(),
            variants: vec![
                PromptVariant { id: "a".into(), label: "Control".into(), overrides: VariantOverrides { system_prefix: None, skill_priority_boost: vec![], verbosity: None, temperature: None } },
                PromptVariant { id: "b".into(), label: "Treatment".into(), overrides: VariantOverrides { system_prefix: Some("Be extra concise.".into()), skill_priority_boost: vec![], verbosity: Some("terse".into()), temperature: None } },
            ],
            traffic_splits: vec![0.5, 0.5],
            primary_metric: "reward".into(),
            active: true,
        };

        // Same session → same variant, every time.
        let v1 = select_variant(&exp, "session-123").unwrap();
        let v2 = select_variant(&exp, "session-123").unwrap();
        assert_eq!(v1.id, v2.id);

        // Different sessions can get different variants.
        // (Not guaranteed, but probabilistically likely.)
    }
}
