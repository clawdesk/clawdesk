//! Transparent Memory — User-Auditable Knowledge Base
//!
//! Surfaces all agent memory as an auditable, editable knowledge base.
//! "What I Know About You" panel with categories, source attribution,
//! edit/delete capability, and confidence scores.
//!
//! ## Confidence Model
//!
//! Bayesian update: `P(fact | evidence) = P(evidence | fact) × P(fact) / P(evidence)`
//! Temporal decay: `confidence(t) = c₀ × e^{-λt}` where `λ = ln(2)/τ_half`
//! - 90-day half-life for persistent facts
//! - 7-day half-life for preferences

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A single memory entry visible to the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryEntry {
    /// Unique entry identifier
    pub id: String,
    /// The fact or knowledge stored
    pub content: String,
    /// Category for grouping
    pub category: MemoryCategory,
    /// Confidence score (0.0 – 1.0)
    pub confidence: f64,
    /// When this was first learned (epoch seconds)
    pub created_at: u64,
    /// When this was last reinforced (epoch seconds)
    pub last_reinforced_at: u64,
    /// Source attribution
    pub source: MemorySource,
    /// Number of times this fact has been reinforced
    pub reinforcement_count: u32,
    /// Whether this entry has been user-verified
    pub user_verified: bool,
    /// Whether this entry has been user-edited
    pub user_edited: bool,
}

/// Memory categories for the "What I Know About You" panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCategory {
    PersonalFacts,
    Preferences,
    WorkContext,
    Relationships,
    Schedules,
    Skills,
    ProjectContext,
    Communication,
    Other,
}

impl MemoryCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::PersonalFacts => "Personal Facts",
            Self::Preferences => "Preferences",
            Self::WorkContext => "Work Context",
            Self::Relationships => "Relationships",
            Self::Schedules => "Schedules & Routines",
            Self::Skills => "Skills & Expertise",
            Self::ProjectContext => "Project Context",
            Self::Communication => "Communication Style",
            Self::Other => "Other",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::PersonalFacts => "👤",
            Self::Preferences => "⚙️",
            Self::WorkContext => "💼",
            Self::Relationships => "👥",
            Self::Schedules => "📅",
            Self::Skills => "🎯",
            Self::ProjectContext => "📁",
            Self::Communication => "💬",
            Self::Other => "📝",
        }
    }

    pub fn all() -> &'static [MemoryCategory] {
        &[
            Self::PersonalFacts, Self::Preferences, Self::WorkContext,
            Self::Relationships, Self::Schedules, Self::Skills,
            Self::ProjectContext, Self::Communication, Self::Other,
        ]
    }

    /// Half-life in seconds for temporal decay.
    ///
    /// Facts persist longer (90 days), preferences decay faster (7 days).
    pub fn half_life_secs(self) -> f64 {
        match self {
            Self::PersonalFacts => 90.0 * 86400.0,  // 90 days
            Self::Preferences => 7.0 * 86400.0,     // 7 days
            Self::WorkContext => 30.0 * 86400.0,     // 30 days
            Self::Relationships => 180.0 * 86400.0,  // 180 days
            Self::Schedules => 14.0 * 86400.0,       // 14 days
            Self::Skills => 365.0 * 86400.0,         // 1 year
            Self::ProjectContext => 30.0 * 86400.0,  // 30 days
            Self::Communication => 30.0 * 86400.0,   // 30 days
            Self::Other => 30.0 * 86400.0,           // 30 days
        }
    }
}

/// Source attribution for a memory entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySource {
    /// Which conversation/channel this came from
    pub channel: String,
    /// The conversation/session ID
    pub session_id: Option<String>,
    /// Date when learned (human-readable)
    pub date: String,
    /// Brief excerpt of the source message
    pub excerpt: Option<String>,
}

/// User action on a memory entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum MemoryAction {
    /// Edit the content of an entry
    Edit {
        entry_id: String,
        new_content: String,
    },
    /// Delete an entry
    Delete {
        entry_id: String,
    },
    /// Verify an entry (user confirms it's correct)
    Verify {
        entry_id: String,
    },
    /// Mark an entry as incorrect
    MarkIncorrect {
        entry_id: String,
    },
}

