//! # Multimodal Journal Skill
//!
//! Structured journaling framework with vision ingestion, time-series storage,
//! and trigger identification via case-crossover study design.
//!
//! ## Architecture
//!
//! ```text
//! input (text/photo/voice) → parse & tag → store time-series entry
//!                                               ↓
//!                            periodic analysis ← query window
//!                                               ↓
//!                            case-crossover study → odds ratios → insights
//! ```
//!
//! ### Case-Crossover Study Design
//!
//! For identifying triggers (e.g., "does coffee after 3pm affect my sleep?"):
//!
//! ```text
//! OR = (a · d) / (b · c)
//! where:
//!   a = case days with exposure
//!   b = case days without exposure
//!   c = control days with exposure
//!   d = control days without exposure
//! ```

use chrono::{DateTime, Utc, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Type of journal entry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum EntryType {
    /// Freeform text
    Text,
    /// Food/drink log
    Food,
    /// Exercise/activity log
    Activity,
    /// Mood/emotion check-in
    Mood,
    /// Sleep log
    Sleep,
    /// Medication/supplement
    Medication,
    /// Photo with optional annotation
    Photo,
    /// Voice memo transcription
    Voice,
    /// Symptom or health observation
    Symptom,
    /// Custom/other
    Custom,
}

/// A single journal entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Unique entry ID
    pub id: String,
    /// Entry type
    pub entry_type: EntryType,
    /// When the entry was recorded
    pub recorded_at: DateTime<Utc>,
    /// When the activity/observation actually occurred (may differ from recorded_at)
    pub occurred_at: DateTime<Utc>,
    /// Textual content
    pub content: String,
    /// Structured data (e.g., {"calories": 500, "food": "salad"})
    pub data: HashMap<String, serde_json::Value>,
    /// Tags for categorization and querying
    pub tags: Vec<String>,
    /// Numeric value if applicable (e.g., mood 1-10, sleep hours)
    pub value: Option<f64>,
    /// Unit of the value (e.g., "hours", "kcal", "rating")
    pub unit: Option<String>,
    /// Media attachments (paths or URIs)
    pub attachments: Vec<String>,
    /// Source channel/device
    pub source: String,
}

/// Time series of journal entries for analysis.
#[derive(Debug, Clone, Default)]
pub struct JournalTimeSeries {
    /// Entries sorted by occurred_at
    entries: Vec<JournalEntry>,
}

impl JournalTimeSeries {
    pub fn new() -> Self {
        Self { entries: Vec::new() }
    }

    /// Add an entry, maintaining chronological order.
    pub fn add(&mut self, entry: JournalEntry) {
        let pos = self.entries
            .binary_search_by(|e| e.occurred_at.cmp(&entry.occurred_at))
            .unwrap_or_else(|i| i);
        self.entries.insert(pos, entry);
    }

    /// Get entries within a date range.
    pub fn range(&self, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<&JournalEntry> {
        self.entries.iter()
            .filter(|e| e.occurred_at >= from && e.occurred_at <= to)
            .collect()
    }

    /// Get entries of a specific type.
    pub fn by_type(&self, entry_type: EntryType) -> Vec<&JournalEntry> {
        self.entries.iter()
            .filter(|e| e.entry_type == entry_type)
            .collect()
    }

    /// Get entries with a specific tag.
    pub fn by_tag(&self, tag: &str) -> Vec<&JournalEntry> {
        self.entries.iter()
            .filter(|e| e.tags.iter().any(|t| t == tag))
            .collect()
    }

    /// Get all entries.
    pub fn entries(&self) -> &[JournalEntry] {
        &self.entries
    }

    /// Get daily aggregates for a given entry type.
    pub fn daily_values(&self, entry_type: EntryType) -> Vec<(NaiveDate, f64)> {
        let mut days: HashMap<NaiveDate, Vec<f64>> = HashMap::new();
        for entry in self.entries.iter().filter(|e| e.entry_type == entry_type) {
            if let Some(val) = entry.value {
                let date = entry.occurred_at.date_naive();
                days.entry(date).or_default().push(val);
            }
        }
        let mut result: Vec<(NaiveDate, f64)> = days.into_iter()
            .map(|(date, vals)| {
                let avg = vals.iter().sum::<f64>() / vals.len() as f64;
                (date, avg)
            })
            .collect();
        result.sort_by_key(|(d, _)| *d);
        result
    }
}

/// Case-crossover study design for trigger identification.
///
/// Compares exposure frequency on "case days" (when outcome occurred)
/// vs "control days" (matched reference days without outcome).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseCrossoverStudy {
    /// What we're studying (e.g., "poor_sleep")
    pub outcome: String,
    /// What exposure we're testing (e.g., "afternoon_coffee")
    pub exposure: String,
    /// Case days with exposure (a)
    pub case_exposed: u32,
    /// Case days without exposure (b)
    pub case_unexposed: u32,
    /// Control days with exposure (c)
    pub control_exposed: u32,
    /// Control days without exposure (d)
    pub control_unexposed: u32,
}

