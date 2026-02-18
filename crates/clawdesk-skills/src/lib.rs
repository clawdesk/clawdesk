//! # clawdesk-skills
//!
//! Composable skill system for ClawDesk agents.
//!
//! A **skill** is a typed unit of agent capability: a prompt fragment + optional
//! tool bindings + parameter schema + dependency declarations. Skills compose
//! into an agent's system prompt via token-budgeted selection.
//!
//! ## Mathematical model
//!
//! Skill selection is a **weighted knapsack** over a token budget B:
//!
//! ```text
//! max  Σ wᵢ · xᵢ     (total value of selected skills)
//! s.t. Σ tᵢ · xᵢ ≤ B  (token budget constraint)
//!      xᵢ ∈ {0, 1}     (binary inclusion)
//!      ∀(i,j) ∈ D: xⱼ ≥ xᵢ  (dependency constraints — if i selected, j must be)
//! ```
//!
//! With dependency constraints this is NP-hard in general, but our instances
//! are small (|skills| < 100) and the dependency graph is a DAG, so we solve
//! in O(k log k) via topological-sort + greedy packing.
//!
//! ## Architecture
//!
//! - `definition` — Skill manifest types (TOML-serializable)
//! - `registry` — In-memory skill registry with FxHashMap O(1) lookup
//! - `resolver` — Dependency resolution via topological sort (Kahn's algorithm)
//! - `loader` — Filesystem skill loader (scans `~/.clawdesk/skills/`)
//! - `selector` — Token-budgeted skill selection (greedy knapsack)

pub mod bundled;
pub mod bundled_design;
pub mod definition;
pub mod executor;
pub mod loader;
pub mod promotion;
pub mod registry;
pub mod resolver;
pub mod scaffold;
pub mod selector;
pub mod templates;
pub mod trigger;
pub mod verification;

pub use bundled::load_bundled_skills;
pub use definition::{Skill, SkillId, SkillManifest, SkillParameter, SkillTrigger};
pub use executor::{SkillExecutor, SkillExecutionResult, SkillExecutionError, SkillHandler};
pub use loader::{LoadResult, SkillLoader};
pub use promotion::{PromotionPipeline, PromotionStage, PipelineEntry, RollbackBuffer, SkillSnapshot, TransitionResult};
pub use registry::SkillRegistry;
pub use resolver::SkillResolver;
pub use selector::{SelectedSkill, SkillSelector};
pub use trigger::{TriggerEvaluator, TriggerResult, TurnContext};
pub use verification::{SkillVerifier, TrustLevel, VerificationResult};
