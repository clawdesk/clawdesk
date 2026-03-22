//! # Curiosity Engine
//!
//! Transforms ClawDesk from a reactive tool to a proactive colleague.
//! Identifies information gaps, schedules autonomous exploration, and
//! manages the idle-time scanning loop.
//!
//! ## Architecture
//!
//! ```text
//! Idle tick (from clawdesk-cron)
//!   ↓
//! CuriosityEngine::tick(world_state)
//!   ├── identify_gaps()          → what don't I know?
//!   ├── prioritize()             → which gaps are worth filling?
//!   ├── budget_check()           → can I afford to explore?
//!   └── plan_explorations()      → concrete tasks to run
//!         ↓
//!       spawned as sub-agents via DynamicOrchestrator
//! ```
//!
//! ## Budget Discipline (Principle 4)
//!
//! Curiosity is token-budgeted. The engine never spends more than
//! `max_exploration_fraction` of available compute on proactive tasks.
//! Default: 10%. When the user is active, exploration pauses entirely.

pub mod gaps;
pub mod budget;
pub mod engine;
pub mod idle;

pub use gaps::{InformationGap, GapSource, GapPriority};
pub use budget::ExplorationBudget;
pub use engine::{CuriosityEngine, CuriosityConfig, ExplorationTask};
pub use idle::{IdleScanner, IdleAction, IdleConfig};
