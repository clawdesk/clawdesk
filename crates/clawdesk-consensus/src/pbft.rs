//! PBFT (Practical Byzantine Fault Tolerance) consensus protocol.
//!
//! Simplified for multi-agent LLM decisions:
//!
//! 1. **Pre-prepare** (leader proposes): Leader agent proposes a decision
//!    with its analysis.
//!
//! 2. **Prepare** (agents vote): Each agent evaluates the proposal and
//!    sends a PREPARE message with its vote (agree/disagree + confidence).
//!
//! 3. **Commit** (threshold reached): If ≥ 2f+1 matching PREPARE votes
//!    are received, the decision is committed.
//!
//! ## Confidence weighting
//!
//! Each agent's vote is weighted by its historical accuracy (EWMA):
//!
//! ```text
//! effective_vote(a_i) = vote(a_i) × accuracy(a_i)
//! ```
//!
//! Low-accuracy agents have reduced influence, naturally marginalizing
//! Byzantine (hallucinating) agents over time.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use thiserror::Error;
use tracing::{debug, info, warn};

// ───────────────────────────────────────────────────────────────
// Protocol types
// ───────────────────────────────────────────────────────────────

/// PBFT protocol phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PbftPhase {
    PrePrepare,
    Prepare,
    Commit,
    Decided,
    Failed,
}

/// A message in the PBFT protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PbftMessage {
    /// Unique message ID.
    pub id: String,
    /// Consensus round.
    pub round: u64,
    /// Phase this message belongs to.
    pub phase: PbftPhase,
    /// Sender agent ID.
    pub sender: String,
    /// The proposed/voted decision (serialized).
    pub decision: String,
    /// Confidence in the vote ∈ [0, 1].
    pub confidence: f64,
    /// Whether the sender agrees with the proposal.
    pub agrees: bool,
    /// Timestamp.
    pub timestamp: DateTime<Utc>,
}

/// Configuration for the PBFT consensus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PbftConfig {
    /// Total number of agents (n).
    pub num_agents: usize,
    /// Maximum Byzantine faults tolerable (f).
    /// Must satisfy: num_agents ≥ 3 × max_faults + 1.
    pub max_faults: usize,
    /// EWMA smoothing factor for accuracy tracking.
    pub accuracy_ewma_alpha: f64,
    /// Minimum confidence for a vote to count.
    pub min_confidence: f64,
    /// Timeout per phase in seconds.
    pub phase_timeout_secs: u64,
}

impl PbftConfig {
    /// Create from desired fault tolerance.
    pub fn from_fault_tolerance(f: usize) -> Self {
        let n = 3 * f + 1;
        Self {
            num_agents: n,
            max_faults: f,
            accuracy_ewma_alpha: 0.2,
            min_confidence: 0.3,
            phase_timeout_secs: 30,
        }
    }

    /// Required votes for quorum: 2f + 1.
    pub fn quorum(&self) -> usize {
        2 * self.max_faults + 1
    }

    /// Validate that the configuration is consistent.
    pub fn validate(&self) -> Result<(), ConsensusError> {
        if self.num_agents < 3 * self.max_faults + 1 {
            return Err(ConsensusError::InsufficientAgents {
                have: self.num_agents,
                need: 3 * self.max_faults + 1,
            });
        }
        Ok(())
    }
}

impl Default for PbftConfig {
    fn default() -> Self {
        Self::from_fault_tolerance(1) // n=4, tolerates 1 Byzantine agent.
    }
}

/// Error types for consensus.
#[derive(Debug, Error)]
pub enum ConsensusError {
    #[error("insufficient agents: have {have}, need ≥ {need}")]
    InsufficientAgents { have: usize, need: usize },
    #[error("consensus timeout in phase {phase:?}")]
    Timeout { phase: PbftPhase },
    #[error("no quorum reached: {votes} votes, need {quorum}")]
    NoQuorum { votes: usize, quorum: usize },
    #[error("duplicate vote from agent {agent_id}")]
    DuplicateVote { agent_id: String },
    #[error("invalid phase transition: {from:?} → {to:?}")]
    InvalidTransition { from: PbftPhase, to: PbftPhase },
}

