//! # Cognitive Self-Diagnosis
//!
//! "I feel sluggish today, maybe I need coffee" →
//! "The embedding service is returning 500s, let me fall back to BM25-only retrieval."
//!
//! When the agent itself is slow, degraded, or misconfigured, it should know.
//! If the embedding provider is failing, the agent should treat it as
//! "my memory system is broken" rather than "recall returned no results."
//!
//! ## Architecture
//!
//! ```text
//! Component health signals (latency, error rate, timeouts)
//!   ↓
//! SelfDiagnostics::record(component, observation)
//!   ↓
//! SelfDiagnostics::diagnose() → Vec<DiagnosisResult>
//!   ├── healthy → no action
//!   ├── degraded → switch to fallback / reduce quality
//!   └── critical → notify user + self-heal if possible
//! ```

pub mod component;
pub mod diagnostics;

pub use component::{Component, ComponentHealth, HealthStatus, HealthObservation};
pub use diagnostics::{SelfDiagnostics, DiagnosisResult, DegradationAction, DiagConfig};
