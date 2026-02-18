//! Staged skill promotion pipeline (T-12).
//!
//! Skills progress through a deterministic FSM before becoming active:
//!
//! ```text
//! Submitted → Scanned → Sandboxed-Tested → Approved → Active
//!     ↓           ↓            ↓               ↓
//!     Rejected    Rejected     Rejected        Rejected (revoked)
//! ```
//!
//! ## Rollback
//!
//! The pipeline maintains a circular buffer of the last N active snapshots.
//! Any active skill can be rolled back to a previous version atomically
//! via `ArcSwap` (or similar pattern at the caller level).
//!
//! ## Integration
//!
//! `load_fresh()` should submit skills through the pipeline rather than
//! directly activating them. The pipeline gates enforce that:
//! 1. Security scanner has approved the prompt content
//! 2. Sandbox tests pass (if present)
//! 3. An approval policy is satisfied (auto-approve for trusted, manual for unsigned)

use std::collections::VecDeque;
use std::time::SystemTime;

// ---------------------------------------------------------------------------
// Pipeline stage FSM
// ---------------------------------------------------------------------------

/// Promotion stage for a skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PromotionStage {
    /// Initial submission — skill manifest parsed but not yet scanned.
    Submitted,
    /// Security scanner approved the skill content.
    Scanned,
    /// Skill ran successfully in a sandboxed environment.
    SandboxTested,
    /// Approval policy satisfied (auto or manual).
    Approved,
    /// Skill is live and available for agent use.
    Active,
    /// Skill was rejected at some stage.
    Rejected,
}

impl std::fmt::Display for PromotionStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Submitted => write!(f, "submitted"),
            Self::Scanned => write!(f, "scanned"),
            Self::SandboxTested => write!(f, "sandbox-tested"),
            Self::Approved => write!(f, "approved"),
            Self::Active => write!(f, "active"),
            Self::Rejected => write!(f, "rejected"),
        }
    }
}

/// Rejection reason.
#[derive(Debug, Clone)]
pub struct RejectionReason {
    pub stage: PromotionStage,
    pub message: String,
    pub timestamp: SystemTime,
}

/// A skill's current position in the promotion pipeline.
#[derive(Debug, Clone)]
pub struct PipelineEntry {
    pub skill_id: String,
    pub stage: PromotionStage,
    pub version: u64,
    pub submitted_at: SystemTime,
    pub last_transition: SystemTime,
    pub rejection: Option<RejectionReason>,
}

impl PipelineEntry {
    /// Create a new entry at the Submitted stage.
    pub fn submit(skill_id: impl Into<String>, version: u64) -> Self {
        let now = SystemTime::now();
        Self {
            skill_id: skill_id.into(),
            stage: PromotionStage::Submitted,
            version,
            submitted_at: now,
            last_transition: now,
            rejection: None,
        }
    }
}

/// Result of a stage transition.
#[derive(Debug)]
pub enum TransitionResult {
    /// Successfully advanced to the next stage.
    Advanced(PromotionStage),
    /// Skill was rejected at this stage.
    Rejected(RejectionReason),
    /// Invalid transition (e.g., trying to skip stages).
    InvalidTransition {
        current: PromotionStage,
        attempted: PromotionStage,
    },
}

// ---------------------------------------------------------------------------
// Rollback buffer
// ---------------------------------------------------------------------------

/// A snapshot of an active skill version for rollback.
#[derive(Debug, Clone)]
pub struct SkillSnapshot {
    pub skill_id: String,
    pub version: u64,
    pub content_hash: String,
    pub activated_at: SystemTime,
}

/// Circular buffer of active snapshots for rollback.
#[derive(Debug)]
pub struct RollbackBuffer {
    capacity: usize,
    snapshots: VecDeque<SkillSnapshot>,
}

