//! The metacognitive monitor — the top-level coordinator that combines
//! stuck detection and approach evaluation into actionable verdicts.
//!
//! This is what gets wired into the runner's execute_loop.

use crate::approach::{ApproachEvaluator, ApproachScore, AlternativeApproach, ApproachSource};
use crate::stuck::{StuckConfig, StuckDetector, StuckReport};
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

/// Configuration for the metacognitive monitor.
#[derive(Debug, Clone)]
pub struct MetacognitiveConfig {
    /// Stuck detection configuration.
    pub stuck: StuckConfig,
    /// Confidence threshold below which we suggest a strategy switch.
    pub switch_confidence_threshold: f64,
    /// Minimum turns before metacognition activates (warm-up period).
    pub warm_up_turns: usize,
}

impl Default for MetacognitiveConfig {
    fn default() -> Self {
        Self {
            stuck: StuckConfig::default(),
            switch_confidence_threshold: 0.25,
            warm_up_turns: 2,
        }
    }
}

/// Snapshot of a single turn, fed into the monitor.
#[derive(Debug, Clone)]
pub struct TurnSnapshot {
    pub tool_names: Vec<String>,
    pub output_text: String,
    pub successful_tools: usize,
    pub total_tools: usize,
    pub had_new_tool_results: bool,
}

/// The metacognitive verdict — what should the agent do differently?
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Verdict {
    /// Everything looks fine. Keep going.
    OnTrack,

    /// The agent appears stuck. Here's why.
    Stuck {
        reason: String,
        streak: usize,
    },

    /// The current approach has low confidence. Consider switching.
    WrongApproach {
        current_confidence: f64,
        suggestion: String,
    },
}

impl Verdict {
    /// Format this verdict as a system message for injection into the LLM context.
    pub fn to_system_message(&self) -> Option<String> {
        match self {
            Verdict::OnTrack => None,
            Verdict::Stuck { reason, streak } => {
                let urgency = if *streak >= 3 { "CRITICAL" } else { "WARNING" };
                Some(format!(
                    "<metacognitive_pause urgency=\"{urgency}\">\n\
                     You appear stuck: {reason}.\n\
                     Step back. What are 3 fundamentally different approaches?\n\
                     Pick the most promising one you haven't tried.\n\
                     Do NOT repeat the same tool calls.\n\
                     </metacognitive_pause>"
                ))
            }
            Verdict::WrongApproach { current_confidence, suggestion } => {
                Some(format!(
                    "<strategy_switch>\n\
                     Current approach has low confidence ({:.0}%).\n\
                     Consider: {suggestion}\n\
                     </strategy_switch>",
                    current_confidence * 100.0
                ))
            }
        }
    }
}

/// The metacognitive monitor — combines stuck detection and approach evaluation.
pub struct MetacognitiveMonitor {
    config: MetacognitiveConfig,
    stuck_detector: StuckDetector,
    approach_eval: ApproachEvaluator,
    turns_observed: usize,
    last_verdict: Verdict,
}

impl MetacognitiveMonitor {
    pub fn new(config: MetacognitiveConfig) -> Self {
        let stuck_detector = StuckDetector::new(config.stuck.clone());
        Self {
            config,
            stuck_detector,
            approach_eval: ApproachEvaluator::new(),
            turns_observed: 0,
            last_verdict: Verdict::OnTrack,
        }
    }

    /// Observe a completed turn and produce a verdict.
    pub fn observe(&mut self, snapshot: &TurnSnapshot) -> &Verdict {
        self.turns_observed += 1;

        // Don't fire during warm-up — first N turns are always "exploring"
        if self.turns_observed <= self.config.warm_up_turns {
            self.last_verdict = Verdict::OnTrack;
            return &self.last_verdict;
        }

        // Run stuck detection
        let stuck_report = self.stuck_detector.observe(
            &snapshot.tool_names,
            &snapshot.output_text,
            snapshot.had_new_tool_results,
        );

        // Run approach evaluation
        let approach_score = self.approach_eval.observe(
            snapshot.successful_tools,
            snapshot.total_tools,
            snapshot.output_text.len(),
        );

        // Produce verdict (stuck takes priority over wrong-approach)
        self.last_verdict = if stuck_report.is_stuck {
            warn!(
                reason = %stuck_report.reason,
                streak = stuck_report.stuck_streak,
                confidence = approach_score.confidence,
                "metacognition: agent appears stuck"
            );
            Verdict::Stuck {
                reason: stuck_report.reason.clone(),
                streak: stuck_report.stuck_streak,
            }
        } else if approach_score.confidence < self.config.switch_confidence_threshold {
            let suggestion = self.generate_suggestion(&approach_score);
            debug!(
                confidence = approach_score.confidence,
                momentum = approach_score.momentum,
                "metacognition: low approach confidence"
            );
            Verdict::WrongApproach {
                current_confidence: approach_score.confidence,
                suggestion,
            }
        } else {
            Verdict::OnTrack
        };

        &self.last_verdict
    }

