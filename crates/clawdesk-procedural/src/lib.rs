//! # Procedural Memory
//!
//! Records *what worked* and *what didn't* — action sequences tied to
//! contexts, with success rates and temporal decay. Unlike declarative
//! memory (facts), procedural memory stores *processes* (tool sequences,
//! approach patterns) that the agent can replay.
//!
//! ## Architecture
//!
//! ```text
//! Agent completes a task
//!   ↓
//! ProceduralMemory::record_episode(context, actions, reward)
//!   ↓
//! Before next execution
//!   ↓
//! ProceduralMemory::suggest(context) → Vec<(ActionPattern, confidence)>
//! ProceduralMemory::inhibited(context) → Vec<InhibitedAction>
//!   ↓
//! Injected into system prompt: "In similar contexts, this worked..."
//! Injected into tool filter: "Don't try X, it fails here."
//! ```
//!
//! ## Design
//!
//! - Context is represented as a bag-of-keywords (cheap, no embedding needed).
//! - Similarity uses keyword Jaccard overlap.
//! - Patterns consolidate over time: near-duplicate sequences merge,
//!   frequency increments, and reward EWMAs.
//! - Inhibition has temporal decay — a tool that failed 6 months ago
//!   might work now. Suppression strength halves every `decay_half_life`.

pub mod pattern;
pub mod inhibition;
pub mod memory;

pub use pattern::{ActionPattern, Action, ActionOutcome};
pub use inhibition::{InhibitionGate, InhibitedAction};
pub use memory::{ProceduralMemory, ProceduralConfig, EpisodeRecord, PatternSuggestion};