impl RollbackBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            snapshots: VecDeque::with_capacity(capacity),
        }
    }

    /// Push a new snapshot, evicting the oldest if at capacity.
    pub fn push(&mut self, snapshot: SkillSnapshot) {
        if self.snapshots.len() >= self.capacity {
            self.snapshots.pop_front();
        }
        self.snapshots.push_back(snapshot);
    }

    /// Get the most recent snapshot for a skill.
    pub fn latest(&self, skill_id: &str) -> Option<&SkillSnapshot> {
        self.snapshots.iter().rev().find(|s| s.skill_id == skill_id)
    }

    /// Get the previous version (one before latest) for rollback.
    pub fn previous(&self, skill_id: &str) -> Option<&SkillSnapshot> {
        let mut found_latest = false;
        for snap in self.snapshots.iter().rev() {
            if snap.skill_id == skill_id {
                if found_latest {
                    return Some(snap);
                }
                found_latest = true;
            }
        }
        None
    }

    /// How many snapshots are stored.
    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Promotion pipeline
// ---------------------------------------------------------------------------

/// Staged promotion pipeline for skills.
pub struct PromotionPipeline {
    /// In-flight entries keyed by skill_id.
    entries: std::collections::HashMap<String, PipelineEntry>,
    /// Rollback buffer for active skills.
    pub rollback: RollbackBuffer,
    /// Auto-approve policy: if true, skip manual approval for trusted skills.
    pub auto_approve_trusted: bool,
}

impl PromotionPipeline {
    pub fn new(rollback_capacity: usize) -> Self {
        Self {
            entries: std::collections::HashMap::new(),
            rollback: RollbackBuffer::new(rollback_capacity),
            auto_approve_trusted: true,
        }
    }

    /// Submit a skill for promotion.
    pub fn submit(&mut self, skill_id: &str, version: u64) {
        let entry = PipelineEntry::submit(skill_id, version);
        self.entries.insert(skill_id.to_string(), entry);
    }

    /// Get the current stage for a skill.
    pub fn stage(&self, skill_id: &str) -> Option<PromotionStage> {
        self.entries.get(skill_id).map(|e| e.stage)
    }

    /// Advance a skill to the next valid stage.
    pub fn advance(&mut self, skill_id: &str) -> TransitionResult {
        let entry = match self.entries.get_mut(skill_id) {
            Some(e) => e,
            None => {
                return TransitionResult::InvalidTransition {
                    current: PromotionStage::Submitted,
                    attempted: PromotionStage::Scanned,
                };
            }
        };

        let next = match entry.stage {
            PromotionStage::Submitted => PromotionStage::Scanned,
            PromotionStage::Scanned => PromotionStage::SandboxTested,
            PromotionStage::SandboxTested => PromotionStage::Approved,
            PromotionStage::Approved => PromotionStage::Active,
            PromotionStage::Active | PromotionStage::Rejected => {
                return TransitionResult::InvalidTransition {
                    current: entry.stage,
                    attempted: PromotionStage::Active,
                };
            }
        };

        entry.stage = next;
        entry.last_transition = SystemTime::now();
        TransitionResult::Advanced(next)
    }

    /// Reject a skill at its current stage.
    pub fn reject(&mut self, skill_id: &str, reason: &str) -> TransitionResult {
        let entry = match self.entries.get_mut(skill_id) {
            Some(e) => e,
            None => {
                return TransitionResult::InvalidTransition {
                    current: PromotionStage::Submitted,
                    attempted: PromotionStage::Rejected,
                };
            }
        };

        let rejection = RejectionReason {
            stage: entry.stage,
            message: reason.to_string(),
            timestamp: SystemTime::now(),
        };

        entry.stage = PromotionStage::Rejected;
        entry.last_transition = rejection.timestamp;
        entry.rejection = Some(rejection.clone());

        TransitionResult::Rejected(rejection)
    }

    /// Activate a skill and push to rollback buffer.
    pub fn activate(&mut self, skill_id: &str, content_hash: &str) -> bool {
        let entry = match self.entries.get_mut(skill_id) {
            Some(e) if e.stage == PromotionStage::Approved => e,
            _ => return false,
        };

        entry.stage = PromotionStage::Active;
        entry.last_transition = SystemTime::now();

        self.rollback.push(SkillSnapshot {
            skill_id: skill_id.to_string(),
            version: entry.version,
            content_hash: content_hash.to_string(),
            activated_at: entry.last_transition,
        });

        true
    }