    /// Evaluate a full trajectory (batch of turns), returning the verdict
    /// for the trajectory as a whole.
    pub fn evaluate_trajectory(&mut self, snapshots: &[TurnSnapshot]) -> &Verdict {
        for snapshot in snapshots {
            self.observe(snapshot);
        }
        &self.last_verdict
    }

    /// Reset the monitor for a new task/strategy.
    pub fn reset(&mut self) {
        self.stuck_detector = StuckDetector::new(self.config.stuck.clone());
        self.approach_eval.reset();
        self.turns_observed = 0;
        self.last_verdict = Verdict::OnTrack;
    }

    /// Record whether a strategy switch improved the outcome (self-calibration).
    pub fn record_switch_outcome(&mut self, improved: bool) {
        self.stuck_detector.record_calibration(improved);
    }

    pub fn current_verdict(&self) -> &Verdict {
        &self.last_verdict
    }

    pub fn approach_confidence(&self) -> f64 {
        self.approach_eval.current_confidence()
    }

    pub fn turns_observed(&self) -> usize {
        self.turns_observed
    }

    fn generate_suggestion(&self, score: &ApproachScore) -> String {
        if score.momentum < -0.1 {
            "Approach is actively deteriorating. Try a completely different strategy: \
             change the tools you're using, reconsider the problem decomposition, \
             or ask for clarification."
                .into()
        } else if score.confidence < 0.15 {
            "Very low confidence. Consider: (1) re-reading the original request, \
             (2) trying a simpler approach, (3) asking the user to clarify ambiguous parts."
                .into()
        } else {
            "Moderate uncertainty. Consider verifying your assumptions before proceeding."
                .into()
        }
    }
}

impl Default for MetacognitiveMonitor {
    fn default() -> Self {
        Self::new(MetacognitiveConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn make_snapshot(tools: &[&str], output: &str, success: usize, total: usize) -> TurnSnapshot {
        TurnSnapshot {
            tool_names: tools.iter().map(|s| s.to_string()).collect(),
            output_text: output.to_string(),
            successful_tools: success,
            total_tools: total,
            had_new_tool_results: success > 0,
        }
    }

    #[test]
    fn on_track_during_warm_up() {
        let mut monitor = MetacognitiveMonitor::new(MetacognitiveConfig {
            warm_up_turns: 3,
            ..Default::default()
        });
        let snap = make_snapshot(&["execute_command"], "error", 0, 1);
        for _ in 0..3 {
            let v = monitor.observe(&snap);
            assert!(matches!(v, Verdict::OnTrack));
        }
    }

    #[test]
    fn detects_stuck_after_repeated_failures() {
        let mut monitor = MetacognitiveMonitor::new(MetacognitiveConfig {
            warm_up_turns: 0,
            stuck: StuckConfig {
                repeated_tool_threshold: 2,
                time_without_progress: Duration::from_millis(1),
                ..Default::default()
            },
            ..Default::default()
        });

        let error_snap = make_snapshot(
            &["execute_command"],
            "Error: permission denied. Cannot write to /etc/hosts.",
            0, 1,
        );

        for _ in 0..4 {
            std::thread::sleep(Duration::from_millis(2));
            monitor.observe(&error_snap);
        }

        assert!(
            matches!(monitor.current_verdict(), Verdict::Stuck { .. }),
            "expected Stuck, got {:?}",
            monitor.current_verdict()
        );
    }

    #[test]
    fn verdict_has_system_message() {
        let stuck = Verdict::Stuck {
            reason: "repeated tool calls".into(),
            streak: 2,
        };
        let msg = stuck.to_system_message();
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("metacognitive_pause"));
    }

    #[test]
    fn on_track_with_varied_progress() {
        let mut monitor = MetacognitiveMonitor::default();
        let snaps = vec![
            make_snapshot(&["read_file"], "file contents: struct Foo { ... }", 1, 1),
            make_snapshot(&["search_files"], "found 5 matches ...", 1, 1),
            make_snapshot(&["read_file"], "impl Foo { fn bar() { ... } }", 1, 1),
            make_snapshot(&["execute_command"], "test passed: 12/12", 1, 1),
        ];
        for snap in &snaps {
            monitor.observe(snap);
        }
        assert!(matches!(monitor.current_verdict(), Verdict::OnTrack));
    }
}
