//! Skill registry — O(1) lookup, lifecycle management, hot-reload.
//!
//! Uses `im::HashMap` (HAMT) for O(log₃₂ n) structural-sharing clones.
//! The ArcSwap COW pattern clones the registry before mutation; switching
//! from `FxHashMap` (O(n) deep copy) to `im::HashMap` (O(log₃₂ n) node
//! sharing) makes hot-reload swaps ~100× faster for a 200-skill registry.
//!
//! Read-path lookups are O(log₃₂ n) ≈ O(1) for practical sizes (a 1000-
//! skill registry needs at most 2 hops).

use crate::definition::{Skill, SkillId, SkillSource, SkillState};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Entry in the skill registry — tracks state and metadata.
#[derive(Debug, Clone)]
pub struct SkillEntry {
    pub skill: Arc<Skill>,
    pub source: SkillSource,
    pub state: SkillState,
    pub load_time_ms: Option<u64>,
    pub error: Option<String>,
}

/// In-memory skill registry backed by persistent HAMT (`im::HashMap`).
///
/// Clone is O(log₃₂ n) with structural sharing — ideal for the ArcSwap
/// COW pattern where "clone → mutate → swap" is the hot-reload path.
/// Read-path lookups are O(log₃₂ n) ≈ O(1) for practical skill counts.
#[derive(Clone, Debug)]
pub struct SkillRegistry {
    entries: im::HashMap<SkillId, SkillEntry>,
    /// Index: tag → skill IDs (for filtered queries).
    tag_index: im::HashMap<String, Vec<SkillId>>,
}

impl SkillRegistry {
    pub fn new() -> Self {
        Self {
            entries: im::HashMap::new(),
            tag_index: im::HashMap::new(),
        }
    }

    /// Register a skill. Overwrites if the ID already exists (hot-reload).
    pub fn register(&mut self, skill: Skill, source: SkillSource) {
        let id = skill.manifest.id.clone();
        info!(skill = %id, "registering skill");

        // Update tag index
        for tag in &skill.manifest.tags {
            self.tag_index
                .entry(tag.clone())
                .or_default()
                .push(id.clone());
        }

        self.entries.insert(
            id,
            SkillEntry {
                skill: Arc::new(skill),
                source,
                state: SkillState::Loaded,
                load_time_ms: None,
                error: None,
            },
        );
    }

    /// Activate a skill (set state to Active).
    pub fn activate(&mut self, id: &SkillId) -> Result<(), String> {
        let entry = self
            .entries
            .get_mut(id)
            .ok_or_else(|| format!("skill not found: {}", id))?;

        match entry.state {
            SkillState::Loaded | SkillState::Resolved | SkillState::Disabled => {
                entry.state = SkillState::Active;
                debug!(skill = %id, "skill activated");
                Ok(())
            }
            SkillState::Active => Ok(()), // idempotent
            SkillState::Failed => Err(format!("cannot activate failed skill: {}", id)),
            SkillState::Discovered => {
                Err(format!("skill {} not yet loaded", id))
            }
        }
    }

    /// Deactivate a skill.
    pub fn deactivate(&mut self, id: &SkillId) -> Result<(), String> {
        let entry = self
            .entries
            .get_mut(id)
            .ok_or_else(|| format!("skill not found: {}", id))?;
        entry.state = SkillState::Disabled;
        debug!(skill = %id, "skill deactivated");
        Ok(())
    }

    /// Mark a skill as failed.
    pub fn mark_failed(&mut self, id: &SkillId, error: String) {
        if let Some(entry) = self.entries.get_mut(id) {
            entry.state = SkillState::Failed;
            entry.error = Some(error.clone());
            warn!(skill = %id, %error, "skill failed");
        }
    }

    /// Get a skill by ID.
    pub fn get(&self, id: &SkillId) -> Option<&SkillEntry> {
        self.entries.get(id)
    }