impl CaseCrossoverStudy {
    /// Calculate the odds ratio.
    ///
    /// OR = (a * d) / (b * c)
    ///
    /// Returns None if denominator is zero.
    pub fn odds_ratio(&self) -> Option<f64> {
        let a = self.case_exposed as f64;
        let b = self.case_unexposed as f64;
        let c = self.control_exposed as f64;
        let d = self.control_unexposed as f64;

        let denom = b * c;
        if denom == 0.0 {
            return None;
        }
        Some((a * d) / denom)
    }

    /// Interpret the odds ratio.
    pub fn interpret(&self) -> TriggerInterpretation {
        match self.odds_ratio() {
            None => TriggerInterpretation::InsufficientData,
            Some(or) if or < 0.5 => TriggerInterpretation::Protective,
            Some(or) if or > 2.0 => TriggerInterpretation::StrongTrigger,
            Some(or) if or > 1.5 => TriggerInterpretation::ModerateTrigger,
            Some(or) if or > 1.1 => TriggerInterpretation::WeakTrigger,
            Some(_) => TriggerInterpretation::NoAssociation,
        }
    }
}

/// Interpretation of a trigger analysis.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum TriggerInterpretation {
    StrongTrigger,
    ModerateTrigger,
    WeakTrigger,
    NoAssociation,
    Protective,
    InsufficientData,
}

/// Run a case-crossover analysis.
///
/// Given a time series, an outcome predicate, and an exposure predicate,
/// computes the 2x2 table and odds ratio.
pub fn analyze_trigger(
    time_series: &JournalTimeSeries,
    outcome_label: &str,
    exposure_label: &str,
    outcome_fn: impl Fn(&[&JournalEntry]) -> bool,
    exposure_fn: impl Fn(&[&JournalEntry]) -> bool,
    from: NaiveDate,
    to: NaiveDate,
) -> CaseCrossoverStudy {
    let mut study = CaseCrossoverStudy {
        outcome: outcome_label.to_string(),
        exposure: exposure_label.to_string(),
        case_exposed: 0,
        case_unexposed: 0,
        control_exposed: 0,
        control_unexposed: 0,
    };

    let mut date = from;
    while date <= to {
        let day_start = date.and_hms_opt(0, 0, 0)
            .and_then(|ndt| ndt.and_local_timezone(Utc).single());
        let day_end = date.and_hms_opt(23, 59, 59)
            .and_then(|ndt| ndt.and_local_timezone(Utc).single());

        if let (Some(start), Some(end)) = (day_start, day_end) {
            let day_entries = time_series.range(start, end);
            let has_outcome = outcome_fn(&day_entries);
            let has_exposure = exposure_fn(&day_entries);

            match (has_outcome, has_exposure) {
                (true, true) => study.case_exposed += 1,
                (true, false) => study.case_unexposed += 1,
                (false, true) => study.control_exposed += 1,
                (false, false) => study.control_unexposed += 1,
            }
        }

        date = date.succ_opt().unwrap_or(date);
    }

    study
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_odds_ratio() {
        let study = CaseCrossoverStudy {
            outcome: "poor_sleep".into(),
            exposure: "late_coffee".into(),
            case_exposed: 12,
            case_unexposed: 3,
            control_exposed: 4,
            control_unexposed: 11,
        };

        let or = study.odds_ratio().unwrap();
        // OR = (12 * 11) / (3 * 4) = 132/12 = 11.0
        assert!((or - 11.0).abs() < 0.001);
        assert_eq!(study.interpret(), TriggerInterpretation::StrongTrigger);
    }

    #[test]
    fn test_protective_factor() {
        let study = CaseCrossoverStudy {
            outcome: "headache".into(),
            exposure: "exercise".into(),
            case_exposed: 2,
            case_unexposed: 10,
            control_exposed: 12,
            control_unexposed: 6,
        };

        let or = study.odds_ratio().unwrap();
        // OR = (2 * 6) / (10 * 12) = 12/120 = 0.1
        assert!(or < 0.5);
        assert_eq!(study.interpret(), TriggerInterpretation::Protective);
    }

    #[test]
    fn test_time_series() {
        let mut ts = JournalTimeSeries::new();
        let now = Utc::now();

        ts.add(JournalEntry {
            id: "1".into(),
            entry_type: EntryType::Mood,
            recorded_at: now,
            occurred_at: now,
            content: "Feeling great".into(),
            data: HashMap::new(),
            tags: vec!["positive".into()],
            value: Some(8.0),
            unit: Some("rating".into()),
            attachments: vec![],
            source: "manual".into(),
        });

        assert_eq!(ts.entries().len(), 1);
        assert_eq!(ts.by_type(EntryType::Mood).len(), 1);
        assert_eq!(ts.by_type(EntryType::Food).len(), 0);
        assert_eq!(ts.by_tag("positive").len(), 1);
    }
}
