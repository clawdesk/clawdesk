//! Information gaps — things the agent doesn't know but should.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Priority level for an information gap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum GapPriority {
    /// Nice to know — fill during idle time.
    Low,
    /// Relevant to recent work — fill within the hour.
    Medium,
    /// Directly blocks a user task — fill immediately.
    High,
    /// Critical for system health — fill now.
    Critical,
}

/// Where this gap was identified from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GapSource {
    /// A world model entity that's gone stale.
    StaleEntity { entity_id: String },
    /// A user question that couldn't be fully answered.
    UnresolvedQuery { query: String },
    /// A tool call that returned ambiguous results.
    AmbiguousResult { tool_name: String, detail: String },
    /// A dependency check suggested by a failed task.
    FailedDependency { task_description: String },
    /// Channel has unread messages that might need attention.
    UnreadChannel { channel_name: String, count: usize },
    /// User has a temporal pattern — predicted need.
    PredictedNeed { pattern_description: String },
}

/// A concrete gap in the agent's knowledge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InformationGap {
    /// Unique identifier.
    pub id: String,
    /// What question does this gap represent?
    pub question: String,
    /// How important is filling this gap?
    pub priority: GapPriority,
    /// Where was this gap identified?
    pub source: GapSource,
    /// Estimated token cost to resolve this gap.
    pub estimated_cost_tokens: u64,
    /// How old is our last data on this topic?
    pub staleness: Duration,
    /// When this gap was first identified.
    pub identified_at: DateTime<Utc>,
    /// Whether an exploration is currently in progress.
    pub in_progress: bool,
}

impl InformationGap {
    pub fn new(
        id: impl Into<String>,
        question: impl Into<String>,
        priority: GapPriority,
        source: GapSource,
    ) -> Self {
        Self {
            id: id.into(),
            question: question.into(),
            priority,
            source,
            estimated_cost_tokens: 500,
            staleness: Duration::from_secs(0),
            identified_at: Utc::now(),
            in_progress: false,
        }
    }

    pub fn with_cost(mut self, tokens: u64) -> Self {
        self.estimated_cost_tokens = tokens;
        self
    }

    pub fn with_staleness(mut self, staleness: Duration) -> Self {
        self.staleness = staleness;
        self
    }

    /// Score for priority queue ordering.
    /// Higher = more urgent.
    pub fn urgency_score(&self) -> f64 {
        let priority_weight = match self.priority {
            GapPriority::Low => 0.1,
            GapPriority::Medium => 0.4,
            GapPriority::High => 0.8,
            GapPriority::Critical => 1.0,
        };

        // Staleness contributes — older gaps become more urgent
        let staleness_hours = self.staleness.as_secs_f64() / 3600.0;
        let staleness_bonus = (staleness_hours / 24.0).min(0.3); // cap at +0.3

        // Cost penalty — prefer cheap explorations
        let cost_penalty = if self.estimated_cost_tokens > 5000 { 0.1 } else { 0.0 };

        priority_weight + staleness_bonus - cost_penalty
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urgency_ordering() {
        let critical = InformationGap::new("a", "server down?", GapPriority::Critical, GapSource::StaleEntity { entity_id: "srv1".into() });
        let low = InformationGap::new("b", "nice to know", GapPriority::Low, GapSource::PredictedNeed { pattern_description: "monday standup".into() });
        assert!(critical.urgency_score() > low.urgency_score());
    }

    #[test]
    fn staleness_increases_urgency() {
        let fresh = InformationGap::new("a", "q", GapPriority::Medium, GapSource::StaleEntity { entity_id: "x".into() });
        let stale = InformationGap::new("b", "q", GapPriority::Medium, GapSource::StaleEntity { entity_id: "x".into() })
            .with_staleness(Duration::from_secs(48 * 3600));
        assert!(stale.urgency_score() > fresh.urgency_score());
    }
}