/// Result of a consensus round.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsensusResult {
    /// The decided value (if consensus was reached).
    pub decision: Option<String>,
    /// Whether consensus was achieved.
    pub reached: bool,
    /// Number of agreeing votes.
    pub agree_votes: usize,
    /// Number of disagreeing votes.
    pub disagree_votes: usize,
    /// Weighted agreement score ∈ [0, 1].
    pub weighted_agreement: f64,
    /// Per-agent details.
    pub agent_votes: Vec<AgentVoteDetail>,
    /// Round number.
    pub round: u64,
}

/// Per-agent vote detail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentVoteDetail {
    pub agent_id: String,
    pub agrees: bool,
    pub confidence: f64,
    pub accuracy_weight: f64,
    pub effective_vote: f64,
}

// ───────────────────────────────────────────────────────────────
// PBFT Consensus Engine
// ───────────────────────────────────────────────────────────────

/// Per-agent accuracy tracker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAccuracy {
    /// EWMA of accuracy (fraction of times this agent agreed with consensus).
    pub accuracy: f64,
    /// Total rounds participated.
    pub rounds: u64,
    /// Alpha for EWMA.
    alpha: f64,
}

impl AgentAccuracy {
    pub fn new(alpha: f64) -> Self {
        Self {
            accuracy: 0.5, // Neutral prior.
            rounds: 0,
            alpha,
        }
    }

    pub fn update(&mut self, agreed_with_consensus: bool) {
        let sample = if agreed_with_consensus { 1.0 } else { 0.0 };
        if self.rounds == 0 {
            self.accuracy = sample;
        } else {
            self.accuracy = self.alpha * sample + (1.0 - self.alpha) * self.accuracy;
        }
        self.rounds += 1;
    }
}

/// PBFT consensus state for a single round.
#[derive(Debug, Clone)]
pub struct PbftState {
    /// Current phase.
    pub phase: PbftPhase,
    /// Current round.
    pub round: u64,
    /// The proposed decision.
    pub proposal: Option<String>,
    /// Leader agent ID.
    pub leader: Option<String>,
    /// Collected PREPARE messages.
    pub prepares: HashMap<String, PbftMessage>,
    /// Collected COMMIT messages.
    pub commits: HashMap<String, PbftMessage>,
}

/// PBFT consensus engine.
pub struct PbftConsensus {
    config: PbftConfig,
    /// Per-agent accuracy tracking.
    agent_accuracy: HashMap<String, AgentAccuracy>,
    /// Current consensus state.
    state: PbftState,
    /// Historical results.
    history: Vec<ConsensusResult>,
}

impl PbftConsensus {
    pub fn new(config: PbftConfig) -> Result<Self, ConsensusError> {
        config.validate()?;
        Ok(Self {
            config: config.clone(),
            agent_accuracy: HashMap::new(),
            state: PbftState {
                phase: PbftPhase::PrePrepare,
                round: 0,
                proposal: None,
                leader: None,
                prepares: HashMap::new(),
                commits: HashMap::new(),
            },
            history: Vec::new(),
        })
    }

    /// Start a new consensus round with a proposal.
    pub fn propose(&mut self, leader: &str, decision: &str) -> Result<u64, ConsensusError> {
        self.state.round += 1;
        self.state.phase = PbftPhase::Prepare;
        self.state.proposal = Some(decision.to_string());
        self.state.leader = Some(leader.to_string());
        self.state.prepares.clear();
        self.state.commits.clear();

        info!(
            round = self.state.round,
            leader,
            "PBFT: new consensus round proposed"
        );

        Ok(self.state.round)
    }

