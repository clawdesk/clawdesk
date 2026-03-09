//! Structural Diff Engine — Field-Level Change Classification.
//!
//! Compares two configuration snapshots and produces a detailed diff
//! recording which fields changed, the type of change, and its impact
//! classification for the reload pipeline.
//!
//! ## Impact Classification
//!
//! | Classification | Meaning | Action |
//! |---------------|---------|--------|
//! | `NoOp` | No change | Skip |
//! | `HotReload` | Can be applied via ArcSwap | Swap atomically |
//! | `WarmRestart` | Requires draining in-flight | Drain → swap |
//! | `ColdRestart` | Requires full service restart | Schedule restart |

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info};

// ---------------------------------------------------------------------------
// Impact classification
// ---------------------------------------------------------------------------

/// Impact level of a configuration change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ChangeImpact {
    /// No change detected.
    NoOp = 0,
    /// Hot-reloadable: atomic ArcSwap, no disruption.
    HotReload = 1,
    /// Warm restart: drain in-flight requests, then swap.
    WarmRestart = 2,
    /// Cold restart: full service restart required.
    ColdRestart = 3,
}

impl ChangeImpact {
    /// The maximum (most severe) of two impacts.
    pub fn max(self, other: Self) -> Self {
        if self >= other { self } else { other }
    }
}

impl std::fmt::Display for ChangeImpact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoOp => write!(f, "no-op"),
            Self::HotReload => write!(f, "hot-reload"),
            Self::WarmRestart => write!(f, "warm-restart"),
            Self::ColdRestart => write!(f, "cold-restart"),
        }
    }
}

// ---------------------------------------------------------------------------
// Change kind
// ---------------------------------------------------------------------------

/// Type of change at the field level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChangeKind {
    /// Field was added (not present in old config).
    Added,
    /// Field was removed (not present in new config).
    Removed,
    /// Field value was modified.
    Modified {
        old_summary: String,
        new_summary: String,
    },
    /// No change.
    Unchanged,
}

impl ChangeKind {
    pub fn is_changed(&self) -> bool {
        !matches!(self, Self::Unchanged)
    }
}

// ---------------------------------------------------------------------------
// Config delta
// ---------------------------------------------------------------------------

/// A single field-level change record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDelta {
    /// Dotted path to the field (e.g., "providers.openai.api_key").
    pub path: String,
    /// What kind of change occurred.
    pub kind: ChangeKind,
    /// Impact classification for this field change.
    pub impact: ChangeImpact,
    /// Registry this field belongs to.
    pub registry: RegistryKind,
}

/// Which registry a field belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RegistryKind {
    Channels,
    Providers,
    Skills,
    Agents,
    A2A,
    Config,
}

impl std::fmt::Display for RegistryKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Channels => write!(f, "channels"),
            Self::Providers => write!(f, "providers"),
            Self::Skills => write!(f, "skills"),
            Self::Agents => write!(f, "agents"),
            Self::A2A => write!(f, "a2a"),
            Self::Config => write!(f, "config"),
        }
    }
}

/// Complete diff between two configuration generations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigDelta {
    /// Source generation number.
    pub from_generation: u64,
    /// Target generation number.
    pub to_generation: u64,
    /// Individual field changes.
    pub fields: Vec<FieldDelta>,
    /// Maximum impact across all fields.
    pub max_impact: ChangeImpact,
    /// Summary per registry.
    pub registry_summary: HashMap<String, RegistryDiffSummary>,
    /// When this diff was computed.
    pub computed_at: DateTime<Utc>,
}

/// Summary of changes within a single registry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryDiffSummary {
    pub added: usize,
    pub removed: usize,
    pub modified: usize,
    pub unchanged: usize,
    pub max_impact: ChangeImpact,
}

impl ConfigDelta {
    /// Whether there are any actual changes.
    pub fn has_changes(&self) -> bool {
        self.fields.iter().any(|f| f.kind.is_changed())
    }

