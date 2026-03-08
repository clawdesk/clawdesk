//! Confidence-weighted voting — simpler alternatives to full PBFT.
//!
//! For cases where full Byzantine consensus is overkill, these functions
//! provide weighted majority voting with confidence scores.

use serde::{Deserialize, Serialize};

/// A vote from an agent with confidence weighting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfidenceVote {
    /// Agent identifier.
    pub agent_id: String,
    /// The agent's choice/answer.
    pub choice: String,
    /// Confidence ∈ [0, 1].
    pub confidence: f64,
    /// Historical accuracy weight (EWMA) ∈ [0, 1].
    pub accuracy_weight: f64,
}

impl ConfidenceVote {
    /// Effective vote weight = confidence × accuracy.
    pub fn effective_weight(&self) -> f64 {
        self.confidence * self.accuracy_weight
    }
}

/// A ballot with weighted votes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeightedBallot {
    /// All votes cast.
    pub votes: Vec<ConfidenceVote>,
    /// Description of what's being decided.
    pub question: String,
}

impl WeightedBallot {
    pub fn new(question: impl Into<String>) -> Self {
        Self {
            votes: Vec::new(),
            question: question.into(),
        }
    }

    pub fn add_vote(&mut self, vote: ConfidenceVote) {
        self.votes.push(vote);
    }
}

/// Result of a voting round.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VotingResult {
    /// The winning choice.
    pub winner: Option<String>,
    /// Weighted score of the winner.
    pub winner_score: f64,
    /// All choices with their weighted scores.
    pub scores: Vec<(String, f64)>,
    /// Number of votes cast.
    pub total_votes: usize,
    /// Whether the result is decisive (winner score > 50% of total weight).
    pub decisive: bool,
}

/// Simple majority vote (unweighted).
pub fn majority_vote(ballot: &WeightedBallot) -> VotingResult {
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();

    for vote in &ballot.votes {
        *counts.entry(&vote.choice).or_insert(0) += 1;
    }

    let scores: Vec<(String, f64)> = counts
        .iter()
        .map(|(choice, count)| (choice.to_string(), *count as f64))
        .collect();

    let winner = scores
        .iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .cloned();

    let total = ballot.votes.len() as f64;
    let decisive = winner
        .as_ref()
        .map(|(_, s)| *s > total / 2.0)
        .unwrap_or(false);

    VotingResult {
        winner_score: winner.as_ref().map(|(_, s)| *s).unwrap_or(0.0),
        winner: winner.map(|(c, _)| c),
        scores,
        total_votes: ballot.votes.len(),
        decisive,
    }
}

/// Confidence-weighted vote.
///
/// Each vote's weight = confidence × accuracy_weight.
/// The choice with the highest total weight wins.
pub fn confidence_weighted_vote(ballot: &WeightedBallot) -> VotingResult {
    let mut weighted_scores: std::collections::HashMap<&str, f64> =
        std::collections::HashMap::new();

    for vote in &ballot.votes {
        *weighted_scores.entry(&vote.choice).or_insert(0.0) += vote.effective_weight();
    }

    let scores: Vec<(String, f64)> = weighted_scores
        .iter()
        .map(|(choice, score)| (choice.to_string(), *score))
        .collect();

    let total_weight: f64 = scores.iter().map(|(_, s)| s).sum();
    let winner = scores
        .iter()
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .cloned();

    let decisive = winner
        .as_ref()
        .map(|(_, s)| *s > total_weight / 2.0)
        .unwrap_or(false);

    VotingResult {
        winner_score: winner.as_ref().map(|(_, s)| *s).unwrap_or(0.0),
        winner: winner.map(|(c, _)| c),
        scores,
        total_votes: ballot.votes.len(),
        decisive,
    }
}

/// Supermajority vote: requires > 2/3 of weighted votes.
pub fn supermajority_vote(ballot: &WeightedBallot) -> VotingResult {
    let mut result = confidence_weighted_vote(ballot);
    let total_weight: f64 = result.scores.iter().map(|(_, s)| s).sum();
    result.decisive = result.winner_score > total_weight * 2.0 / 3.0;
    result
}

// ───────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn vote(agent: &str, choice: &str, conf: f64, accuracy: f64) -> ConfidenceVote {
        ConfidenceVote {
            agent_id: agent.into(),
            choice: choice.into(),
            confidence: conf,
            accuracy_weight: accuracy,
        }
    }

    #[test]
    fn test_majority_vote() {
        let mut ballot = WeightedBallot::new("best language?");
        ballot.add_vote(vote("a1", "rust", 0.9, 0.8));
        ballot.add_vote(vote("a2", "rust", 0.7, 0.6));
        ballot.add_vote(vote("a3", "python", 0.8, 0.9));

        let result = majority_vote(&ballot);
        assert_eq!(result.winner.as_deref(), Some("rust"));
        assert!(result.decisive);
    }

    #[test]
    fn test_confidence_weighted_vote() {
        let mut ballot = WeightedBallot::new("best approach?");
        // Low confidence for "A".
        ballot.add_vote(vote("a1", "A", 0.3, 0.5));
        ballot.add_vote(vote("a2", "A", 0.2, 0.5));
        // High confidence for "B" from accurate agent.
        ballot.add_vote(vote("a3", "B", 0.9, 0.9));

        let result = confidence_weighted_vote(&ballot);
        assert_eq!(result.winner.as_deref(), Some("B"));
    }

    #[test]
    fn test_supermajority_not_reached() {
        let mut ballot = WeightedBallot::new("consensus?");
        ballot.add_vote(vote("a1", "yes", 0.6, 0.7));
        ballot.add_vote(vote("a2", "no", 0.5, 0.6));
        ballot.add_vote(vote("a3", "maybe", 0.4, 0.5));

        let result = supermajority_vote(&ballot);
        assert!(!result.decisive, "no choice has >2/3 of weight");
    }

    #[test]
    fn test_effective_weight() {
        let v = vote("a1", "x", 0.8, 0.5);
        assert!((v.effective_weight() - 0.4).abs() < 1e-10);
    }
}
