//! User predictor — the top-level coordinator for temporal prediction.

use crate::pattern::{InteractionRecord, TemporalPattern, TimeSlot};
use chrono::{DateTime, Datelike, Timelike, Utc, Weekday};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info};

/// Configuration for the user predictor.
#[derive(Debug, Clone)]
pub struct PredictorConfig {
    /// Minimum frequency before a pattern is considered established.
    pub min_frequency: u32,
    /// Minimum confidence to trigger a prediction.
    pub min_confidence: f64,
    /// Maximum patterns to track per user.
    pub max_patterns: usize,
    /// Time window size for slot matching (hours).
    pub slot_window_hours: u32,
}

impl Default for PredictorConfig {
    fn default() -> Self {
        Self {
            min_frequency: 3,
            min_confidence: 0.5,
            max_patterns: 50,
            slot_window_hours: 2,
        }
    }
}

/// A predicted need — something the user will probably want.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredictedNeed {
    /// What we predict the user will need.
    pub description: String,
    /// Keywords for the predicted action.
    pub keywords: Vec<String>,
    /// Confidence in this prediction.
    pub confidence: f64,
    /// Which pattern triggered this prediction.
    pub pattern_id: String,
    /// The time slot this prediction applies to.
    pub time_slot: TimeSlot,
}

/// A pre-prepared action the agent could have ready.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedAction {
    /// What to prepare.
    pub description: String,
    /// Suggested tool calls to prepare.
    pub suggested_tools: Vec<String>,
    /// When to have this ready by.
    pub ready_by: TimeSlot,
    /// Source pattern ID.
    pub pattern_id: String,
}

/// Per-user temporal predictor.
pub struct UserPredictor {
    config: PredictorConfig,
    /// All learned patterns for this user.
    patterns: Vec<TemporalPattern>,
    /// Interaction history (for pattern learning).
    history: Vec<InteractionRecord>,
    /// Maximum history entries to retain.
    max_history: usize,
    /// Pattern ID counter.
    next_id: u64,
}

impl UserPredictor {
    pub fn new(config: PredictorConfig) -> Self {
        Self {
            config,
            patterns: Vec::new(),
            history: Vec::new(),
            max_history: 500,
            next_id: 0,
        }
    }

    /// Record a user interaction for pattern learning.
    pub fn record_interaction(&mut self, record: InteractionRecord) {
        let matched = self.try_match_pattern(&record);
        self.history.push(record.clone());

        if self.history.len() > self.max_history {
            self.history.remove(0);
        }

        // If no existing pattern matched, try to discover new ones
        if !matched {
            self.discover_patterns();
        }
    }

    /// Get predictions for the current time.
    pub fn predict_now(&self) -> Vec<PredictedNeed> {
        let now = Utc::now();
        self.predict_at(&now)
    }

    /// Get predictions for a specific time.
    pub fn predict_at(&self, at: &DateTime<Utc>) -> Vec<PredictedNeed> {
        self.patterns.iter()
            .filter(|p| {
                p.matches_now(at)
                    && p.frequency >= self.config.min_frequency
                    && p.confidence >= self.config.min_confidence
            })
            .map(|p| PredictedNeed {
                description: p.description.clone(),
                keywords: p.action_keywords.clone(),
                confidence: p.confidence,
                pattern_id: p.id.clone(),
                time_slot: p.time_slot.clone(),
            })
            .collect()
    }

    /// Get prepared actions — things the agent could do proactively.
    pub fn prepare_actions(&self) -> Vec<PreparedAction> {
        let predictions = self.predict_now();
        predictions.into_iter().map(|p| {
            let tools = if p.keywords.iter().any(|k| k.contains("summary") || k.contains("standup")) {
                vec!["search_files".into(), "read_file".into()]
            } else if p.keywords.iter().any(|k| k.contains("deploy") || k.contains("build")) {
                vec!["execute_command".into()]
            } else {
                vec!["search_files".into()]
            };

            PreparedAction {
                description: p.description,
                suggested_tools: tools,
                ready_by: p.time_slot,
                pattern_id: p.pattern_id,
            }
        }).collect()
    }