    /// Number of changed fields.
    pub fn change_count(&self) -> usize {
        self.fields.iter().filter(|f| f.kind.is_changed()).count()
    }

    /// Get changes filtered by registry.
    pub fn for_registry(&self, registry: RegistryKind) -> Vec<&FieldDelta> {
        self.fields
            .iter()
            .filter(|f| f.registry == registry)
            .collect()
    }

    /// Get changes filtered by impact level.
    pub fn by_impact(&self, impact: ChangeImpact) -> Vec<&FieldDelta> {
        self.fields
            .iter()
            .filter(|f| f.impact == impact)
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Diff engine
// ---------------------------------------------------------------------------

/// Impact classification rules for known field patterns.
///
/// Maps field path prefixes to their impact classification.
#[derive(Debug, Clone)]
pub struct ImpactRules {
    rules: Vec<(String, ChangeImpact)>,
    default_impact: ChangeImpact,
}

impl Default for ImpactRules {
    fn default() -> Self {
        Self {
            rules: vec![
                // Credential changes are hot-reloadable.
                ("providers.*.api_key".into(), ChangeImpact::HotReload),
                ("providers.*.token".into(), ChangeImpact::HotReload),
                // Skill additions/removals are hot-reloadable.
                ("skills.*".into(), ChangeImpact::HotReload),
                // Agent config changes are hot-reloadable.
                ("agents.*".into(), ChangeImpact::HotReload),
                // Channel structural changes need warm restart.
                ("channels.*.type".into(), ChangeImpact::WarmRestart),
                ("channels.*.protocol".into(), ChangeImpact::WarmRestart),
                // Transport/bind changes need cold restart.
                ("server.bind_address".into(), ChangeImpact::ColdRestart),
                ("server.port".into(), ChangeImpact::ColdRestart),
                ("server.tls.*".into(), ChangeImpact::ColdRestart),
            ],
            default_impact: ChangeImpact::HotReload,
        }
    }
}

impl ImpactRules {
    /// Classify a field path's change impact.
    pub fn classify(&self, path: &str) -> ChangeImpact {
        for (pattern, impact) in &self.rules {
            if glob_match(pattern, path) {
                return *impact;
            }
        }
        self.default_impact
    }
}

/// Simple glob matching for dotted paths (supports `*` as single-segment wildcard).
fn glob_match(pattern: &str, path: &str) -> bool {
    let pat_parts: Vec<&str> = pattern.split('.').collect();
    let path_parts: Vec<&str> = path.split('.').collect();

    if pat_parts.len() != path_parts.len() {
        return false;
    }

    pat_parts
        .iter()
        .zip(path_parts.iter())
        .all(|(p, s)| *p == "*" || *p == *s)
}

// ---------------------------------------------------------------------------
// Diff builder
// ---------------------------------------------------------------------------

/// Configuration diff engine.
///
/// Compares two sets of named key-value entries and produces a `ConfigDelta`.
pub struct DiffEngine {
    rules: ImpactRules,
}

impl DiffEngine {
    pub fn new(rules: ImpactRules) -> Self {
        Self { rules }
    }

    pub fn with_defaults() -> Self {
        Self::new(ImpactRules::default())
    }

    /// Compute a diff between two flat key-value representations.
    ///
    /// `old` and `new` map dotted field paths to string summaries of their values.
    /// The `registry` parameter tags all deltas with their owning registry.
    pub fn diff_flat(
        &self,
        old: &HashMap<String, String>,
        new: &HashMap<String, String>,
        from_generation: u64,
        to_generation: u64,
        registry: RegistryKind,
    ) -> ConfigDelta {
        let mut fields = Vec::new();

        // Detect modified and removed.
        for (key, old_val) in old {
            match new.get(key) {
                Some(new_val) if new_val != old_val => {
                    fields.push(FieldDelta {
                        path: key.clone(),
                        kind: ChangeKind::Modified {
                            old_summary: old_val.clone(),
                            new_summary: new_val.clone(),
                        },
                        impact: self.rules.classify(key),
                        registry,
                    });
                }
                Some(_) => {
                    // Unchanged.
                }
                None => {
                    fields.push(FieldDelta {
                        path: key.clone(),
                        kind: ChangeKind::Removed,
                        impact: self.rules.classify(key),
                        registry,
                    });
                }
            }
        }

        // Detect added.
        for (key, new_val) in new {
            if !old.contains_key(key) {
                fields.push(FieldDelta {
                    path: key.clone(),
                    kind: ChangeKind::Added,
                    impact: self.rules.classify(key),
                    registry,
                });
            }
        }

        let max_impact = fields
            .iter()
            .filter(|f| f.kind.is_changed())
            .map(|f| f.impact)
            .max()
            .unwrap_or(ChangeImpact::NoOp);

        let mut registry_summary = HashMap::new();
        let summary = RegistryDiffSummary {
            added: fields.iter().filter(|f| matches!(f.kind, ChangeKind::Added)).count(),
            removed: fields.iter().filter(|f| matches!(f.kind, ChangeKind::Removed)).count(),
            modified: fields.iter().filter(|f| matches!(f.kind, ChangeKind::Modified { .. })).count(),
            unchanged: 0,
            max_impact,
        };
        registry_summary.insert(registry.to_string(), summary);

        ConfigDelta {
            from_generation,
            to_generation,
            fields,
            max_impact,
            registry_summary,
            computed_at: Utc::now(),
        }
    }

    /// Merge multiple per-registry deltas into a single combined delta.
    pub fn merge_deltas(&self, deltas: Vec<ConfigDelta>) -> ConfigDelta {
        let mut all_fields = Vec::new();
        let mut all_summaries = HashMap::new();
        let mut max_impact = ChangeImpact::NoOp;
        let mut from_gen = u64::MAX;
        let mut to_gen = 0u64;

        for delta in deltas {
            from_gen = from_gen.min(delta.from_generation);
            to_gen = to_gen.max(delta.to_generation);
            max_impact = max_impact.max(delta.max_impact);
            all_fields.extend(delta.fields);
            all_summaries.extend(delta.registry_summary);
        }

        ConfigDelta {
            from_generation: from_gen,
            to_generation: to_gen,
            fields: all_fields,
            max_impact,
            registry_summary: all_summaries,
            computed_at: Utc::now(),
        }
    }
}

// ---------------------------------------------------------------------------
// Reload plan
// ---------------------------------------------------------------------------

/// A reload plan derived from a ConfigDelta.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReloadPlan {
    /// The overall impact level (determines strategy).
    pub impact: ChangeImpact,
    /// Registries that need to be swapped.
    pub registries_to_reload: Vec<String>,
    /// Whether to drain in-flight requests first.
    pub drain_required: bool,
    /// Whether a full process restart is needed.
    pub restart_required: bool,
    /// Human-readable summary.
    pub summary: String,
}

impl ReloadPlan {
    /// Generate a reload plan from a config delta.
    pub fn from_delta(delta: &ConfigDelta) -> Self {
        let registries_to_reload: Vec<String> = delta
            .registry_summary
            .iter()
            .filter(|(_, s)| s.added > 0 || s.removed > 0 || s.modified > 0)
            .map(|(name, _)| name.clone())
            .collect();

        let impact = delta.max_impact;
        let drain_required = impact >= ChangeImpact::WarmRestart;
        let restart_required = impact >= ChangeImpact::ColdRestart;

        let summary = format!(
            "{} changes across {} registries (impact: {})",
            delta.change_count(),
            registries_to_reload.len(),
            impact
        );

        info!(
            impact = %impact,
            changes = delta.change_count(),
            registries = ?registries_to_reload,
            "reload plan generated"
        );

        Self {
            impact,
            registries_to_reload,
            drain_required,
            restart_required,
            summary,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn engine() -> DiffEngine {
        DiffEngine::with_defaults()
    }

    #[test]
    fn no_changes_is_noop() {
        let mut old = HashMap::new();
        old.insert("skills.search".into(), "v1".into());

        let delta = engine().diff_flat(&old, &old, 1, 2, RegistryKind::Skills);
        assert_eq!(delta.max_impact, ChangeImpact::NoOp);
        assert!(!delta.has_changes());
    }

    #[test]
    fn added_field_detected() {
        let old = HashMap::new();
        let mut new = HashMap::new();
        new.insert("skills.new_skill".into(), "v1".into());

        let delta = engine().diff_flat(&old, &new, 1, 2, RegistryKind::Skills);
        assert!(delta.has_changes());
        assert_eq!(delta.change_count(), 1);
        assert!(matches!(delta.fields[0].kind, ChangeKind::Added));
    }

    #[test]
    fn removed_field_detected() {
        let mut old = HashMap::new();
        old.insert("skills.old_skill".into(), "v1".into());
        let new = HashMap::new();

        let delta = engine().diff_flat(&old, &new, 1, 2, RegistryKind::Skills);
        assert_eq!(delta.change_count(), 1);
        assert!(matches!(delta.fields[0].kind, ChangeKind::Removed));
    }

    #[test]
    fn modified_field_detected() {
        let mut old = HashMap::new();
        old.insert("providers.openai.api_key".into(), "old_key".into());
        let mut new = HashMap::new();
        new.insert("providers.openai.api_key".into(), "new_key".into());

        let delta = engine().diff_flat(&old, &new, 1, 2, RegistryKind::Providers);
        assert_eq!(delta.change_count(), 1);
        assert!(matches!(delta.fields[0].kind, ChangeKind::Modified { .. }));
    }

    #[test]
    fn impact_classification() {
        let rules = ImpactRules::default();
        assert_eq!(
            rules.classify("providers.openai.api_key"),
            ChangeImpact::HotReload
        );
        assert_eq!(
            rules.classify("server.bind_address"),
            ChangeImpact::ColdRestart
        );
        assert_eq!(
            rules.classify("channels.slack.type"),
            ChangeImpact::WarmRestart
        );
    }

    #[test]
    fn glob_matching() {
        assert!(glob_match("providers.*.api_key", "providers.openai.api_key"));
        assert!(!glob_match("providers.*.api_key", "providers.openai.model"));
        assert!(!glob_match("providers.*.api_key", "skills.search"));
    }

    #[test]
    fn reload_plan_from_delta() {
        let mut old = HashMap::new();
        old.insert("server.port".into(), "8080".into());
        let mut new = HashMap::new();
        new.insert("server.port".into(), "9090".into());

        let delta = engine().diff_flat(&old, &new, 1, 2, RegistryKind::Config);
        let plan = ReloadPlan::from_delta(&delta);
        assert!(plan.restart_required);
        assert!(plan.drain_required);
        assert_eq!(plan.impact, ChangeImpact::ColdRestart);
    }

    #[test]
    fn merge_deltas() {
        let e = engine();
        let mut old1 = HashMap::new();
        old1.insert("skills.a".into(), "v1".into());
        let mut new1 = HashMap::new();
        new1.insert("skills.a".into(), "v2".into());

        let delta1 = e.diff_flat(&old1, &new1, 1, 2, RegistryKind::Skills);

        let mut old2 = HashMap::new();
        old2.insert("server.port".into(), "8080".into());
        let mut new2 = HashMap::new();
        new2.insert("server.port".into(), "9090".into());

        let delta2 = e.diff_flat(&old2, &new2, 1, 2, RegistryKind::Config);

        let merged = e.merge_deltas(vec![delta1, delta2]);
        assert_eq!(merged.change_count(), 2);
        assert_eq!(merged.max_impact, ChangeImpact::ColdRestart);
    }

    #[test]
    fn change_impact_ordering() {
        assert!(ChangeImpact::ColdRestart > ChangeImpact::WarmRestart);
        assert!(ChangeImpact::WarmRestart > ChangeImpact::HotReload);
        assert!(ChangeImpact::HotReload > ChangeImpact::NoOp);
    }
}
