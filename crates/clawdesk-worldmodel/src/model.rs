//! The world model — persistent environment state tracker.

use crate::entity::{EntityId, EntityKind, EntityState, Relation, TemporalFact};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, info, warn};

/// A perception event — something the agent observed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Perception {
    /// Source of the perception (tool name, channel, browser).
    pub source: String,
    /// Entity being observed (created if new).
    pub entity_id: EntityId,
    /// Entity kind (used only for creation).
    pub entity_kind: EntityKind,
    /// Human-readable label.
    pub label: String,
    /// The observed state (JSON blob).
    pub state: serde_json::Value,
    /// Confidence in this observation.
    pub confidence: f64,
    /// Optional temporal bounds for the fact.
    pub valid_until: Option<DateTime<Utc>>,
}

/// What changed after processing a perception.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldDelta {
    pub entity_id: EntityId,
    pub is_new: bool,
    pub state_changed: bool,
    pub previous_state: Option<serde_json::Value>,
    pub contradictions: Vec<Contradiction>,
}

/// A predicted future state for an entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredictedState {
    pub entity_id: EntityId,
    pub predicted_state: serde_json::Value,
    pub confidence: f64,
    pub horizon: Duration,
    pub basis: String,
}

/// An internal consistency issue.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contradiction {
    pub entity_id: EntityId,
    pub fact_a: String,
    pub fact_b: String,
    pub severity: f64,
}

/// The world model — tracks entities, relations, and temporal facts.
pub struct WorldModel {
    entities: HashMap<EntityId, EntityState>,
    relations: Vec<Relation>,
    temporal_facts: Vec<TemporalFact>,
    /// Default confidence decay half-life.
    confidence_half_life: Duration,
    /// Maximum entities before eviction of lowest-confidence.
    max_entities: usize,
}

impl WorldModel {
    pub fn new() -> Self {
        Self {
            entities: HashMap::new(),
            relations: Vec::new(),
            temporal_facts: Vec::new(),
            confidence_half_life: Duration::from_secs(3600), // 1 hour
            max_entities: 500,
        }
    }

    /// Observe something in the environment. Updates or creates an entity.
    pub fn observe(&mut self, perception: &Perception) -> WorldDelta {
        let previous = self.entities.get(&perception.entity_id).map(|e| e.state.clone());
        let is_new = !self.entities.contains_key(&perception.entity_id);

        let state_changed = previous.as_ref().map_or(true, |prev| prev != &perception.state);

        if let Some(entity) = self.entities.get_mut(&perception.entity_id) {
            // Update existing entity
            let old_state = entity.state.clone();
            entity.state = perception.state.clone();
            entity.confidence = perception.confidence;
            entity.last_observed = Utc::now();
            entity.observation_count += 1;

            debug!(
                entity = %perception.entity_id,
                changed = state_changed,
                observations = entity.observation_count,
                "world model: updated entity"
            );
        } else {
            // Create new entity
            let entity = EntityState::new(
                perception.entity_id.clone(),
                perception.entity_kind.clone(),
                perception.label.clone(),
            ).with_state(perception.state.clone());

            self.entities.insert(perception.entity_id.clone(), entity);
            info!(entity = %perception.entity_id, kind = ?perception.entity_kind, "world model: new entity");
        }

        // Add temporal fact if bounded
        if let Some(valid_until) = perception.valid_until {
            self.temporal_facts.push(TemporalFact {
                entity_id: perception.entity_id.clone(),
                fact: serde_json::to_string(&perception.state).unwrap_or_default(),
                valid_from: Utc::now(),
                valid_until: Some(valid_until),
                confidence: perception.confidence,
            });
        }

        // Check for contradictions
        let contradictions = self.check_contradictions(&perception.entity_id);

        // Evict if over capacity
        if self.entities.len() > self.max_entities {
            self.evict_lowest_confidence();
        }

        WorldDelta {
            entity_id: perception.entity_id.clone(),
            is_new,
            state_changed,
            previous_state: previous,
            contradictions,
        }
    }

    /// Get an entity by ID.
    pub fn get(&self, entity_id: &str) -> Option<&EntityState> {
        self.entities.get(entity_id)
    }

    /// Get an entity's decayed confidence (accounts for staleness).
    pub fn entity_confidence(&self, entity_id: &str) -> Option<f64> {
        self.entities.get(entity_id)
            .map(|e| e.decayed_confidence(self.confidence_half_life))
    }