    /// Get the pipeline entry for a skill.
    pub fn entry(&self, skill_id: &str) -> Option<&PipelineEntry> {
        self.entries.get(skill_id)
    }

    /// List all skills at a given stage.
    pub fn at_stage(&self, stage: PromotionStage) -> Vec<&PipelineEntry> {
        self.entries
            .values()
            .filter(|e| e.stage == stage)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_full_promotion_lifecycle() {
        let mut pipeline = PromotionPipeline::new(5);

        pipeline.submit("core/search", 1);
        assert_eq!(pipeline.stage("core/search"), Some(PromotionStage::Submitted));

        // Advance through all stages.
        match pipeline.advance("core/search") {
            TransitionResult::Advanced(s) => assert_eq!(s, PromotionStage::Scanned),
            _ => panic!("expected Scanned"),
        }

        match pipeline.advance("core/search") {
            TransitionResult::Advanced(s) => assert_eq!(s, PromotionStage::SandboxTested),
            _ => panic!("expected SandboxTested"),
        }

        match pipeline.advance("core/search") {
            TransitionResult::Advanced(s) => assert_eq!(s, PromotionStage::Approved),
            _ => panic!("expected Approved"),
        }

        assert!(pipeline.activate("core/search", "abc123"));
        assert_eq!(pipeline.stage("core/search"), Some(PromotionStage::Active));
        assert_eq!(pipeline.rollback.len(), 1);
    }

    #[test]
    fn test_rejection() {
        let mut pipeline = PromotionPipeline::new(5);
        pipeline.submit("malicious/skill", 1);

        // Advance to Scanned, then reject.
        pipeline.advance("malicious/skill");
        match pipeline.reject("malicious/skill", "contains eval() pattern") {
            TransitionResult::Rejected(r) => {
                assert_eq!(r.stage, PromotionStage::Scanned);
                assert!(r.message.contains("eval()"));
            }
            _ => panic!("expected Rejected"),
        }

        assert_eq!(
            pipeline.stage("malicious/skill"),
            Some(PromotionStage::Rejected)
        );
    }

    #[test]
    fn test_cannot_advance_past_active() {
        let mut pipeline = PromotionPipeline::new(5);
        pipeline.submit("s", 1);
        pipeline.advance("s"); // Scanned
        pipeline.advance("s"); // SandboxTested
        pipeline.advance("s"); // Approved
        pipeline.activate("s", "hash");

        match pipeline.advance("s") {
            TransitionResult::InvalidTransition { current, .. } => {
                assert_eq!(current, PromotionStage::Active);
            }
            _ => panic!("expected InvalidTransition"),
        }
    }

    #[test]
    fn test_rollback_buffer() {
        let mut buf = RollbackBuffer::new(3);

        for v in 1..=4 {
            buf.push(SkillSnapshot {
                skill_id: "s".into(),
                version: v,
                content_hash: format!("h{}", v),
                activated_at: SystemTime::now(),
            });
        }

        // Capacity is 3, so version 1 should be evicted.
        assert_eq!(buf.len(), 3);
        assert_eq!(buf.latest("s").unwrap().version, 4);
        assert_eq!(buf.previous("s").unwrap().version, 3);
    }

    #[test]
    fn test_at_stage_query() {
        let mut pipeline = PromotionPipeline::new(5);
        pipeline.submit("a", 1);
        pipeline.submit("b", 1);
        pipeline.advance("a"); // a → Scanned

        let submitted = pipeline.at_stage(PromotionStage::Submitted);
        assert_eq!(submitted.len(), 1);
        assert_eq!(submitted[0].skill_id, "b");

        let scanned = pipeline.at_stage(PromotionStage::Scanned);
        assert_eq!(scanned.len(), 1);
        assert_eq!(scanned[0].skill_id, "a");
    }
}
