//! # Theory of Mind — Per-User Modeling
//!
//! A good teacher gauges whether the student understands.
//! A good colleague adjusts their explanation based on who they're talking to.
//!
//! This crate models each user's expertise, communication preferences,
//! frustration signals, and unspoken needs. It feeds into the PromptAssembler
//! to adapt response style per-user.
//!
//! ## Architecture
//!
//! ```text
//! User message
//!   ↓
//! UserModel::update_from_message(msg)
//!   ├── infer expertise level (keyword, code density, question depth)
//!   ├── detect frustration signals (repetition, shortened msgs, "??" patterns)
//!   └── update satisfaction EWMA
//!   ↓
//! UserModel::should_explain(topic) → bool
//! UserModel::response_style() → StyleHints
//! UserModel::predict_needs(context) → Vec<InferredNeed>
//! ```

pub mod expertise;
pub mod frustration;
pub mod user;

pub use expertise::{ExpertiseLevel, ExpertiseProfile, Domain};
pub use frustration::{FrustrationDetector, FrustrationLevel};
pub use user::{UserModel, StyleHints, InferredNeed};
