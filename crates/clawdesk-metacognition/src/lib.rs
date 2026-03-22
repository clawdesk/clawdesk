//! # Metacognitive Monitoring
//!
//! The self-reflection layer that monitors the agent's own reasoning process.
//! Detects when the agent is stuck, evaluates whether the current approach
//! is promising, and suggests strategy switches.
//!
//! ## Architecture
//!
//! ```text
//! TurnOutcome stream
//!   ↓
//! MetacognitiveMonitor::observe(outcome)
//!   ├── StuckDetector::update()      → stuck / not-stuck
//!   ├── ApproachEvaluator::score()   → confidence in current approach
//!   └── Verdict                      → OnTrack / Stuck(reason) / WrongApproach(alt)
//!         ↓
//!       injected as system message in runner loop
//! ```
//!
//! ## Design Principles
//!
//! 1. **O(1) per turn** — The stuck check is a constant-time comparison.
//!    Full metacognitive evaluation runs only when the quick check fires.
//!
//! 2. **No false pauses** — Tool repetition is only a signal, not a verdict.
//!    We require *convergence* (repeated tools + no output change) before
//!    declaring stuck.
//!
//! 3. **Calibrated confidence** — The monitor tracks its own accuracy:
//!    how often "stuck" verdicts led to strategy switches that improved
//!    outcomes. The stuck threshold adjusts accordingly.

pub mod stuck;
pub mod approach;
pub mod monitor;

pub use stuck::{StuckDetector, StuckReport, StuckSignal};
pub use approach::{ApproachEvaluator, ApproachScore, AlternativeApproach};
pub use monitor::{MetacognitiveMonitor, Verdict, MetacognitiveConfig, TurnSnapshot};
