//! # clawdesk-conscious
//!
//! Conscious Execution Gateway — graduated awareness pipeline for agent tool control.
//!
//! ## Neuroscience-Inspired Architecture
//!
//! Human consciousness isn't a binary gate (approve/deny). It's a graduated
//! attention system with multiple layers. This crate implements five levels:
//!
//! | Level | Name           | Analog                    | Behavior                    |
//! |-------|----------------|---------------------------|-----------------------------|
//! | L0    | Reflexive      | Automatic (breathing)     | Execute instantly, no gate  |
//! | L1    | Preconscious   | Reticular activating sys  | Sentinel anomaly detection  |
//! | L2    | Deliberative   | Prefrontal deliberation   | LLM self-review before exec |
//! | L3    | Critical       | Libet's conscious veto    | Human must approve          |
//! | L4    | Retrospective  | Episodic consolidation    | Immutable trace → feedback  |
//!
//! ## Design Principles
//!
//! 1. **Continuous risk scoring** — `RiskScore = (base + contextual + sentinel_boost).clamp(0.0, 1.0)`
//!    maps to graduated levels via configurable thresholds.
//! 2. **Dynamic escalation** — Sentinel EWMA detects anomalies and boosts risk scores.
//! 3. **Retrospective learning** — Human veto rates feed back into base risk scores.
//! 4. **Replaces three systems** — Unifies `tool_policy + permission_engine + approval_gate`.
//!
//! ## Homeostatic Controller
//!
//! PID-like regulation of five vital signs (token burn rate, cost rate, error rate,
//! latency P95, memory pressure) with automatic corrective actions.

pub mod awareness;
pub mod sentinel;
pub mod deliberation;
pub mod veto;
pub mod trace;
pub mod gateway;
pub mod homeostasis;
pub mod workspace;
pub mod agent_selector;

pub use awareness::{AwarenessClassifier, ConsciousnessLevel, RiskScore, LevelThresholds};
pub use sentinel::{Sentinel, Escalation, SentinelSignal};
pub use deliberation::{Deliberator, DeliberationOutcome};
pub use veto::{VetoGate, VetoDecision};
pub use trace::{ConsciousTrace, TraceEntry};
pub use gateway::{ConsciousGateway, GatewayOutcome, SessionContext};
pub use homeostasis::{HomeostaticController, VitalSign, HomeostaticAction, VitalSetpoints};
pub use workspace::{GlobalWorkspace, CognitiveEvent};
pub use agent_selector::{AgentSelector, AgentCandidate, AgentFeatures, AgentCapabilities};
