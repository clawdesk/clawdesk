//! Action patterns — successful tool sequences tied to contexts.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single action in a sequence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Action {
    /// Tool name (e.g., "read_file", "execute_command").
    pub tool_name: String,
    /// Condensed argument signature (e.g., file path, command prefix).
    /// Not the full argument — just enough to identify the *kind* of call.
    pub argument_signature: String,
}

impl Action {
    pub fn new(tool_name: impl Into<String>, arg_sig: impl Into<String>) -> Self {
        Self {
            tool_name: tool_name.into(),
            argument_signature: arg_sig.into(),
        }
    }
}

/// The outcome of an action (for learning).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum ActionOutcome {
    Success,
    Failure,
    Partial,
}

/// A learned pattern: a context → action-sequence mapping with statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionPattern {
    /// Unique pattern identifier.
    pub id: String,
    /// Context keywords that trigger this pattern.
    pub context_keywords: Vec<String>,
    /// The action sequence that was performed.
    pub action_sequence: Vec<Action>,
    /// EWMA of the reward signal (0.0–1.0).
    pub reward_ewma: f64,
    /// How many times this pattern has been matched.
    pub frequency: u32,
    /// When this pattern was last matched.
    pub last_used: DateTime<Utc>,
    /// When this pattern was first recorded.
    pub created: DateTime<Utc>,
}

impl ActionPattern {
    /// Create a new pattern from an episode.
    pub fn from_episode(
        id: impl Into<String>,
        context_keywords: Vec<String>,
        actions: Vec<Action>,
        reward: f64,
    ) -> Self {
        let now = Utc::now();
        Self {
            id: id.into(),
            context_keywords,
            action_sequence: actions,
            reward_ewma: reward,
            frequency: 1,
            last_used: now,
            created: now,
        }
    }

    /// Update statistics after a new match.
    pub fn update(&mut self, reward: f64) {
        const ALPHA: f64 = 0.3;
        self.reward_ewma = ALPHA * reward + (1.0 - ALPHA) * self.reward_ewma;
        self.frequency += 1;
        self.last_used = Utc::now();
    }

    /// Confidence score incorporating reward, frequency, and recency.
    pub fn confidence(&self) -> f64 {
        let freq_bonus = (self.frequency as f64).ln().max(0.0) / 10.0;
        let recency_days = (Utc::now() - self.last_used).num_days().max(0) as f64;
        let recency_penalty = (-recency_days / 30.0).exp(); // half-life ~30 days
        (self.reward_ewma + freq_bonus.min(0.2)) * recency_penalty
    }

    /// Whether this pattern's actions overlap with another (for consolidation).
    pub fn action_overlap(&self, other: &ActionPattern) -> f64 {
        if self.action_sequence.is_empty() && other.action_sequence.is_empty() {
            return 1.0;
        }
        if self.action_sequence.is_empty() || other.action_sequence.is_empty() {
            return 0.0;
        }

        // Compare tool name sequences (order-sensitive, using LCS ratio)
        let lcs_len = longest_common_subsequence_len(
            &self.action_sequence.iter().map(|a| a.tool_name.as_str()).collect::<Vec<_>>(),
            &other.action_sequence.iter().map(|a| a.tool_name.as_str()).collect::<Vec<_>>(),
        );
        let max_len = self.action_sequence.len().max(other.action_sequence.len());
        lcs_len as f64 / max_len as f64
    }
}

/// LCS length for short sequences (O(n*m), fine for tool call chains ≤ 25).
fn longest_common_subsequence_len(a: &[&str], b: &[&str]) -> usize {
    let n = a.len();
    let m = b.len();
    let mut dp = vec![vec![0usize; m + 1]; n + 1];
    for i in 1..=n {
        for j in 1..=m {
            dp[i][j] = if a[i - 1] == b[j - 1] {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }
    dp[n][m]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_confidence_increases_with_frequency() {
        let mut p = ActionPattern::from_episode(
            "test",
            vec!["rust".into(), "build".into()],
            vec![Action::new("execute_command", "cargo build")],
            0.8,
        );
        let c1 = p.confidence();
        p.update(0.9);
        p.update(0.85);
        let c2 = p.confidence();
        assert!(c2 > c1, "confidence should increase: {} vs {}", c1, c2);
    }

    #[test]
    fn action_overlap_identical() {
        let p1 = ActionPattern::from_episode(
            "a", vec![],
            vec![Action::new("read_file", ""), Action::new("search_files", "")],
            0.8,
        );
        let p2 = ActionPattern::from_episode(
            "b", vec![],
            vec![Action::new("read_file", ""), Action::new("search_files", "")],
            0.7,
        );
        assert!((p1.action_overlap(&p2) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn lcs_works() {
        assert_eq!(longest_common_subsequence_len(&["a", "b", "c"], &["a", "c"]), 2);
        assert_eq!(longest_common_subsequence_len(&["x", "y"], &["a", "b"]), 0);
    }
}