    /// Submit a PREPARE vote for the current round.
    pub fn prepare(
        &mut self,
        agent_id: &str,
        agrees: bool,
        confidence: f64,
    ) -> Result<(), ConsensusError> {
        if self.state.phase != PbftPhase::Prepare {
            return Err(ConsensusError::InvalidTransition {
                from: self.state.phase,
                to: PbftPhase::Prepare,
            });
        }

        if self.state.prepares.contains_key(agent_id) {
            return Err(ConsensusError::DuplicateVote {
                agent_id: agent_id.to_string(),
            });
        }

        let msg = PbftMessage {
            id: uuid::Uuid::new_v4().to_string(),
            round: self.state.round,
            phase: PbftPhase::Prepare,
            sender: agent_id.to_string(),
            decision: self.state.proposal.clone().unwrap_or_default(),
            confidence: confidence.clamp(0.0, 1.0),
            agrees,
            timestamp: Utc::now(),
        };

        debug!(
            agent = agent_id,
            agrees,
            confidence,
            round = self.state.round,
            "PBFT: prepare vote received"
        );

        self.state.prepares.insert(agent_id.to_string(), msg);

        Ok(())
    }

    /// Evaluate the current round and produce a consensus result.
    pub fn evaluate(&mut self) -> ConsensusResult {
        let total_votes = self.state.prepares.len();
        let quorum = self.config.quorum();

        let mut agent_votes = Vec::new();
        let mut weighted_agree = 0.0;
        let mut weighted_total = 0.0;
        let mut agree_count = 0usize;
        let mut disagree_count = 0usize;

        for (agent_id, msg) in &self.state.prepares {
            if msg.confidence < self.config.min_confidence {
                continue; // Skip low-confidence votes.
            }

            let accuracy_weight = self
                .agent_accuracy
                .get(agent_id)
                .map(|a| a.accuracy)
                .unwrap_or(0.5);

            let effective = msg.confidence * accuracy_weight;

            if msg.agrees {
                agree_count += 1;
                weighted_agree += effective;
            } else {
                disagree_count += 1;
            }
            weighted_total += effective;

            agent_votes.push(AgentVoteDetail {
                agent_id: agent_id.clone(),
                agrees: msg.agrees,
                confidence: msg.confidence,
                accuracy_weight,
                effective_vote: effective,
            });
        }

        let weighted_agreement = if weighted_total > 0.0 {
            weighted_agree / weighted_total
        } else {
            0.0
        };

        let reached = agree_count >= quorum && weighted_agreement > 0.5;

        let result = ConsensusResult {
            decision: if reached {
                self.state.proposal.clone()
            } else {
                None
            },
            reached,
            agree_votes: agree_count,
            disagree_votes: disagree_count,
            weighted_agreement,
            agent_votes: agent_votes.clone(),
            round: self.state.round,
        };

        // Update accuracy tracking.
        if reached {
            let decision_agrees = true; // Consensus reached = agreement was correct.
            for detail in &agent_votes {
                let acc = self
                    .agent_accuracy
                    .entry(detail.agent_id.clone())
                    .or_insert_with(|| AgentAccuracy::new(self.config.accuracy_ewma_alpha));
                acc.update(detail.agrees == decision_agrees);
            }
        }

        self.state.phase = if reached {
            PbftPhase::Decided
        } else {
            PbftPhase::Failed
        };

        info!(
            round = self.state.round,
            reached,
            agree = agree_count,
            disagree = disagree_count,
            weighted = weighted_agreement,
            "PBFT: consensus evaluation"
        );

        self.history.push(result.clone());
        result
    }

    /// Get the accuracy record for an agent.
    pub fn agent_accuracy(&self, agent_id: &str) -> Option<&AgentAccuracy> {
        self.agent_accuracy.get(agent_id)
    }

    /// Current round number.
    pub fn current_round(&self) -> u64 {
        self.state.round
    }