    /// Try to match an interaction against existing patterns.
    fn try_match_pattern(&mut self, record: &InteractionRecord) -> bool {
        for pattern in self.patterns.iter_mut() {
            if pattern.time_slot.matches(&record.timestamp)
                && keyword_overlap(&pattern.action_keywords, &record.action_keywords) >= 0.3
            {
                pattern.reinforce();
                debug!(
                    pattern = %pattern.id,
                    frequency = pattern.frequency,
                    confidence = pattern.confidence,
                    "user_predict: pattern reinforced"
                );
                return true;
            }
        }
        false
    }

    /// Discover new patterns from interaction history using time-bucketing.
    fn discover_patterns(&mut self) {
        if self.history.len() < 5 {
            return; // not enough data
        }

        // Group interactions by (day_of_week, hour_bucket) → keywords
        let window = self.config.slot_window_hours;
        let mut buckets: HashMap<(Weekday, u32), Vec<Vec<String>>> = HashMap::new();

        for record in &self.history {
            let bucket_hour = (record.hour / window) * window;
            let key = (record.day, bucket_hour);
            buckets.entry(key).or_default().push(record.action_keywords.clone());
        }

        // Find buckets with at least min_frequency entries that share keywords
        for ((day, hour), keyword_lists) in &buckets {
            if keyword_lists.len() < self.config.min_frequency as usize {
                continue;
            }

            // Find common keywords across entries in this bucket
            let common = find_common_keywords(keyword_lists);
            if common.is_empty() {
                continue;
            }

            // Check if we already have a pattern for this slot+keywords
            let already_exists = self.patterns.iter().any(|p| {
                p.time_slot.matches_day(*day)
                    && p.time_slot.start_hour == *hour
                    && keyword_overlap(&p.action_keywords, &common) >= 0.5
            });

            if already_exists {
                continue;
            }

            // Create new pattern
            self.next_id += 1;
            let pattern = TemporalPattern {
                id: format!("tp_{}", self.next_id),
                time_slot: TimeSlot::new(Some(*day), *hour, hour + window),
                action_keywords: common.clone(),
                typical_tool: None,
                description: format!(
                    "{:?} at {}:00–{}:00: {}",
                    day, hour, hour + window,
                    common.join(", ")
                ),
                frequency: keyword_lists.len() as u32,
                confidence: (1.0 - 1.0 / (keyword_lists.len() as f64 + 1.0)).min(0.95),
                last_matched: Utc::now(),
            };

            info!(
                pattern = %pattern.id,
                description = %pattern.description,
                frequency = pattern.frequency,
                "user_predict: discovered new temporal pattern"
            );

            self.patterns.push(pattern);
        }

        // Evict old patterns if over capacity
        if self.patterns.len() > self.config.max_patterns {
            self.patterns.sort_by(|a, b| {
                b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal)
            });
            self.patterns.truncate(self.config.max_patterns);
        }
    }

    pub fn pattern_count(&self) -> usize {
        self.patterns.len()
    }

    pub fn history_count(&self) -> usize {
        self.history.len()
    }
}

impl Default for UserPredictor {
    fn default() -> Self {
        Self::new(PredictorConfig::default())
    }
}

// Add matches_day helper to TimeSlot
impl TimeSlot {
    pub fn matches_day(&self, day: Weekday) -> bool {
        self.day.map_or(true, |d| d == day)
    }
}

/// Find keywords that appear in ≥50% of the lists.
fn find_common_keywords(lists: &[Vec<String>]) -> Vec<String> {
    if lists.is_empty() { return vec![]; }
    let threshold = lists.len() / 2;
    let mut counts: HashMap<&str, usize> = HashMap::new();

    for list in lists {
        // Deduplicate within each list
        let unique: std::collections::HashSet<&str> = list.iter().map(|s| s.as_str()).collect();
        for kw in unique {
            *counts.entry(kw).or_insert(0) += 1;
        }
    }

    counts.into_iter()
        .filter(|(_, count)| *count > threshold)
        .map(|(kw, _)| kw.to_string())
        .collect()
}

