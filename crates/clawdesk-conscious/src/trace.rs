//! L4: Retrospective Trace — immutable audit trail with feedback learning.
//!
//! Records every tool execution through the consciousness gateway with full
//! context: tool name, args, risk score, consciousness level, gate path,
//! decision, timing, cost.
//!
//! The trace enables two critical feedback loops:
//! - **L4 → L0**: Human veto rates adjust base risk scores (learning from behavior)
//! - **L4 → L1**: Execution patterns feed sentinel baselines

use chrono::{DateTime, Utc};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

use crate::awareness::{ConsciousnessLevel, RiskScore};

/// A single trace entry — immutable record of one tool execution decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceEntry {
    /// Unique trace ID.
    pub id: String,
    /// Timestamp of the decision.
    pub timestamp: DateTime<Utc>,
    /// Session ID.
    pub session_id: String,
    /// Tool name.
    pub tool: String,
    /// Tool arguments (JSON).
    pub args: serde_json::Value,
    /// Risk score breakdown.
    pub risk_score: RiskScore,
    /// Consciousness level assigned.
    pub level: ConsciousnessLevel,
    /// Decision path through the gateway.
    pub gate_path: GatePath,
    /// Final outcome.
    pub outcome: TraceOutcome,
    /// Execution duration (None if blocked before execution).
    pub duration_ms: Option<u64>,
    /// Cost delta from this operation (None if unknown).
    pub cost_delta: Option<f64>,
    /// Sentinel signals active at decision time.
    pub sentinel_signals: Vec<String>,
}

/// How the decision flowed through the gateway layers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatePath {
    /// L0 classification.
    pub classified_level: ConsciousnessLevel,
    /// Whether sentinel escalated the level.
    pub sentinel_escalated: bool,
    /// Whether deliberation ran and what it decided.
    pub deliberation: Option<String>,
    /// Whether human veto was requested and what they decided.
    pub human_veto: Option<String>,
}

/// Final outcome of the tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TraceOutcome {
    /// Tool executed successfully.
    Executed,
    /// Tool executed with errors.
    ExecutedWithError { error: String },
    /// Blocked by policy (L0).
    PolicyBlocked,
    /// Blocked by sentinel escalation (L1).
    SentinelBlocked { explanation: String },
    /// Blocked by deliberation pattern match (L2).
    PatternBlocked { pattern: String },
    /// Blocked by LLM self-review (L2).
    SelfBlocked { reasoning: String },
    /// Blocked by human veto (L3).
    HumanVetoed,
    /// Approved by human (L3).
    HumanApproved,
    /// Timed out waiting for human (L3 → deny).
    HumanTimeout,
    /// Approved with modified args (L3).
    HumanModified,
}

impl TraceOutcome {
    /// Whether this outcome counts as a human veto (for feedback learning).
    pub fn is_human_veto(&self) -> bool {
        matches!(self, Self::HumanVetoed | Self::HumanTimeout)
    }

    /// Whether this outcome counts as a human approval.
    pub fn is_human_approval(&self) -> bool {
        matches!(self, Self::HumanApproved | Self::HumanModified)
    }

    /// Whether the tool actually executed.
    pub fn was_executed(&self) -> bool {
        matches!(self, Self::Executed | Self::ExecutedWithError { .. }
            | Self::HumanApproved | Self::HumanModified)
    }
}

/// Per-tool veto statistics for feedback learning.
#[derive(Debug, Default)]
struct ToolVetoStats {
    /// Total times this tool reached human veto.
    total_veto_requests: u32,
    /// Times the human denied.
    veto_count: u32,
    /// Times the human approved.
    approve_count: u32,
}

impl ToolVetoStats {
    fn veto_rate(&self) -> f64 {
        if self.total_veto_requests == 0 {
            return 0.0;
        }
        self.veto_count as f64 / self.total_veto_requests as f64
    }
}

/// The Conscious Trace — append-only audit trail with feedback computation.
pub struct ConsciousTrace {
    /// Recent trace entries (ring buffer, configurable capacity).
    entries: VecDeque<TraceEntry>,
    /// Maximum entries to retain in memory.
    max_entries: usize,
    /// Per-tool veto statistics for L4 → L0 feedback.
    veto_stats: FxHashMap<String, ToolVetoStats>,
    /// Minimum samples before adjusting base risk.
    min_feedback_samples: u32,
    /// Veto rate threshold above which base risk increases.
    high_veto_threshold: f64,
    /// Veto rate threshold below which base risk decreases.
    low_veto_threshold: f64,
}

