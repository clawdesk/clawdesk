//! # clawdesk-polls
//!
//! Channel-agnostic interactive poll engine with CRDT-compatible vote aggregation.
//!
//! ## Poll State Machine
//! ```text
//! Created → Active → Closed → Tallied
//!                  ↘ Expired
//! ```

pub mod adapters;
pub mod engine;
pub mod state;
pub mod vote;

pub use engine::{PollEngine, PollInput, PollResult, normalize_poll_input};
pub use state::{PollState, PollTransition};
pub use vote::{Ballot, VoteCounter, VoteTally};
pub use adapters::renderer::render_tally;
