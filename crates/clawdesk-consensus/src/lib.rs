//! # clawdesk-consensus
//!
//! Byzantine fault-tolerant consensus for multi-agent decisions.
//!
//! Implements a simplified PBFT (Practical Byzantine Fault Tolerance) protocol
//! adapted for LLM agent voting:
//!
//! - **Pre-prepare**: propose a decision
//! - **Prepare**: agents vote with confidence weights
//! - **Commit**: if ≥ 2f+1 matching votes, commit the decision
//!
//! ## Requirements
//!
//! Tolerates f Byzantine (unreliable/hallucinating) agents out of n total,
//! where n ≥ 3f + 1.
//!
//! ## Message complexity: O(n²) per consensus round.

pub mod pbft;
pub mod voting;

pub use pbft::{
    PbftConsensus, PbftConfig, PbftState, PbftMessage, PbftPhase,
    ConsensusResult, ConsensusError,
};
pub use voting::{
    ConfidenceVote, VotingResult, WeightedBallot,
    confidence_weighted_vote, majority_vote, supermajority_vote,
};