    /// Find all entities that are stale (older than threshold).
    pub fn stale_entities(&self, threshold: Duration) -> Vec<&EntityState> {
        self.entities.values()
            .filter(|e| e.is_stale(threshold))
            .collect()
    }

    /// Find all entities of a specific kind.
    pub fn entities_by_kind(&self, kind: &EntityKind) -> Vec<&EntityState> {
        self.entities.values()
            .filter(|e| &e.kind == kind)
            .collect()
    }

    /// Predict an entity's state at a future time horizon.
    /// For now, simple extrapolation: if state hasn't changed, predict same.
    /// If confidence is decaying, project the decay forward.
    pub fn predict(&self, entity_id: &str, horizon: Duration) -> Option<PredictedState> {
        let entity = self.entities.get(entity_id)?;
        let future_staleness = entity.staleness() + horizon;
        let hl = self.confidence_half_life.as_secs_f64();
        let projected_conf = if hl > 0.0 {
            entity.confidence * (-future_staleness.as_secs_f64() * 2.0_f64.ln() / hl).exp()
        } else {
            entity.confidence
        };

        Some(PredictedState {
            entity_id: entity_id.to_string(),
            predicted_state: entity.state.clone(),
            confidence: projected_conf,
            horizon,
            basis: format!(
                "Last observed {} ago, {} observations total",
                humanize_duration(entity.staleness()),
                entity.observation_count,
            ),
        })
    }

    /// Add a relation between two entities.
    pub fn add_relation(&mut self, from: &str, to: &str, label: &str, confidence: f64) {
        self.relations.push(Relation {
            from: from.to_string(),
            to: to.to_string(),
            label: label.to_string(),
            confidence,
        });
    }

    /// Get all relations for an entity (both directions).
    pub fn relations_for(&self, entity_id: &str) -> Vec<&Relation> {
        self.relations.iter()
            .filter(|r| r.from == entity_id || r.to == entity_id)
            .collect()
    }

    /// Get all active temporal facts for an entity.
    pub fn active_facts(&self, entity_id: &str) -> Vec<&TemporalFact> {
        self.temporal_facts.iter()
            .filter(|f| f.entity_id == entity_id && f.is_active())
            .collect()
    }

    /// Check for contradictions in an entity's facts.
    fn check_contradictions(&self, entity_id: &str) -> Vec<Contradiction> {
        let facts: Vec<&TemporalFact> = self.active_facts(entity_id);
        let mut contradictions = Vec::new();

        // Compare all pairs of active facts for the same entity.
        // A contradiction occurs when two facts assert different states
        // with overlapping validity and high confidence.
        for i in 0..facts.len() {
            for j in (i + 1)..facts.len() {
                if facts[i].fact != facts[j].fact
                    && facts[i].confidence > 0.7
                    && facts[j].confidence > 0.7
                {
                    contradictions.push(Contradiction {
                        entity_id: entity_id.to_string(),
                        fact_a: facts[i].fact.clone(),
                        fact_b: facts[j].fact.clone(),
                        severity: (facts[i].confidence + facts[j].confidence) / 2.0,
                    });
                }
            }
        }
        contradictions
    }

    /// Find all contradictions across the entire world model.
    pub fn all_contradictions(&self) -> Vec<Contradiction> {
        let entity_ids: Vec<String> = self.entities.keys().cloned().collect();
        entity_ids.iter()
            .flat_map(|id| self.check_contradictions(id))
            .collect()
    }