    /// Get all active skills, sorted by priority_weight descending.
    /// This is the snapshot used by the agent runner for prompt assembly.
    pub fn active_skills(&self) -> Vec<Arc<Skill>> {
        let mut skills: Vec<Arc<Skill>> = self
            .entries
            .values()
            .filter(|e| e.state == SkillState::Active)
            .map(|e| Arc::clone(&e.skill))
            .collect();

        // Sort descending by priority_weight — O(k log k) where k = active skills.
        skills.sort_by(|a, b| {
            b.manifest
                .priority_weight
                .partial_cmp(&a.manifest.priority_weight)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        skills
    }

    /// Get skills matching a trigger condition.
    pub fn skills_for_command(&self, command: &str) -> Vec<Arc<Skill>> {
        self.entries
            .values()
            .filter(|e| {
                e.state == SkillState::Active
                    && e.skill.manifest.triggers.iter().any(|t| match t {
                        crate::definition::SkillTrigger::Command { command: cmd } => {
                            cmd == command
                        }
                        crate::definition::SkillTrigger::Always => true,
                        _ => false,
                    })
            })
            .map(|e| Arc::clone(&e.skill))
            .collect()
    }

    /// Get skills by tag.
    pub fn skills_by_tag(&self, tag: &str) -> Vec<Arc<Skill>> {
        self.tag_index
            .get(tag)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| {
                        self.entries
                            .get(id)
                            .filter(|e| e.state == SkillState::Active)
                            .map(|e| Arc::clone(&e.skill))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// List all skill manifests with state info.
    pub fn list(&self) -> Vec<SkillInfo> {
        self.entries
            .values()
            .map(|e| SkillInfo {
                id: e.skill.manifest.id.clone(),
                display_name: e.skill.manifest.display_name.clone(),
                version: e.skill.manifest.version.clone(),
                state: e.state,
                source: e.source.clone(),
                estimated_tokens: e.skill.token_cost(),
                priority_weight: e.skill.manifest.priority_weight,
                error: e.error.clone(),
            })
            .collect()
    }

    /// Total number of registered skills.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Remove a skill entirely.
    pub fn remove(&mut self, id: &SkillId) -> Option<SkillEntry> {
        // Clean up tag index
        if let Some(entry) = self.entries.get(id) {
            for tag in &entry.skill.manifest.tags {
                if let Some(ids) = self.tag_index.get_mut(tag) {
                    ids.retain(|i| i != id);
                }
            }
        }
        self.entries.remove(id)
    }
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary info for a skill (for listing/display/admin API).
#[derive(Debug, Clone, serde::Serialize)]
pub struct SkillInfo {
    pub id: SkillId,
    pub display_name: String,
    pub version: String,
    pub state: SkillState,
    pub source: SkillSource,
    pub estimated_tokens: usize,
    pub priority_weight: f64,
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::*;

    fn make_skill(id: &str, priority: f64, tags: Vec<&str>) -> Skill {
        Skill {
            manifest: SkillManifest {
                id: SkillId::from(id),
                display_name: id.to_string(),
                description: format!("Test skill: {}", id),
                version: "0.1.0".into(),
                author: None,
                dependencies: vec![],
                required_tools: vec![],
                parameters: vec![],
                triggers: vec![SkillTrigger::Always],
                estimated_tokens: 100,
                priority_weight: priority,
                tags: tags.into_iter().map(String::from).collect(),
                signature: None,
                publisher_key: None,
                content_hash: None,
                schema_version: 1,
            },
            prompt_fragment: format!("You have the {} capability.", id),
            provided_tools: vec![],
            parameter_values: serde_json::Value::Null,
            source_path: None,
        }
    }

    #[test]
    fn active_skills_sorted_by_priority() {
        let mut reg = SkillRegistry::new();
        reg.register(make_skill("low", 1.0, vec![]), SkillSource::Builtin);
        reg.register(make_skill("high", 10.0, vec![]), SkillSource::Builtin);
        reg.register(make_skill("mid", 5.0, vec![]), SkillSource::Builtin);

        reg.activate(&SkillId::from("low")).unwrap();
        reg.activate(&SkillId::from("high")).unwrap();
        reg.activate(&SkillId::from("mid")).unwrap();

        let active = reg.active_skills();
        assert_eq!(active.len(), 3);
        assert_eq!(active[0].manifest.id.as_str(), "high");
        assert_eq!(active[1].manifest.id.as_str(), "mid");
        assert_eq!(active[2].manifest.id.as_str(), "low");
    }

    #[test]
    fn tag_index_works() {
        let mut reg = SkillRegistry::new();
        reg.register(make_skill("a", 1.0, vec!["search"]), SkillSource::Builtin);
        reg.register(
            make_skill("b", 2.0, vec!["search", "web"]),
            SkillSource::Builtin,
        );
        reg.activate(&SkillId::from("a")).unwrap();
        reg.activate(&SkillId::from("b")).unwrap();

        let search_skills = reg.skills_by_tag("search");
        assert_eq!(search_skills.len(), 2);

        let web_skills = reg.skills_by_tag("web");
        assert_eq!(web_skills.len(), 1);
    }
}
