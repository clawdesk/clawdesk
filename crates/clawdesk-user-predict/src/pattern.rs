//! Temporal patterns — recurring user behaviors.

use chrono::{DateTime, Datelike, NaiveTime, Timelike, Utc, Weekday};
use serde::{Deserialize, Serialize};

/// A time slot for pattern matching (hour-of-day + optional day-of-week).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeSlot {
    /// Day of week (None = any day).
    pub day: Option<Weekday>,
    /// Start of the time window (hour:minute).
    pub start_hour: u32,
    pub start_minute: u32,
    /// End of the time window (hour:minute).
    pub end_hour: u32,
    pub end_minute: u32,
}

impl TimeSlot {
    pub fn new(day: Option<Weekday>, start_hour: u32, end_hour: u32) -> Self {
        Self {
            day,
            start_hour,
            start_minute: 0,
            end_hour,
            end_minute: 0,
        }
    }

    /// Whether the given timestamp falls within this slot.
    pub fn matches(&self, dt: &DateTime<Utc>) -> bool {
        // Day check
        if let Some(day) = self.day {
            if dt.weekday() != day {
                return false;
            }
        }
        // Time check
        let hour = dt.hour();
        let minute = dt.minute();
        let current = hour * 60 + minute;
        let start = self.start_hour * 60 + self.start_minute;
        let end = self.end_hour * 60 + self.end_minute;

        if start <= end {
            current >= start && current < end
        } else {
            // Wraps midnight
            current >= start || current < end
        }
    }
}

/// A record of a user interaction (for learning patterns).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionRecord {
    /// What the user asked/did (condensed keywords).
    pub action_keywords: Vec<String>,
    /// When it happened.
    pub timestamp: DateTime<Utc>,
    /// The day of week.
    pub day: Weekday,
    /// The hour of day (0–23).
    pub hour: u32,
    /// Optional tool that was heavily used.
    pub primary_tool: Option<String>,
}

impl InteractionRecord {
    pub fn new(keywords: Vec<String>, timestamp: DateTime<Utc>) -> Self {
        Self {
            action_keywords: keywords,
            day: timestamp.weekday(),
            hour: timestamp.hour(),
            primary_tool: None,
            timestamp,
        }
    }

    pub fn with_tool(mut self, tool: impl Into<String>) -> Self {
        self.primary_tool = Some(tool.into());
        self
    }
}

/// A learned temporal pattern: "At time T, the user typically does X."
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemporalPattern {
    /// Unique pattern identifier.
    pub id: String,
    /// When this pattern fires.
    pub time_slot: TimeSlot,
    /// What the user typically asks for (keywords).
    pub action_keywords: Vec<String>,
    /// What tool is typically needed.
    pub typical_tool: Option<String>,
    /// Description of the pattern.
    pub description: String,
    /// How many times this pattern has been observed.
    pub frequency: u32,
    /// Confidence in this pattern (0.0–1.0).
    pub confidence: f64,
    /// Last time this pattern fired.
    pub last_matched: DateTime<Utc>,
}

impl TemporalPattern {
    /// Whether this pattern applies to the given time.
    pub fn matches_now(&self, now: &DateTime<Utc>) -> bool {
        self.time_slot.matches(now)
    }

    /// Update after a new observation that matches this pattern.
    pub fn reinforce(&mut self) {
        self.frequency += 1;
        self.last_matched = Utc::now();
        // Confidence grows with frequency, max 0.95
        self.confidence = (1.0 - 1.0 / (self.frequency as f64 + 1.0)).min(0.95);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn time_slot_matching() {
        let slot = TimeSlot::new(Some(Weekday::Mon), 9, 12);
        // Monday at 10am UTC
        let monday_10am = Utc.with_ymd_and_hms(2026, 3, 23, 10, 0, 0).unwrap(); // March 23, 2026 is Monday
        assert!(slot.matches(&monday_10am));

        // Tuesday at 10am
        let tuesday_10am = Utc.with_ymd_and_hms(2026, 3, 24, 10, 0, 0).unwrap();
        assert!(!slot.matches(&tuesday_10am));

        // Monday at 1pm (outside window)
        let monday_1pm = Utc.with_ymd_and_hms(2026, 3, 23, 13, 0, 0).unwrap();
        assert!(!slot.matches(&monday_1pm));
    }

    #[test]
    fn any_day_slot() {
        let slot = TimeSlot::new(None, 8, 10); // 8-10am any day
        let morning = Utc.with_ymd_and_hms(2026, 3, 21, 9, 0, 0).unwrap();
        assert!(slot.matches(&morning));
    }

    #[test]
    fn pattern_confidence_grows() {
        let mut pattern = TemporalPattern {
            id: "standup".into(),
            time_slot: TimeSlot::new(Some(Weekday::Mon), 9, 10),
            action_keywords: vec!["standup".into(), "summary".into()],
            typical_tool: None,
            description: "Monday standup summary".into(),
            frequency: 1,
            confidence: 0.5,
            last_matched: Utc::now(),
        };
        pattern.reinforce();
        pattern.reinforce();
        pattern.reinforce();
        assert!(pattern.confidence > 0.7, "confidence should grow: {}", pattern.confidence);
    }
}
