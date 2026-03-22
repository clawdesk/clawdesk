//! Frustration detection — notices when the user is getting annoyed.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Frustration level — coarse, not false-precise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum FrustrationLevel {
    /// User is happy / neutral.
    Calm,
    /// Mild annoyance — slightly shorter messages, minor repetition.
    Rising,
    /// Clearly frustrated — repeated questions, terse responses, negative markers.
    Frustrated,
    /// Actively upset — all-caps, profanity, explicit complaints about the agent.
    Critical,
}

/// Detects frustration from message patterns.
pub struct FrustrationDetector {
    /// Recent message lengths (sliding window).
    msg_lengths: VecDeque<usize>,
    /// Recent message timestamps.
    timestamps: VecDeque<DateTime<Utc>>,
    /// Count of repeated/rephrased questions.
    repetition_count: u32,
    /// Last message content fingerprint (for repetition detection).
    last_fingerprint: Option<u64>,
    /// EWMA of frustration score (0.0–1.0).
    frustration_ewma: f64,
    /// Window size for sliding analysis.
    window_size: usize,
}

impl FrustrationDetector {
    pub fn new() -> Self {
        Self {
            msg_lengths: VecDeque::new(),
            timestamps: VecDeque::new(),
            repetition_count: 0,
            last_fingerprint: None,
            frustration_ewma: 0.0,
            window_size: 10,
        }
    }

    /// Analyze a new user message for frustration signals.
    pub fn observe(&mut self, text: &str) -> FrustrationLevel {
        let now = Utc::now();
        let len = text.len();

        // Update sliding windows
        self.msg_lengths.push_back(len);
        self.timestamps.push_back(now);
        if self.msg_lengths.len() > self.window_size {
            self.msg_lengths.pop_front();
            self.timestamps.pop_front();
        }

        // Compute frustration signals
        let mut score = 0.0;

        // Signal 1: Message getting shorter (user losing patience)
        if self.msg_lengths.len() >= 3 {
            let recent: Vec<usize> = self.msg_lengths.iter().rev().take(3).copied().collect();
            if recent[0] < recent.get(1).copied().unwrap_or(recent[0])
                && recent.get(1).copied().unwrap_or(0) < recent.get(2).copied().unwrap_or(0)
            {
                score += 0.15; // declining message length
            }
        }

        // Signal 2: Repetition (asking the same thing again)
        let fp = fingerprint(text);
        if let Some(last) = self.last_fingerprint {
            if fp == last {
                self.repetition_count += 1;
                score += 0.2 * self.repetition_count as f64;
            } else {
                self.repetition_count = 0;
            }
        }
        self.last_fingerprint = Some(fp);

        // Signal 3: Frustration markers in text
        let lower = text.to_lowercase();
        let frustration_markers = [
            "doesn't work", "still broken", "wrong", "not what i asked",
            "again", "already told you", "please just", "why can't",
            "??", "!!",
        ];
        let marker_hits = frustration_markers.iter()
            .filter(|m| lower.contains(*m))
            .count();
        score += marker_hits as f64 * 0.15;

        // Signal 4: All-caps (shouting)
        let upper_ratio = text.chars().filter(|c| c.is_uppercase()).count() as f64
            / text.chars().filter(|c| c.is_alphabetic()).count().max(1) as f64;
        if upper_ratio > 0.7 && text.len() > 5 {
            score += 0.3;
        }

        // Signal 5: Very short follow-up messages (terse responses)
        if len < 15 && self.msg_lengths.len() >= 2 {
            score += 0.1;
        }

        // EWMA update
        const ALPHA: f64 = 0.35;
        self.frustration_ewma = ALPHA * score.min(1.0) + (1.0 - ALPHA) * self.frustration_ewma;

        self.level()
    }

    /// Current frustration level.
    pub fn level(&self) -> FrustrationLevel {
        if self.frustration_ewma >= 0.7 { FrustrationLevel::Critical }
        else if self.frustration_ewma >= 0.4 { FrustrationLevel::Frustrated }
        else if self.frustration_ewma >= 0.15 { FrustrationLevel::Rising }
        else { FrustrationLevel::Calm }
    }

    /// Raw EWMA score for fine-grained use.
    pub fn score(&self) -> f64 {
        self.frustration_ewma
    }

    /// Reset after a positive interaction (user expressed satisfaction).
    pub fn reset(&mut self) {
        self.frustration_ewma = 0.0;
        self.repetition_count = 0;
    }
}

impl Default for FrustrationDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Quick fingerprint for repetition detection (not crypto-quality, just fast).
fn fingerprint(text: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    // Normalize: lowercase, collapse whitespace, remove punctuation
    let normalized: String = text.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let mut hasher = DefaultHasher::new();
    normalized.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calm_with_normal_messages() {
        let mut det = FrustrationDetector::new();
        det.observe("Can you help me fix this build error in my Rust project?");
        det.observe("Thanks, now can you also check the test file?");
        assert_eq!(det.level(), FrustrationLevel::Calm);
    }

    #[test]
    fn rising_with_repetition() {
        let mut det = FrustrationDetector::new();
        det.observe("Fix the build error");
        det.observe("Fix the build error");
        det.observe("Fix the build error");
        assert!(det.level() >= FrustrationLevel::Rising);
    }

    #[test]
    fn frustrated_with_markers() {
        let mut det = FrustrationDetector::new();
        det.observe("This doesn't work, it's still broken! wrong answer!!");
        det.observe("NOT WHAT I ASKED!! STILL BROKEN!! WHY CAN'T YOU FIX IT??");
        det.observe("NOT WHAT I ASKED!! STILL BROKEN!! WHY CAN'T YOU FIX IT??");
        det.observe("WRONG AGAIN?? DOESN'T WORK!!");
        assert!(det.level() >= FrustrationLevel::Frustrated,
            "level was {:?}, score was {}", det.level(), det.score());
    }

    #[test]
    fn reset_clears_frustration() {
        let mut det = FrustrationDetector::new();
        det.observe("This doesn't work!! Still broken!!");
        det.observe("WRONG AGAIN??");
        assert!(det.level() >= FrustrationLevel::Rising);
        det.reset();
        assert_eq!(det.level(), FrustrationLevel::Calm);
    }
}
