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

pub mod archetype;
pub mod browser_skill;
pub mod bundled;
pub mod capability;
pub mod bundled_design;
pub mod bundled_packs;
pub mod agent_filter;
pub mod commands;
pub mod corpus_ingest;
pub mod dag_installer;
pub mod definition;
pub mod distribution;
pub mod eligibility;
pub mod embedded;
pub mod embedded_openclaw;
pub mod env_injection;
pub mod executor;
pub mod federated_registry;
pub mod installer;
pub mod journal;
pub mod layered_loader;
pub mod life_os;
pub mod loader;
pub mod openclaw_adapter;
pub mod orchestrator;
pub mod pack;
pub mod pack_distribution;
pub mod promotion;
pub mod registry;
pub mod resolver;
pub mod scaffold;
pub mod selector;
pub mod semantic_router;
pub mod skill_provider;
pub mod snapshot;
pub mod quality_loop;
pub mod store;
pub mod store_cache;
pub mod store_federation;
pub mod store_sync;
pub mod templates;
pub mod trigger;
pub mod trigram_index;
pub mod watcher;
pub mod verification;

pub use bundled::load_bundled_skills;
pub use definition::{Skill, SkillId, SkillManifest, SkillParameter, SkillTrigger};
pub use executor::{SkillExecutor, SkillExecutionResult, SkillExecutionError, SkillHandler};
pub use loader::{LoadResult, SkillLoader};
pub use promotion::{PromotionPipeline, PromotionStage, PipelineEntry, RollbackBuffer, SkillSnapshot, TransitionResult};
pub use registry::SkillRegistry;
pub use skill_provider::OrchestratorSkillProvider;
pub use resolver::SkillResolver;
pub use selector::{SelectedSkill, SkillSelector};
pub use capability::{SkillCapability, TrustLevel as CapabilityTrustLevel, CapabilityIndex, CapabilityEntry};
pub use trigger::{TriggerEvaluator, TriggerResult, TurnContext};
pub use verification::{SkillVerifier, TrustLevel, VerificationResult};
pub use corpus_ingest::{CorpusIngest, IngestResult, IngestLockfile};
pub use federated_registry::{FederatedRegistry, ContentAddress, FederationSource, SourcePriority};
pub use store_federation::{StoreFederationBridge, BridgeEvent};
pub use store_sync::{SyncConfig, SyncState, SyncResult, SyncError, compute_merkle_root};
pub use store_cache::{StoreCache, CacheError};
pub use archetype::{Archetype, ArchetypeRegistry, ResolvedArchetype};
pub use browser_skill::BrowserSkillProvider;
pub use bundled_packs::load_bundled_packs;
pub use pack::{PackId, PackRegistry, PackTier, SkillPack};
pub use pack_distribution::{PackContentAddress, PackResolver, PackSourceTier};
pub use quality_loop::{QualityTracker, QualitySignal, WeightOptimizer, QualityGate};
