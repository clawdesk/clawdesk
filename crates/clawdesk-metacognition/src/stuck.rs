//! Stuck detection — identifies when the agent is going in circles.
//!
//! Three convergent signals must fire simultaneously:
//! 1. Tool repetition — the same tool set appears repeatedly
//! 2. Output similarity — consecutive outputs are near-identical
//! 3. Time without progress — wall-clock time exceeds threshold
//!
//! Any single signal is just noise (e.g., repeated `read_file` is normal
//! during research). All three together indicate genuine stuckness.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Configuration for stuck detection.
#[derive(Debug, Clone)]
pub struct StuckConfig {
    /// Number of consecutive turns with identical tool sets to trigger.
    pub repeated_tool_threshold: usize,
    /// Jaccard similarity threshold for "same tool set" (0.0–1.0).
    pub tool_similarity_threshold: f64,
    /// Cosine-like similarity threshold for "same output" (0.0–1.0).
    /// Uses char-trigram Jaccard as a cheap proxy for semantic similarity.
    pub output_similarity_threshold: f64,
    /// Wall-clock time without meaningful progress before flagging.
    pub time_without_progress: Duration,
    /// How many recent turns to keep in the sliding window.
    pub window_size: usize,
}

impl Default for StuckConfig {
    fn default() -> Self {
        Self {
            repeated_tool_threshold: 3,
            tool_similarity_threshold: 0.8,
            output_similarity_threshold: 0.85,
            time_without_progress: Duration::from_secs(120),
            window_size: 8,
        }
    }
}

/// A single signal that contributes to the stuck verdict.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StuckSignal {
    /// The same tools were called N times in a row.
    RepeatedTools {
        tool_names: Vec<String>,
        consecutive_count: usize,
    },
    /// Consecutive outputs are near-identical.
    OutputConvergence {
        similarity: f64,
        window: usize,
    },
    /// No meaningful progress for this duration.
    TimeWithoutProgress {
        elapsed: Duration,
    },
}

/// The result of a stuck detection check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StuckReport {
    /// Whether the agent appears stuck.
    pub is_stuck: bool,
    /// Fired signals (may be non-empty even if not stuck — needs convergence).
    pub signals: Vec<StuckSignal>,
    /// Human-readable explanation.
    pub reason: String,
    /// How many consecutive turns have shown stuck signals.
    pub stuck_streak: usize,
}

/// Snapshot of a single turn for the detector's sliding window.
#[derive(Debug, Clone)]
struct TurnRecord {
    tool_names: Vec<String>,
    output_trigrams: Vec<u64>,
    timestamp: Instant,
    had_progress: bool,
}

/// Detects when the agent is going in circles.
pub struct StuckDetector {
    config: StuckConfig,
    window: VecDeque<TurnRecord>,
    last_progress: Instant,
    stuck_streak: usize,
    /// Self-calibration: tracks (stuck_declared, strategy_switch_improved_outcome).
    calibration: (u32, u32),
}

impl StuckDetector {
    pub fn new(config: StuckConfig) -> Self {
        Self {
            config,
            window: VecDeque::new(),
            last_progress: Instant::now(),
            stuck_streak: 0,
            calibration: (0, 0),
        }
    }

    /// Record a new turn and check for stuckness.
    pub fn observe(
        &mut self,
        tool_names: &[String],
        output_text: &str,
        had_new_tool_results: bool,
    ) -> StuckReport {
        let now = Instant::now();
        let trigrams = char_trigram_hashes(output_text);

        let had_progress = had_new_tool_results
            && !output_text.is_empty()
            && self.output_differs_from_recent(&trigrams);

        if had_progress {
            self.last_progress = now;
        }

        let record = TurnRecord {
            tool_names: tool_names.to_vec(),
            output_trigrams: trigrams,
            timestamp: now,
            had_progress,
        };
        self.window.push_back(record);
        if self.window.len() > self.config.window_size {
            self.window.pop_front();
        }

        self.evaluate()
    }

