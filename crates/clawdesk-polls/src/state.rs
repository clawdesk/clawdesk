//! Poll state machine — deterministic transitions with persistence hooks.

use serde::{Deserialize, Serialize};

/// Poll lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PollState {
    Created,
    Active,
    Closed,
    Tallied,
    Expired,
}

/// State transition event.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PollTransition {
    Publish,
    DurationElapsed,
    ManualClose,
    Tally,
    Timeout,
}

impl PollState {
    /// Apply a transition. Returns the next state or an error if invalid.
    pub fn apply(self, transition: PollTransition) -> Result<Self, String> {
        match (self, transition) {
            (Self::Created, PollTransition::Publish) => Ok(Self::Active),
            (Self::Active, PollTransition::DurationElapsed) => Ok(Self::Closed),
            (Self::Active, PollTransition::ManualClose) => Ok(Self::Closed),
            (Self::Active, PollTransition::Timeout) => Ok(Self::Expired),
            (Self::Closed, PollTransition::Tally) => Ok(Self::Tallied),
            (state, transition) => {
                Err(format!("invalid transition {:?} from state {:?}", transition, state))
            }
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Tallied | Self::Expired)
    }

    pub fn accepts_votes(self) -> bool {
        self == Self::Active
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_transitions() {
        let s = PollState::Created;
        let s = s.apply(PollTransition::Publish).unwrap();
        assert_eq!(s, PollState::Active);
        let s = s.apply(PollTransition::ManualClose).unwrap();
        assert_eq!(s, PollState::Closed);
        let s = s.apply(PollTransition::Tally).unwrap();
        assert_eq!(s, PollState::Tallied);
        assert!(s.is_terminal());
    }

    #[test]
    fn invalid_transition_rejected() {
        let s = PollState::Created;
        assert!(s.apply(PollTransition::Tally).is_err());
    }

    #[test]
    fn active_accepts_votes() {
        assert!(PollState::Active.accepts_votes());
        assert!(!PollState::Closed.accepts_votes());
    }
}