/// Keyword Jaccard overlap.
fn keyword_overlap(a: &[String], b: &[String]) -> f64 {
    use std::collections::HashSet;
    if a.is_empty() && b.is_empty() { return 1.0; }
    if a.is_empty() || b.is_empty() { return 0.0; }
    let sa: HashSet<&str> = a.iter().map(|s| s.as_str()).collect();
    let sb: HashSet<&str> = b.iter().map(|s| s.as_str()).collect();
    let intersection = sa.intersection(&sb).count();
    let union = sa.union(&sb).count();
    if union == 0 { 0.0 } else { intersection as f64 / union as f64 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn monday_record(hour: u32, keywords: &[&str]) -> InteractionRecord {
        let ts = Utc.with_ymd_and_hms(2026, 3, 23, hour, 0, 0).unwrap();
        InteractionRecord::new(
            keywords.iter().map(|s| s.to_string()).collect(),
            ts,
        )
    }

    #[test]
    fn pattern_discovery_from_history() {
        let mut pred = UserPredictor::new(PredictorConfig {
            min_frequency: 3,
            slot_window_hours: 2,
            ..Default::default()
        });

        // Record 5 Monday morning standup requests (need ≥5 for pattern discovery)
        for i in 0..5 {
            let ts = Utc.with_ymd_and_hms(2026, 3, 2 + i * 7, 9, 30, 0).unwrap(); // Mondays
            pred.record_interaction(InteractionRecord::new(
                vec!["standup".into(), "summary".into(), "yesterday".into()],
                ts,
            ));
        }

        assert!(pred.pattern_count() > 0, "should discover a monday standup pattern (history={})", pred.history_count());
    }

    #[test]
    fn predictions_at_matching_time() {
        let mut pred = UserPredictor::default();
        // Manually add a pattern
        pred.patterns.push(TemporalPattern {
            id: "test".into(),
            time_slot: TimeSlot::new(Some(Weekday::Mon), 9, 11),
            action_keywords: vec!["standup".into()],
            typical_tool: None,
            description: "Monday standup".into(),
            frequency: 5,
            confidence: 0.8,
            last_matched: Utc::now(),
        });

        let monday_10am = Utc.with_ymd_and_hms(2026, 3, 23, 10, 0, 0).unwrap();
        let predictions = pred.predict_at(&monday_10am);
        assert!(!predictions.is_empty());

        let tuesday_10am = Utc.with_ymd_and_hms(2026, 3, 24, 10, 0, 0).unwrap();
        let no_predictions = pred.predict_at(&tuesday_10am);
        assert!(no_predictions.is_empty());
    }

    #[test]
    fn pattern_reinforcement() {
        let mut pred = UserPredictor::default();
        pred.patterns.push(TemporalPattern {
            id: "deploy".into(),
            time_slot: TimeSlot::new(Some(Weekday::Fri), 14, 16),
            action_keywords: vec!["deploy".into(), "production".into()],
            typical_tool: None,
            description: "Friday deploy".into(),
            frequency: 3,
            confidence: 0.6,
            last_matched: Utc::now(),
        });

        let friday_3pm = Utc.with_ymd_and_hms(2026, 3, 27, 15, 0, 0).unwrap();
        let record = InteractionRecord::new(
            vec!["deploy".into(), "production".into(), "release".into()],
            friday_3pm,
        );
        pred.record_interaction(record);
        assert_eq!(pred.patterns[0].frequency, 4);
    }

    #[test]
    fn common_keywords_extraction() {
        let lists = vec![
            vec!["standup".into(), "summary".into(), "morning".into()],
            vec!["standup".into(), "summary".into(), "team".into()],
            vec!["standup".into(), "update".into()],
        ];
        let common = find_common_keywords(&lists);
        assert!(common.contains(&"standup".to_string()));
    }
}