    /// Run the three-signal convergence check.
    fn evaluate(&mut self) -> StuckReport {
        let mut signals = Vec::new();

        // Signal 1: Tool repetition
        if let Some(rep) = self.check_tool_repetition() {
            signals.push(rep);
        }

        // Signal 2: Output convergence
        if let Some(conv) = self.check_output_convergence() {
            signals.push(conv);
        }

        // Signal 3: Time without progress
        let elapsed = self.last_progress.elapsed();
        if elapsed >= self.config.time_without_progress {
            signals.push(StuckSignal::TimeWithoutProgress { elapsed });
        }

        // Require at least 2 signals for stuck (convergent evidence)
        let is_stuck = signals.len() >= 2;

        if is_stuck {
            self.stuck_streak += 1;
        } else {
            self.stuck_streak = 0;
        }

        let reason = if is_stuck {
            let parts: Vec<String> = signals.iter().map(|s| match s {
                StuckSignal::RepeatedTools { tool_names, consecutive_count } => {
                    format!(
                        "same tools [{}] called {} times",
                        tool_names.join(", "),
                        consecutive_count
                    )
                }
                StuckSignal::OutputConvergence { similarity, window } => {
                    format!(
                        "outputs {:.0}% similar over last {} turns",
                        similarity * 100.0,
                        window
                    )
                }
                StuckSignal::TimeWithoutProgress { elapsed } => {
                    format!("no progress for {:.0}s", elapsed.as_secs_f64())
                }
            }).collect();
            parts.join("; ")
        } else {
            String::new()
        };

        StuckReport {
            is_stuck,
            signals,
            reason,
            stuck_streak: self.stuck_streak,
        }
    }

    fn check_tool_repetition(&self) -> Option<StuckSignal> {
        if self.window.len() < self.config.repeated_tool_threshold {
            return None;
        }

        let recent: Vec<&TurnRecord> = self.window.iter().rev()
            .take(self.config.repeated_tool_threshold)
            .collect();

        // Check if all recent turns used similar tool sets (Jaccard)
        let reference = &recent[0].tool_names;
        let all_similar = recent.iter().skip(1).all(|r| {
            jaccard_similarity(reference, &r.tool_names) >= self.config.tool_similarity_threshold
        });

        if all_similar {
            Some(StuckSignal::RepeatedTools {
                tool_names: reference.clone(),
                consecutive_count: self.config.repeated_tool_threshold,
            })
        } else {
            None
        }
    }

    fn check_output_convergence(&self) -> Option<StuckSignal> {
        if self.window.len() < 2 {
            return None;
        }

        // Compare the last 3 outputs pairwise
        let compare_count = self.window.len().min(3);
        let recent: Vec<&TurnRecord> = self.window.iter().rev()
            .take(compare_count)
            .collect();

        let mut total_sim = 0.0;
        let mut pairs = 0;
        for i in 0..recent.len() {
            for j in (i + 1)..recent.len() {
                total_sim += trigram_jaccard(&recent[i].output_trigrams, &recent[j].output_trigrams);
                pairs += 1;
            }
        }

        if pairs == 0 {
            return None;
        }

        let avg_sim = total_sim / pairs as f64;
        if avg_sim >= self.config.output_similarity_threshold {
            Some(StuckSignal::OutputConvergence {
                similarity: avg_sim,
                window: compare_count,
            })
        } else {
            None
        }
    }

    fn output_differs_from_recent(&self, trigrams: &[u64]) -> bool {
        if let Some(last) = self.window.back() {
            trigram_jaccard(trigrams, &last.output_trigrams) < self.config.output_similarity_threshold
        } else {
            true
        }
    }

    /// Record whether a strategy switch (triggered by a stuck verdict)
    /// actually improved the outcome. Used for self-calibration.
    pub fn record_calibration(&mut self, switch_improved: bool) {
        self.calibration.0 += 1;
        if switch_improved {
            self.calibration.1 += 1;
        }
    }