    /// Current phase.
    pub fn current_phase(&self) -> PbftPhase {
        self.state.phase
    }

    /// Get historical results.
    pub fn history(&self) -> &[ConsensusResult] {
        &self.history
    }
}

// ───────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_validation() {
        let config = PbftConfig::from_fault_tolerance(1);
        assert!(config.validate().is_ok());
        assert_eq!(config.num_agents, 4);
        assert_eq!(config.quorum(), 3);

        let bad = PbftConfig {
            num_agents: 2,
            max_faults: 1,
            ..Default::default()
        };
        assert!(bad.validate().is_err());
    }

    #[test]
    fn test_consensus_reached() {
        let config = PbftConfig::from_fault_tolerance(1); // n=4, quorum=3
        let mut pbft = PbftConsensus::new(config).unwrap();

        pbft.propose("leader", "decision-A").unwrap();

        pbft.prepare("agent1", true, 0.9).unwrap();
        pbft.prepare("agent2", true, 0.8).unwrap();
        pbft.prepare("agent3", true, 0.7).unwrap();
        pbft.prepare("agent4", false, 0.5).unwrap();

        let result = pbft.evaluate();
        assert!(result.reached);
        assert_eq!(result.agree_votes, 3);
        assert_eq!(result.disagree_votes, 1);
    }

    #[test]
    fn test_consensus_not_reached() {
        let config = PbftConfig::from_fault_tolerance(1);
        let mut pbft = PbftConsensus::new(config).unwrap();

        pbft.propose("leader", "decision-B").unwrap();

        pbft.prepare("agent1", true, 0.9).unwrap();
        pbft.prepare("agent2", false, 0.8).unwrap();
        pbft.prepare("agent3", false, 0.7).unwrap();
        pbft.prepare("agent4", false, 0.6).unwrap();

        let result = pbft.evaluate();
        assert!(!result.reached);
    }

    #[test]
    fn test_duplicate_vote_rejected() {
        let config = PbftConfig::from_fault_tolerance(1);
        let mut pbft = PbftConsensus::new(config).unwrap();

        pbft.propose("leader", "decision-C").unwrap();
        pbft.prepare("agent1", true, 0.9).unwrap();

        assert!(pbft.prepare("agent1", false, 0.8).is_err());
    }

    #[test]
    fn test_accuracy_tracking() {
        let config = PbftConfig::from_fault_tolerance(1);
        let mut pbft = PbftConsensus::new(config).unwrap();

        // Round 1: consensus reached.
        pbft.propose("leader", "d1").unwrap();
        pbft.prepare("a1", true, 0.9).unwrap();
        pbft.prepare("a2", true, 0.8).unwrap();
        pbft.prepare("a3", true, 0.7).unwrap();
        pbft.prepare("a4", false, 0.5).unwrap(); // Disagrees.
        pbft.evaluate();

        // a1-a3 agreed with consensus → accuracy should be high.
        let acc1 = pbft.agent_accuracy("a1").unwrap();
        assert!(acc1.accuracy > 0.5);

        // a4 disagreed → accuracy should be low.
        let acc4 = pbft.agent_accuracy("a4").unwrap();
        assert!(acc4.accuracy < 0.5);
    }

    #[test]
    fn test_low_confidence_filtered() {
        let config = PbftConfig {
            min_confidence: 0.5,
            ..PbftConfig::from_fault_tolerance(1)
        };
        let mut pbft = PbftConsensus::new(config).unwrap();

        pbft.propose("leader", "d2").unwrap();
        pbft.prepare("a1", true, 0.9).unwrap();
        pbft.prepare("a2", true, 0.8).unwrap();
        pbft.prepare("a3", true, 0.1).unwrap(); // Below min_confidence.
        pbft.prepare("a4", false, 0.9).unwrap();

        let result = pbft.evaluate();
        // a3's vote should be filtered. Only 2 effective agrees (< quorum of 3).
        assert!(!result.reached);
    }
}
