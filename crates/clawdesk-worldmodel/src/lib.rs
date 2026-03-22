//! # World Model — Persistent Environment State
//!
//! The agent walks into a room and immediately knows: database is slow,
//! 3 PRs are open, CI is red, the user's timezone is PST and it's morning.
//! Before anyone gives a task, it has a situational model.
//!
//! ## Architecture
//!
//! ```text
//! Tool outputs / channel messages / browser observations
//!   ↓
//! WorldModel::observe(perception) → WorldDelta
//!   ↓
//! WorldModel::predict(entity, horizon) → PredictedState
//! WorldModel::stale_entities(threshold) → Vec<EntityId>
//! WorldModel::contradictions() → Vec<Contradiction>
//! ```
//!
//! ## Design Choices
//!
//! - Entities are typed (Service, File, Person, Channel, Environment).
//! - Each entity tracks confidence, staleness, and a freeform state blob.
//! - Relations are a lightweight directed graph (entity→entity with label).
//! - Temporal facts auto-expire. No garbage collection needed — staleness
//!   is computed on read via `last_observed` comparison.
//! - The world model is queryable by the context guard, curiosity engine,
//!   and the proactive orchestrator's SystemContext.

pub mod entity;
pub mod model;

pub use entity::{EntityId, EntityKind, EntityState, TemporalFact, Relation};
pub use model::{WorldModel, WorldDelta, PredictedState, Contradiction, Perception};
