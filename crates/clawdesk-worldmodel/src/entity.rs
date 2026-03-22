//! Entity types and state tracking for the world model.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Unique identifier for an entity in the world model.
pub type EntityId = String;

/// What kind of thing this entity represents.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EntityKind {
    /// A running service (database, API, CI pipeline).
    Service,
    /// A file or directory the agent has observed.
    File,
    /// A person (user, colleague, stakeholder).
    Person,
    /// A communication channel (Slack, Telegram, email).
    Channel,
    /// An environment variable, config, or system setting.
    Environment,
    /// A task, PR, issue, or work item.
    WorkItem,
    /// Custom entity type.
    Custom(String),
}

/// The current known state of an entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityState {
    /// Unique identifier.
    pub id: EntityId,
    /// What kind of entity this is.
    pub kind: EntityKind,
    /// Human-readable name/label.
    pub label: String,
    /// Freeform state snapshot (JSON blob).
    /// Examples: `{"status": "healthy", "latency_ms": 45}` for a service,
    /// `{"lines": 342, "last_modified": "2026-03-20"}` for a file.
    pub state: serde_json::Value,
    /// Confidence in the current state (0.0–1.0).
    pub confidence: f64,
    /// When this entity was last observed/updated.
    pub last_observed: DateTime<Utc>,
    /// When this entity was first added to the world model.
    pub created: DateTime<Utc>,
    /// How many times this entity has been observed.
    pub observation_count: u64,
}

impl EntityState {
    pub fn new(id: impl Into<String>, kind: EntityKind, label: impl Into<String>) -> Self {
        let now = Utc::now();
        Self {
            id: id.into(),
            kind,
            label: label.into(),
            state: serde_json::Value::Null,
            confidence: 1.0,
            last_observed: now,
            created: now,
            observation_count: 1,
        }
    }

    pub fn with_state(mut self, state: serde_json::Value) -> Self {
        self.state = state;
        self
    }

    /// How long since the last observation.
    pub fn staleness(&self) -> Duration {
        let elapsed = (Utc::now() - self.last_observed).num_seconds().max(0) as u64;
        Duration::from_secs(elapsed)
    }

    /// Whether this entity is stale (older than threshold).
    pub fn is_stale(&self, threshold: Duration) -> bool {
        self.staleness() >= threshold
    }

    /// Update confidence based on staleness. Confidence decays as
    /// observations age — a 1-hour-old fact is less trustworthy than
    /// a 1-minute-old one.
    pub fn decayed_confidence(&self, half_life: Duration) -> f64 {
        let staleness_secs = self.staleness().as_secs_f64();
        let hl_secs = half_life.as_secs_f64();
        if hl_secs <= 0.0 {
            return self.confidence;
        }
        self.confidence * (-staleness_secs * 2.0_f64.ln() / hl_secs).exp()
    }
}

/// A fact that's true for a bounded time period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemporalFact {
    /// What entity this fact is about.
    pub entity_id: EntityId,
    /// The fact itself.
    pub fact: String,
    /// When this fact became true.
    pub valid_from: DateTime<Utc>,
    /// When this fact expires (None = indefinite until contradicted).
    pub valid_until: Option<DateTime<Utc>>,
    /// Confidence in this fact.
    pub confidence: f64,
}

impl TemporalFact {
    /// Whether this fact is currently active.
    pub fn is_active(&self) -> bool {
        let now = Utc::now();
        now >= self.valid_from && self.valid_until.map_or(true, |until| now < until)
    }
}

/// A directed relation between two entities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Relation {
    pub from: EntityId,
    pub to: EntityId,
    pub label: String,
    pub confidence: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entity_staleness() {
        let e = EntityState::new("srv-1", EntityKind::Service, "API Server");
        assert!(e.staleness() < Duration::from_secs(1));
        assert!(!e.is_stale(Duration::from_secs(60)));
    }

    #[test]
    fn confidence_decays() {
        let mut e = EntityState::new("db", EntityKind::Service, "Database");
        // Artificially age the entity
        e.last_observed = Utc::now() - chrono::Duration::hours(2);
        let fresh_conf = 1.0;
        let decayed = e.decayed_confidence(Duration::from_secs(3600)); // 1hr half-life
        assert!(decayed < fresh_conf, "decayed={} should be < {}", decayed, fresh_conf);
        assert!(decayed > 0.2, "decayed={} should not be near zero after 2 half-lives", decayed);
    }

    #[test]
    fn temporal_fact_expiry() {
        let active = TemporalFact {
            entity_id: "ci".into(),
            fact: "build is green".into(),
            valid_from: Utc::now() - chrono::Duration::minutes(5),
            valid_until: Some(Utc::now() + chrono::Duration::hours(1)),
            confidence: 0.95,
        };
        assert!(active.is_active());

        let expired = TemporalFact {
            entity_id: "ci".into(),
            fact: "build is red".into(),
            valid_from: Utc::now() - chrono::Duration::hours(2),
            valid_until: Some(Utc::now() - chrono::Duration::hours(1)),
            confidence: 0.9,
        };
        assert!(!expired.is_active());
    }
}