    /// Accuracy of stuck verdicts that led to improvements.
    pub fn calibration_accuracy(&self) -> Option<f64> {
        if self.calibration.0 == 0 {
            None
        } else {
            Some(self.calibration.1 as f64 / self.calibration.0 as f64)
        }
    }

    pub fn stuck_streak(&self) -> usize {
        self.stuck_streak
    }
}

// ═══════════════════════════════════════════════════════════════════════
// Cheap similarity primitives (no external dependencies)
// ═══════════════════════════════════════════════════════════════════════

/// Hash character trigrams for fast similarity comparison.
fn char_trigram_hashes(text: &str) -> Vec<u64> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let chars: Vec<char> = text.chars().collect();
    if chars.len() < 3 {
        return vec![];
    }

    let mut hashes = Vec::with_capacity(chars.len() - 2);
    for window in chars.windows(3) {
        let mut hasher = DefaultHasher::new();
        window.hash(&mut hasher);
        hashes.push(hasher.finish());
    }
    hashes.sort_unstable();
    hashes.dedup();
    hashes
}

/// Jaccard similarity between two sorted-deduplicated hash sets.
fn trigram_jaccard(a: &[u64], b: &[u64]) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }

    let mut i = 0;
    let mut j = 0;
    let mut intersection = 0usize;
    let mut union = 0usize;

    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Equal => {
                intersection += 1;
                union += 1;
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => {
                union += 1;
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                union += 1;
                j += 1;
            }
        }
    }
    union += (a.len() - i) + (b.len() - j);

    intersection as f64 / union as f64
}

/// Jaccard similarity between two string-element sets.
fn jaccard_similarity(a: &[String], b: &[String]) -> f64 {
    use std::collections::HashSet;
    let sa: HashSet<&str> = a.iter().map(|s| s.as_str()).collect();
    let sb: HashSet<&str> = b.iter().map(|s| s.as_str()).collect();
    let intersection = sa.intersection(&sb).count();
    let union = sa.union(&sb).count();
    if union == 0 { 1.0 } else { intersection as f64 / union as f64 }
}

// ═══════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_stuck_on_varied_tool_use() {
        let mut det = StuckDetector::new(StuckConfig::default());
        let r1 = det.observe(
            &["read_file".into()],
            "file contents here...",
            true,
        );
        assert!(!r1.is_stuck);

        let r2 = det.observe(
            &["search_files".into()],
            "found 3 matches in src/",
            true,
        );
        assert!(!r2.is_stuck);
    }

    #[test]
    fn stuck_on_repeated_identical_output() {
        let mut det = StuckDetector::new(StuckConfig {
            repeated_tool_threshold: 2,
            time_without_progress: Duration::from_millis(1),
            ..Default::default()
        });

        // Same tool, same output, 3 times
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(2));
            det.observe(
                &["execute_command".into()],
                "Error: command failed with exit code 1. Permission denied.",
                false,
            );
        }

        let r = det.observe(
            &["execute_command".into()],
            "Error: command failed with exit code 1. Permission denied.",
            false,
        );
        assert!(r.is_stuck);
        assert!(r.stuck_streak >= 1);
    }

    #[test]
    fn trigram_similarity_identical() {
        let h1 = char_trigram_hashes("hello world foo bar");
        let h2 = char_trigram_hashes("hello world foo bar");
        assert!((trigram_jaccard(&h1, &h2) - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn trigram_similarity_different() {
        let h1 = char_trigram_hashes("the quick brown fox");
        let h2 = char_trigram_hashes("completely different text entirely");
        assert!(trigram_jaccard(&h1, &h2) < 0.3);
    }

    #[test]
    fn calibration_tracking() {
        let mut det = StuckDetector::new(StuckConfig::default());
        assert!(det.calibration_accuracy().is_none());
        det.record_calibration(true);
        det.record_calibration(false);
        det.record_calibration(true);
        assert!((det.calibration_accuracy().unwrap() - 2.0 / 3.0).abs() < 0.01);
    }
}
