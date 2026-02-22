//! Per-agent skill filtering.
//!
//! ## Per-Agent Filtering (P2)
//!
//! Agents can declare which skills they want via a skill filter. This enables:
//! - Focused agents that only use specific skills (e.g., "coding-agent" only
//!   wants coding/git/terminal skills)
//! - Agents that opt out of skills entirely
//! - The default behavior where all eligible skills are available
//!
//! ## Semantics
//!
//! - `None` → all eligible skills (default)
//! - `Some([])` → no skills (skill-less agent)
//! - `Some(["a", "b"])` → only skills "a" and "b" (if eligible)
//!
//! ## Integration
//!
//! The filter is applied between eligibility checking and token-budgeted
//! selection:
//!
//! ```text
//! All skills → Eligibility → Agent filter → SkillSelector → Prompt
//! ```

use crate::definition::{Skill, SkillId};
use std::collections::HashSet;
use std::sync::Arc;
use tracing::debug;

/// Agent skill filter configuration.
#[derive(Debug, Clone)]
pub enum SkillFilter {
    /// All eligible skills are available (default for most agents).
    All,
    /// No skills — agent operates without skill augmentation.
    None,
    /// Only the listed skills are available.
    Allow(HashSet<String>),
    /// All skills except the listed ones.
    Deny(HashSet<String>),
}

impl SkillFilter {
    /// Create an allow-list filter from skill names.
    pub fn allow(skills: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::Allow(skills.into_iter().map(Into::into).collect())
    }

    /// Create a deny-list filter from skill names.
    pub fn deny(skills: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::Deny(skills.into_iter().map(Into::into).collect())
    }

    /// Parse from an optional Vec<String> (the common config representation).
    ///
    /// - `None` → `All`
    /// - `Some([])` → `None`
    /// - `Some(["a", "b"])` → `Allow({"a", "b"})`
    pub fn from_config(config: Option<Vec<String>>) -> Self {
        match config {
            Option::None => Self::All,
            Some(v) if v.is_empty() => Self::None,
            Some(v) => Self::allow(v),
        }
    }

    /// Check if a skill is allowed by this filter.
    pub fn allows(&self, skill_id: &str) -> bool {
        match self {
            Self::All => true,
            Self::None => false,
            Self::Allow(set) => set.contains(skill_id),
            Self::Deny(set) => !set.contains(skill_id),
        }
    }

    /// Whether this filter allows any skills at all.
    pub fn allows_any(&self) -> bool {
        !matches!(self, Self::None)
    }

    /// A fingerprint for cache invalidation.
    pub fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        match self {
            Self::All => 0u8.hash(&mut hasher),
            Self::None => 1u8.hash(&mut hasher),
            Self::Allow(set) => {
                2u8.hash(&mut hasher);
                let mut sorted: Vec<_> = set.iter().collect();
                sorted.sort();
                for s in sorted {
                    s.hash(&mut hasher);
                }
            }
            Self::Deny(set) => {
                3u8.hash(&mut hasher);
                let mut sorted: Vec<_> = set.iter().collect();
                sorted.sort();
                for s in sorted {
                    s.hash(&mut hasher);
                }
            }
        }
        hasher.finish()
    }
}

impl Default for SkillFilter {
    fn default() -> Self {
        Self::All
    }
}

impl std::fmt::Display for SkillFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::All => write!(f, "all"),
            Self::None => write!(f, "none"),
            Self::Allow(set) => {
                let mut sorted: Vec<&str> = set.iter().map(|s| s.as_str()).collect();
                sorted.sort();
                write!(f, "allow({})", sorted.join(", "))
            }
            Self::Deny(set) => {
                let mut sorted: Vec<&str> = set.iter().map(|s| s.as_str()).collect();
                sorted.sort();
                write!(f, "deny({})", sorted.join(", "))
            }
        }
    }
}

/// Apply an agent's skill filter to a candidate set.
///
/// Returns only the skills that pass the filter, preserving order.
pub fn apply_filter(candidates: &[Arc<Skill>], filter: &SkillFilter) -> Vec<Arc<Skill>> {
    if matches!(filter, SkillFilter::All) {
        return candidates.to_vec();
    }

    if matches!(filter, SkillFilter::None) {
        return vec![];
    }

    let result: Vec<Arc<Skill>> = candidates
        .iter()
        .filter(|s| filter.allows(s.manifest.id.as_str()))
        .cloned()
        .collect();

    debug!(
        before = candidates.len(),
        after = result.len(),
        filter = %filter,
        "applied agent skill filter"
    );

    result
}