impl ConsciousTrace {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: VecDeque::with_capacity(max_entries.min(10000)),
            max_entries,
            veto_stats: FxHashMap::default(),
            min_feedback_samples: 10,
            high_veto_threshold: 0.3,
            low_veto_threshold: 0.05,
        }
    }

    /// Record a trace entry.
    pub fn record(&mut self, entry: TraceEntry) {
        // Update veto stats
        if entry.outcome.is_human_veto() || entry.outcome.is_human_approval() {
            let stats = self.veto_stats.entry(entry.tool.clone()).or_default();
            stats.total_veto_requests += 1;
            if entry.outcome.is_human_veto() {
                stats.veto_count += 1;
            } else {
                stats.approve_count += 1;
            }
        }

        // Append to ring buffer
        if self.entries.len() >= self.max_entries {
            self.entries.pop_front();
        }
        self.entries.push_back(entry);
    }

    /// Compute L4 → L0 feedback: risk adjustment deltas based on human veto rates.
    ///
    /// Returns a map of tool_name → risk_delta. Positive = increase risk,
    /// negative = decrease risk. Caller applies via `classifier.adjust_base_risk()`.
    pub fn compute_feedback(&self) -> FxHashMap<String, f64> {
        let mut deltas = FxHashMap::default();

        for (tool, stats) in &self.veto_stats {
            if stats.total_veto_requests < self.min_feedback_samples {
                continue; // Not enough data
            }

            let rate = stats.veto_rate();
            if rate > self.high_veto_threshold {
                // Users keep vetoing this tool → it's riskier than classified.
                // Increase base risk proportionally to how far above threshold.
                let delta = (rate - self.high_veto_threshold) * 0.2;
                deltas.insert(tool.clone(), delta);
            } else if rate < self.low_veto_threshold {
                // Users almost never veto this tool → it's safer than classified.
                // Decrease base risk slightly.
                let delta = -(self.low_veto_threshold - rate) * 0.1;
                deltas.insert(tool.clone(), delta);
            }
        }

        deltas
    }

    /// Get recent trace entries (newest first).
    pub fn recent(&self, n: usize) -> Vec<&TraceEntry> {
        self.entries.iter().rev().take(n).collect()
    }

    /// Get total number of trace entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Check if the trace is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get veto rate for a specific tool.
    pub fn tool_veto_rate(&self, tool: &str) -> Option<f64> {
        self.veto_stats.get(tool).map(|s| s.veto_rate())
    }

    /// Get all entries for a session.
    pub fn session_entries(&self, session_id: &str) -> Vec<&TraceEntry> {
        self.entries.iter()
            .filter(|e| e.session_id == session_id)
            .collect()
    }
}

impl Default for ConsciousTrace {
    fn default() -> Self {
        Self::new(5000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(tool: &str, outcome: TraceOutcome) -> TraceEntry {
        TraceEntry {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: Utc::now(),
            session_id: "test-session".to_string(),
            tool: tool.to_string(),
            args: serde_json::json!({}),
            risk_score: RiskScore::compute(0.5, 0.0, 0.0),
            level: ConsciousnessLevel::Deliberative,
            gate_path: GatePath {
                classified_level: ConsciousnessLevel::Deliberative,
                sentinel_escalated: false,
                deliberation: None,
                human_veto: None,
            },
            outcome,
            duration_ms: Some(100),
            cost_delta: None,
            sentinel_signals: vec![],
        }
    }

    #[test]
    fn record_and_retrieve() {
        let mut trace = ConsciousTrace::new(100);
        trace.record(make_entry("shell_exec", TraceOutcome::Executed));
        assert_eq!(trace.len(), 1);
        assert_eq!(trace.recent(1)[0].tool, "shell_exec");
    }

    #[test]
    fn veto_rate_computation() {
        let mut trace = ConsciousTrace::new(100);

        // Record 10 human vetos and 2 approvals for shell_exec
        for _ in 0..8 {
            trace.record(make_entry("shell_exec", TraceOutcome::HumanVetoed));
        }
        for _ in 0..2 {
            trace.record(make_entry("shell_exec", TraceOutcome::HumanApproved));
        }

        let rate = trace.tool_veto_rate("shell_exec").unwrap();
        assert!((rate - 0.8).abs() < f64::EPSILON); // 8/10 = 0.8
    }

    #[test]
    fn feedback_increases_risk_for_high_veto_rate() {
        let mut trace = ConsciousTrace::new(100);

        // 50% veto rate with enough samples
        for _ in 0..10 {
            trace.record(make_entry("shell_exec", TraceOutcome::HumanVetoed));
        }
        for _ in 0..10 {
            trace.record(make_entry("shell_exec", TraceOutcome::HumanApproved));
        }

        let feedback = trace.compute_feedback();
        let delta = feedback.get("shell_exec").unwrap();
        assert!(*delta > 0.0, "high veto rate should increase risk");
    }

    #[test]
    fn feedback_decreases_risk_for_low_veto_rate() {
        let mut trace = ConsciousTrace::new(100);

        // 0% veto rate with enough samples
        for _ in 0..15 {
            trace.record(make_entry("file_write", TraceOutcome::HumanApproved));
        }

        let feedback = trace.compute_feedback();
        let delta = feedback.get("file_write").unwrap();
        assert!(*delta < 0.0, "low veto rate should decrease risk");
    }

    #[test]
    fn ring_buffer_eviction() {
        let mut trace = ConsciousTrace::new(3);
        trace.record(make_entry("a", TraceOutcome::Executed));
        trace.record(make_entry("b", TraceOutcome::Executed));
        trace.record(make_entry("c", TraceOutcome::Executed));
        trace.record(make_entry("d", TraceOutcome::Executed));

        assert_eq!(trace.len(), 3);
        let recent = trace.recent(4);
        assert_eq!(recent.len(), 3);
        assert_eq!(recent[0].tool, "d");
    }
}
