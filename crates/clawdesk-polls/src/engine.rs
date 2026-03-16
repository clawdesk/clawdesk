//! Poll engine — creation, validation, and lifecycle management.

use crate::state::{PollState, PollTransition};
use crate::vote::{Ballot, VoteCounter, VoteTally};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Input for creating a poll.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollInput {
    pub question: String,
    pub options: Vec<String>,
    pub max_selections: Option<usize>,
    pub duration_secs: Option<u64>,
    pub duration_hours: Option<u64>,
    pub anonymous: bool,
}

/// Validated poll.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollResult {
    pub id: String,
    pub question: String,
    pub options: Vec<String>,
    pub max_selections: usize,
    pub duration: Duration,
    pub anonymous: bool,
}

/// Normalize and validate poll input.
pub fn normalize_poll_input(input: &PollInput) -> Result<PollResult, PollError> {
    if input.options.len() < 2 {
        return Err(PollError::TooFewOptions { count: input.options.len() });
    }
    if input.options.len() > 20 {
        return Err(PollError::TooManyOptions { count: input.options.len() });
    }
    if input.question.is_empty() {
        return Err(PollError::EmptyQuestion);
    }

    let max_sel = input.max_selections.unwrap_or(1).min(input.options.len());
    if max_sel == 0 {
        return Err(PollError::InvalidMaxSelections);
    }

    // Duration resolution: seconds override hours.
    let duration = if let Some(secs) = input.duration_secs {
        Duration::from_secs(secs.max(5).min(604_800)) // 5s to 7 days
    } else if let Some(hours) = input.duration_hours {
        Duration::from_secs(hours.max(1).min(168) * 3600)
    } else {
        Duration::from_secs(3600) // 1 hour default
    };

    Ok(PollResult {
        id: uuid::Uuid::new_v4().to_string(),
        question: input.question.clone(),
        options: input.options.clone(),
        max_selections: max_sel,
        duration,
        anonymous: input.anonymous,
    })
}

/// Active poll instance with vote tracking.
pub struct PollEngine {
    pub poll: PollResult,
    pub state: PollState,
    pub votes: VoteCounter,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl PollEngine {
    pub fn new(poll: PollResult) -> Self {
        let option_count = poll.options.len();
        Self {
            poll,
            state: PollState::Created,
            votes: VoteCounter::new(option_count),
            created_at: chrono::Utc::now(),
        }
    }

    /// Publish the poll (Created → Active).
    pub fn publish(&mut self) -> Result<(), PollError> {
        self.state = self.state.apply(PollTransition::Publish)
            .map_err(|e| PollError::InvalidTransition(e))?;
        Ok(())
    }

    /// Cast a ballot.
    pub fn cast_vote(&mut self, ballot: &Ballot) -> Result<(), PollError> {
        if !self.state.accepts_votes() {
            return Err(PollError::NotAcceptingVotes);
        }
        if ballot.selections.len() > self.poll.max_selections {
            return Err(PollError::TooManySelections {
                selected: ballot.selections.len(),
                max: self.poll.max_selections,
            });
        }
        for &idx in &ballot.selections {
            if idx >= self.poll.options.len() {
                return Err(PollError::InvalidOption { index: idx });
            }
            self.votes.increment(&ballot.channel_instance, idx);
        }
        Ok(())
    }

    /// Close the poll and tally votes.
    pub fn close_and_tally(&mut self) -> Result<VoteTally, PollError> {
        self.state = self.state.apply(PollTransition::ManualClose)
            .map_err(|e| PollError::InvalidTransition(e))?;
        self.state = self.state.apply(PollTransition::Tally)
            .map_err(|e| PollError::InvalidTransition(e))?;
        Ok(self.votes.tally())
    }

    /// Check if the poll has expired.
    pub fn is_expired(&self) -> bool {
        let elapsed = chrono::Utc::now()
            .signed_duration_since(self.created_at)
            .to_std()
            .unwrap_or_default();
        self.state == PollState::Active && elapsed > self.poll.duration
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PollError {
    #[error("too few options ({count}, minimum 2)")]
    TooFewOptions { count: usize },
    #[error("too many options ({count}, maximum 20)")]
    TooManyOptions { count: usize },
    #[error("empty question")]
    EmptyQuestion,
    #[error("invalid max selections")]
    InvalidMaxSelections,
    #[error("poll is not accepting votes")]
    NotAcceptingVotes,
    #[error("too many selections ({selected}, max {max})")]
    TooManySelections { selected: usize, max: usize },
    #[error("invalid option index: {index}")]
    InvalidOption { index: usize },
    #[error("invalid transition: {0}")]
    InvalidTransition(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_valid_poll() {
        let input = PollInput {
            question: "Best language?".into(),
            options: vec!["Rust".into(), "Go".into(), "Zig".into()],
            max_selections: None,
            duration_secs: Some(600),
            duration_hours: None,
            anonymous: false,
        };
        let result = normalize_poll_input(&input).unwrap();
        assert_eq!(result.max_selections, 1);
        assert_eq!(result.duration, Duration::from_secs(600));
    }

    #[test]
    fn normalize_rejects_too_few_options() {
        let input = PollInput {
            question: "?".into(),
            options: vec!["Only one".into()],
            max_selections: None,
            duration_secs: None,
            duration_hours: None,
            anonymous: false,
        };
        assert!(normalize_poll_input(&input).is_err());
    }

    #[test]
    fn full_poll_lifecycle() {
        let input = PollInput {
            question: "Lunch?".into(),
            options: vec!["Pizza".into(), "Sushi".into()],
            max_selections: Some(1),
            duration_secs: Some(300),
            duration_hours: None,
            anonymous: false,
        };
        let poll = normalize_poll_input(&input).unwrap();
        let mut engine = PollEngine::new(poll);
        engine.publish().unwrap();

        let ballot = Ballot {
            voter_id: "user1".into(),
            selections: vec![0],
            timestamp: chrono::Utc::now().to_rfc3339(),
            channel_instance: "discord".into(),
        };
        engine.cast_vote(&ballot).unwrap();

        let tally = engine.close_and_tally().unwrap();
        assert_eq!(tally.total_votes, 1);
        assert_eq!(tally.counts[0], 1);
    }
}