/// Filtered selection — combines agent filter with token-budgeted selection.
///
/// This is the high-level entry point:
/// 1. Apply agent filter
/// 2. Run SkillSelector::select on the filtered candidates
pub fn filtered_select(
    candidates: &[Arc<Skill>],
    filter: &SkillFilter,
    budget: usize,
) -> crate::selector::SelectionResult {
    let filtered = apply_filter(candidates, filter);
    crate::selector::SkillSelector::select(&filtered, budget)
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::*;

    fn make_skill(id: &str) -> Arc<Skill> {
        Arc::new(Skill {
            manifest: SkillManifest {
                id: SkillId::from(id),
                display_name: id.to_string(),
                description: format!("Skill: {}", id),
                version: "0.1.0".into(),
                author: None,
                dependencies: vec![],
                required_tools: vec![],
                parameters: vec![],
                triggers: vec![SkillTrigger::Always],
                estimated_tokens: 100,
                priority_weight: 1.0,
                tags: vec![],
                signature: None,
                publisher_key: None,
                content_hash: None,
                schema_version: 1,
            },
            prompt_fragment: format!("I am {}", id),
            provided_tools: vec![],
            parameter_values: serde_json::Value::Null,
            source_path: None,
        })
    }

    #[test]
    fn all_filter_passes_everything() {
        let skills = vec![make_skill("a"), make_skill("b"), make_skill("c")];
        let result = apply_filter(&skills, &SkillFilter::All);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn none_filter_blocks_everything() {
        let skills = vec![make_skill("a"), make_skill("b")];
        let result = apply_filter(&skills, &SkillFilter::None);
        assert!(result.is_empty());
    }

    #[test]
    fn allow_filter_includes_only_listed() {
        let skills = vec![make_skill("a"), make_skill("b"), make_skill("c")];
        let filter = SkillFilter::allow(["a", "c"]);
        let result = apply_filter(&skills, &filter);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].manifest.id.as_str(), "a");
        assert_eq!(result[1].manifest.id.as_str(), "c");
    }

    #[test]
    fn deny_filter_excludes_listed() {
        let skills = vec![make_skill("a"), make_skill("b"), make_skill("c")];
        let filter = SkillFilter::deny(["b"]);
        let result = apply_filter(&skills, &filter);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].manifest.id.as_str(), "a");
        assert_eq!(result[1].manifest.id.as_str(), "c");
    }

    #[test]
    fn from_config_none_is_all() {
        let filter = SkillFilter::from_config(None);
        assert!(matches!(filter, SkillFilter::All));
    }

    #[test]
    fn from_config_empty_is_none() {
        let filter = SkillFilter::from_config(Some(vec![]));
        assert!(matches!(filter, SkillFilter::None));
    }

    #[test]
    fn from_config_with_items_is_allow() {
        let filter = SkillFilter::from_config(Some(vec!["x".to_string(), "y".to_string()]));
        assert!(filter.allows("x"));
        assert!(filter.allows("y"));
        assert!(!filter.allows("z"));
    }

    #[test]
    fn allows_any() {
        assert!(SkillFilter::All.allows_any());
        assert!(!SkillFilter::None.allows_any());
        assert!(SkillFilter::allow(["a"]).allows_any());
        assert!(SkillFilter::deny(["a"]).allows_any());
    }

    #[test]
    fn fingerprint_stable() {
        let f1 = SkillFilter::allow(["a", "b"]);
        let f2 = SkillFilter::allow(["a", "b"]);
        assert_eq!(f1.fingerprint(), f2.fingerprint());
    }

    #[test]
    fn fingerprint_differs_for_different_filters() {
        let f1 = SkillFilter::allow(["a"]);
        let f2 = SkillFilter::allow(["b"]);
        assert_ne!(f1.fingerprint(), f2.fingerprint());

        let f3 = SkillFilter::All;
        let f4 = SkillFilter::None;
        assert_ne!(f3.fingerprint(), f4.fingerprint());
    }

    #[test]
    fn display() {
        assert_eq!(SkillFilter::All.to_string(), "all");
        assert_eq!(SkillFilter::None.to_string(), "none");
        let f = SkillFilter::allow(["b", "a"]);
        assert_eq!(f.to_string(), "allow(a, b)");
        let f = SkillFilter::deny(["z"]);
        assert_eq!(f.to_string(), "deny(z)");
    }

    #[test]
    fn filtered_select_respects_filter() {
        let skills = vec![
            make_skill("allowed"),
            make_skill("blocked"),
        ];
        let filter = SkillFilter::allow(["allowed"]);
        let result = filtered_select(&skills, &filter, 10000);
        assert_eq!(result.selected.len(), 1);
        assert_eq!(result.selected[0].skill.manifest.id.as_str(), "allowed");
    }
}
