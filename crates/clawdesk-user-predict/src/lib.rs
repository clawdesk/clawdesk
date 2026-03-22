//! # User Prediction — Temporal Pattern Matching
//!
//! "It's Monday morning and the user always asks for a standup summary."
//! The agent should have it ready.
//!
//! This crate identifies temporal patterns in user interactions:
//! - What they ask for at specific times/days
//! - Recurring workflows (morning standup, weekly deploy, end-of-day review)
//! - Seasonal/cyclical needs
//!
//! Feeds into the `CuriosityEngine` as `GapSource::PredictedNeed` and
//! the `ProactiveOrchestrator` for pre-emptive action.

pub mod pattern;
pub mod predictor;

pub use pattern::{TemporalPattern, InteractionRecord, TimeSlot};
pub use predictor::{UserPredictor, PredictorConfig, PredictedNeed, PreparedAction};