/// Complete memory knowledge base for the UI panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryKnowledgeBase {
    /// All memory entries
    pub entries: Vec<MemoryEntry>,
    /// Entries grouped by category
    pub by_category: HashMap<String, Vec<String>>, // category -> entry IDs
    /// Total entry count
    pub total_count: usize,
    /// Number of stale entries (below confidence threshold)
    pub stale_count: usize,
    /// Last updated timestamp
    pub last_updated: u64,
}

impl MemoryKnowledgeBase {
    /// Build from a flat list of entries.
    pub fn from_entries(entries: Vec<MemoryEntry>) -> Self {
        let mut by_category: HashMap<String, Vec<String>> = HashMap::new();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut stale_count = 0;

        for entry in &entries {
            let cat_key = format!("{:?}", entry.category);
            by_category.entry(cat_key).or_default().push(entry.id.clone());

            let decayed_confidence = compute_decayed_confidence(
                entry.confidence,
                entry.last_reinforced_at,
                now,
                entry.category,
            );
            if decayed_confidence < 0.3 {
                stale_count += 1;
            }
        }

        let total_count = entries.len();

        Self {
            entries,
            by_category,
            total_count,
            stale_count,
            last_updated: now,
        }
    }
}

/// Compute decayed confidence using exponential decay.
///
/// `confidence(t) = c₀ × e^{-λt}` where `λ = ln(2)/τ_half`
pub fn compute_decayed_confidence(
    initial_confidence: f64,
    last_reinforced_at: u64,
    now: u64,
    category: MemoryCategory,
) -> f64 {
    if now <= last_reinforced_at {
        return initial_confidence;
    }

    let elapsed_secs = (now - last_reinforced_at) as f64;
    let half_life = category.half_life_secs();
    let lambda = (2.0_f64).ln() / half_life;

    let decayed = initial_confidence * (-lambda * elapsed_secs).exp();
    decayed.max(0.0).min(1.0)
}

/// Bayesian confidence update when a fact is reinforced.
///
/// `P(fact | new_evidence) = P(evidence | fact) × P(fact) / P(evidence)`
/// Simplified: `c_new = 1 - (1 - c_old) × (1 - evidence_strength)`
pub fn bayesian_update(current_confidence: f64, evidence_strength: f64) -> f64 {
    let updated = 1.0 - (1.0 - current_confidence) * (1.0 - evidence_strength);
    updated.max(0.0).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_decays_over_time() {
        let now = 1000000;
        let one_week_ago = now - 7 * 86400;

        let confidence = compute_decayed_confidence(
            1.0, one_week_ago, now, MemoryCategory::Preferences,
        );
        // Preferences have 7-day half-life, so after 7 days confidence = 0.5
        assert!((confidence - 0.5).abs() < 0.01);
    }

    #[test]
    fn confidence_stable_for_facts() {
        let now = 1000000;
        let one_week_ago = now - 7 * 86400;

        let confidence = compute_decayed_confidence(
            1.0, one_week_ago, now, MemoryCategory::PersonalFacts,
        );
        // Facts have 90-day half-life, so 7 days barely affects them
        assert!(confidence > 0.9);
    }

    #[test]
    fn bayesian_update_increases_confidence() {
        let initial = 0.5;
        let updated = bayesian_update(initial, 0.8);
        assert!(updated > initial);
        assert!(updated < 1.0);
    }

    #[test]
    fn bayesian_update_bounded() {
        assert_eq!(bayesian_update(1.0, 1.0), 1.0);
        assert_eq!(bayesian_update(0.0, 0.0), 0.0);
    }

    #[test]
    fn knowledge_base_groups_by_category() {
        let entries = vec![
            MemoryEntry {
                id: "1".into(), content: "Likes dark mode".into(),
                category: MemoryCategory::Preferences, confidence: 0.9,
                created_at: 0, last_reinforced_at: 0,
                source: MemorySource {
                    channel: "chat".into(), session_id: None,
                    date: "2025-01-01".into(), excerpt: None,
                },
                reinforcement_count: 3, user_verified: false, user_edited: false,
            },
            MemoryEntry {
                id: "2".into(), content: "Name: Alice".into(),
                category: MemoryCategory::PersonalFacts, confidence: 1.0,
                created_at: 0, last_reinforced_at: 0,
                source: MemorySource {
                    channel: "chat".into(), session_id: None,
                    date: "2025-01-01".into(), excerpt: None,
                },
                reinforcement_count: 1, user_verified: true, user_edited: false,
            },
        ];

        let kb = MemoryKnowledgeBase::from_entries(entries);
        assert_eq!(kb.total_count, 2);
    }
}