    /// Evict the entity with the lowest decayed confidence.
    fn evict_lowest_confidence(&mut self) {
        let hl = self.confidence_half_life;
        if let Some((id, _)) = self.entities.iter()
            .min_by(|(_, a), (_, b)| {
                a.decayed_confidence(hl)
                    .partial_cmp(&b.decayed_confidence(hl))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        {
            let id = id.clone();
            self.entities.remove(&id);
            self.relations.retain(|r| r.from != id && r.to != id);
            debug!(entity = %id, "world model: evicted lowest-confidence entity");
        }
    }

    /// GC expired temporal facts.
    pub fn gc_expired_facts(&mut self) {
        let before = self.temporal_facts.len();
        self.temporal_facts.retain(|f| f.is_active());
        let removed = before - self.temporal_facts.len();
        if removed > 0 {
            debug!(removed, "world model: GC'd expired temporal facts");
        }
    }

    /// Generate a summary of the current world state for prompt injection.
    pub fn to_context_summary(&self, max_entities: usize) -> String {
        let hl = self.confidence_half_life;
        let mut ranked: Vec<(&EntityState, f64)> = self.entities.values()
            .map(|e| (e, e.decayed_confidence(hl)))
            .collect();
        ranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ranked.truncate(max_entities);

        if ranked.is_empty() {
            return String::new();
        }

        let mut lines = vec!["<world_state>".to_string()];
        for (entity, conf) in &ranked {
            let state_str = if entity.state.is_null() {
                "unknown".to_string()
            } else {
                serde_json::to_string(&entity.state).unwrap_or_else(|_| "?".into())
            };
            lines.push(format!(
                "  {} ({:?}): {} [confidence: {:.0}%, age: {}]",
                entity.label,
                entity.kind,
                state_str,
                conf * 100.0,
                humanize_duration(entity.staleness()),
            ));
        }
        lines.push("</world_state>".to_string());
        lines.join("\n")
    }

    pub fn entity_count(&self) -> usize {
        self.entities.len()
    }

    pub fn fact_count(&self) -> usize {
        self.temporal_facts.len()
    }

    pub fn relation_count(&self) -> usize {
        self.relations.len()
    }
}

impl Default for WorldModel {
    fn default() -> Self {
        Self::new()
    }
}

fn humanize_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 { format!("{}s", secs) }
    else if secs < 3600 { format!("{}m", secs / 60) }
    else if secs < 86400 { format!("{}h", secs / 3600) }
    else { format!("{}d", secs / 86400) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service_perception(id: &str, status: &str) -> Perception {
        Perception {
            source: "health_check".into(),
            entity_id: id.into(),
            entity_kind: EntityKind::Service,
            label: id.into(),
            state: serde_json::json!({"status": status}),
            confidence: 0.95,
            valid_until: None,
        }
    }

    #[test]
    fn observe_creates_and_updates() {
        let mut wm = WorldModel::new();
        let delta = wm.observe(&service_perception("api", "healthy"));
        assert!(delta.is_new);
        assert!(delta.state_changed);
        assert_eq!(wm.entity_count(), 1);

        let delta2 = wm.observe(&service_perception("api", "degraded"));
        assert!(!delta2.is_new);
        assert!(delta2.state_changed);
        assert_eq!(delta2.previous_state, Some(serde_json::json!({"status": "healthy"})));
    }

    #[test]
    fn stale_entities_detection() {
        let mut wm = WorldModel::new();
        wm.observe(&service_perception("api", "healthy"));
        // Artificially age the entity
        if let Some(e) = wm.entities.get_mut("api") {
            e.last_observed = Utc::now() - chrono::Duration::hours(2);
        }
        let stale = wm.stale_entities(Duration::from_secs(3600));
        assert_eq!(stale.len(), 1);
    }

    #[test]
    fn context_summary_generation() {
        let mut wm = WorldModel::new();
        wm.observe(&service_perception("api", "healthy"));
        wm.observe(&service_perception("db", "slow"));
        let summary = wm.to_context_summary(10);
        assert!(summary.contains("world_state"));
        assert!(summary.contains("api"));
        assert!(summary.contains("db"));
    }

    #[test]
    fn prediction_with_decay() {
        let mut wm = WorldModel::new();
        wm.observe(&service_perception("api", "healthy"));
        let pred = wm.predict("api", Duration::from_secs(7200)).unwrap();
        assert!(pred.confidence < 0.95, "confidence should decay over 2h horizon");
    }

    #[test]
    fn relations_work() {
        let mut wm = WorldModel::new();
        wm.observe(&service_perception("api", "healthy"));
        wm.observe(&service_perception("db", "healthy"));
        wm.add_relation("api", "db", "depends_on", 1.0);
        let rels = wm.relations_for("api");
        assert_eq!(rels.len(), 1);
        assert_eq!(rels[0].label, "depends_on");
    }

    #[test]
    fn entities_by_kind() {
        let mut wm = WorldModel::new();
        wm.observe(&service_perception("api", "healthy"));
        wm.observe(&Perception {
            source: "editor".into(),
            entity_id: "main.rs".into(),
            entity_kind: EntityKind::File,
            label: "main.rs".into(),
            state: serde_json::json!({"lines": 200}),
            confidence: 1.0,
            valid_until: None,
        });
        assert_eq!(wm.entities_by_kind(&EntityKind::Service).len(), 1);
        assert_eq!(wm.entities_by_kind(&EntityKind::File).len(), 1);
    }
}
