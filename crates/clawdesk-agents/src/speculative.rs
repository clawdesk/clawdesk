//! Speculative Parallel Execution with Branch Prediction.
//!
//! Implements 2-bit saturating branch predictor for Router pipeline nodes:
//!
//! ```text
//! States: StronglyNotTaken → WeaklyNotTaken → WeaklyTaken → StronglyTaken
//! ```
//!
//! When a Router step is about to execute:
//! 1. **Predict** the most likely branch using the branch predictor.
//! 2. **Speculatively execute** the predicted branch in parallel with
//!    condition evaluation.
//! 3. **On hit**: reuse the speculative result (zero extra latency).
//! 4. **On miss**: discard speculative result, execute correct branch.
//!
//! This trades compute for latency — when predictions are accurate (>60%),
//! Router steps effectively have zero overhead.
//!
//! ## Expected speedup
//!
//! ```text
//! speedup = 1 / (1 - p_hit + p_hit × t_condition / t_branch)
//! ```
//!
//! With p_hit=0.7, t_condition=0.2s, t_branch=2s: ~1.56× speedup.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{debug, info};

// ───────────────────────────────────────────────────────────────
// 2-bit Branch Predictor
// ───────────────────────────────────────────────────────────────

/// 2-bit saturating counter state.
///
/// ```text
/// 00 = StronglyNotTaken (predict "not taken" = route 0)
/// 01 = WeaklyNotTaken   (predict "not taken" = route 0)
/// 10 = WeaklyTaken      (predict "taken" = route 1)
/// 11 = StronglyTaken    (predict "taken" = route 1)
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum PredictorState {
    StronglyNotTaken = 0,
    WeaklyNotTaken = 1,
    WeaklyTaken = 2,
    StronglyTaken = 3,
}

impl PredictorState {
    /// Prediction: which route index to speculatively execute.
    pub fn predicted_route(self) -> usize {
        match self {
            PredictorState::StronglyNotTaken | PredictorState::WeaklyNotTaken => 0,
            PredictorState::WeaklyTaken | PredictorState::StronglyTaken => 1,
        }
    }

    /// Update after observing the actual outcome.
    pub fn update(self, actual_taken: bool) -> Self {
        if actual_taken {
            match self {
                PredictorState::StronglyNotTaken => PredictorState::WeaklyNotTaken,
                PredictorState::WeaklyNotTaken => PredictorState::WeaklyTaken,
                PredictorState::WeaklyTaken => PredictorState::StronglyTaken,
                PredictorState::StronglyTaken => PredictorState::StronglyTaken,
            }
        } else {
            match self {
                PredictorState::StronglyNotTaken => PredictorState::StronglyNotTaken,
                PredictorState::WeaklyNotTaken => PredictorState::StronglyNotTaken,
                PredictorState::WeaklyTaken => PredictorState::WeaklyNotTaken,
                PredictorState::StronglyTaken => PredictorState::WeaklyTaken,
            }
        }
    }

    pub fn from_u8(v: u8) -> Self {
        match v & 0x03 {
            0 => PredictorState::StronglyNotTaken,
            1 => PredictorState::WeaklyNotTaken,
            2 => PredictorState::WeaklyTaken,
            _ => PredictorState::StronglyTaken,
        }
    }
}

// ───────────────────────────────────────────────────────────────
// N-way Branch Predictor
// ───────────────────────────────────────────────────────────────

/// N-way branch predictor for Router nodes with more than 2 routes.
///
/// Tracks a frequency counter per route and predicts the most frequent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NWayPredictor {
    /// Frequency counts per route index.
    pub counts: Vec<u64>,
    /// Total predictions made.
    pub total: u64,
    /// Total correct predictions.
    pub correct: u64,
}

impl NWayPredictor {
    pub fn new(num_routes: usize) -> Self {
        Self {
            counts: vec![0; num_routes],
            total: 0,
            correct: 0,
        }
    }

    /// Predict the most likely route index.
    pub fn predict(&self) -> usize {
        self.counts
            .iter()
            .enumerate()
            .max_by_key(|(_, &count)| count)
            .map(|(idx, _)| idx)
            .unwrap_or(0)
    }

    /// Update with the actual outcome.
    pub fn update(&mut self, actual_route: usize) {
        let predicted = self.predict();
        self.total += 1;
        if predicted == actual_route {
            self.correct += 1;
        }
        if actual_route < self.counts.len() {
            self.counts[actual_route] += 1;
        }
    }

    /// Prediction accuracy so far.
    pub fn accuracy(&self) -> f64 {
        if self.total == 0 {
            0.5 // No data → neutral.
        } else {
            self.correct as f64 / self.total as f64
        }
    }
}

// ───────────────────────────────────────────────────────────────
// Branch Predictor Table
// ───────────────────────────────────────────────────────────────

/// A table of branch predictors, one per Router step.
///
/// Thread-safe: binary predictors use `AtomicU8` (wait-free predict,
/// CAS-loop update). N-way predictors use `RwLock` with per-entry scope.
pub struct BranchPredictorTable {
    /// 2-bit predictors for binary Router steps (2 routes).
    /// Value is a `PredictorState` encoded as u8 (0-3).
    binary: std::sync::RwLock<HashMap<String, AtomicU8>>,
    /// N-way predictors for Router steps with >2 routes.
    nway: std::sync::RwLock<HashMap<String, NWayPredictor>>,
    /// Global statistics.
    total_predictions: AtomicU64,
    total_hits: AtomicU64,
}

impl BranchPredictorTable {
    pub fn new() -> Self {
        Self {
            binary: std::sync::RwLock::new(HashMap::new()),
            nway: std::sync::RwLock::new(HashMap::new()),
            total_predictions: AtomicU64::new(0),
            total_hits: AtomicU64::new(0),
        }
    }

    /// Predict the route for a Router step.
    ///
    /// `step_key` is a unique identifier for the Router step (e.g., pipeline_id:step_idx).
    /// `num_routes` is the number of routes the Router has.
    pub fn predict(&self, step_key: &str, num_routes: usize) -> usize {
        if num_routes <= 2 {
            // Wait-free: single atomic load
            let predictors = self.binary.read().unwrap();
            predictors
                .get(step_key)
                .map(|atom| PredictorState::from_u8(atom.load(Ordering::Relaxed)).predicted_route())
                .unwrap_or(0)
        } else {
            let predictors = self.nway.read().unwrap();
            predictors
                .get(step_key)
                .map(|p| p.predict())
                .unwrap_or(0)
        }
    }

    /// Update the predictor after observing the actual route.
    pub fn record_outcome(&self, step_key: &str, actual_route: usize, num_routes: usize) {
        let prediction = self.predict(step_key, num_routes);
        self.total_predictions.fetch_add(1, Ordering::Relaxed);
        if prediction == actual_route {
            self.total_hits.fetch_add(1, Ordering::Relaxed);
        }

        if num_routes <= 2 {
            // Try CAS-loop update under read lock first (common path).
            {
                let predictors = self.binary.read().unwrap();
                if let Some(atom) = predictors.get(step_key) {
                    loop {
                        let old = atom.load(Ordering::Relaxed);
                        let state = PredictorState::from_u8(old);
                        let new_state = state.update(actual_route > 0);
                        match atom.compare_exchange_weak(
                            old, new_state as u8,
                            Ordering::Relaxed, Ordering::Relaxed,
                        ) {
                            Ok(_) => break,
                            Err(_) => continue,
                        }
                    }
                    debug!(
                        step_key,
                        predicted = prediction,
                        actual = actual_route,
                        hit = prediction == actual_route,
                        "branch predictor update"
                    );
                    return;
                }
            }
            // Key doesn't exist yet — take write lock to insert.
            let mut predictors = self.binary.write().unwrap();
            let atom = predictors
                .entry(step_key.to_string())
                .or_insert_with(|| AtomicU8::new(PredictorState::WeaklyNotTaken as u8));
            let old = atom.load(Ordering::Relaxed);
            let new_state = PredictorState::from_u8(old).update(actual_route > 0);
            atom.store(new_state as u8, Ordering::Relaxed);
        } else {
            let mut predictors = self.nway.write().unwrap();
            let predictor = predictors
                .entry(step_key.to_string())
                .or_insert_with(|| NWayPredictor::new(num_routes));
            predictor.update(actual_route);
        }

        debug!(
            step_key,
            predicted = prediction,
            actual = actual_route,
            hit = prediction == actual_route,
            "branch predictor update"
        );
    }

    /// Global prediction accuracy.
    pub fn accuracy(&self) -> f64 {
        let total = self.total_predictions.load(Ordering::Relaxed);
        let hits = self.total_hits.load(Ordering::Relaxed);
        if total == 0 {
            0.5
        } else {
            hits as f64 / total as f64
        }
    }

    /// Whether speculation should be enabled for a given step.
    ///
    /// Disabled if accuracy is below 50% (worse than random).
    pub fn should_speculate(&self, step_key: &str, num_routes: usize) -> bool {
        if num_routes <= 2 {
            // Always speculate for binary — the 2-bit predictor converges fast.
            true
        } else {
            let predictors = self.nway.read().unwrap();
            predictors
                .get(step_key)
                .map(|p| p.accuracy() > 0.5)
                .unwrap_or(true) // Speculate by default until we have data.
        }
    }

    /// Number of tracked Router steps.
    pub fn tracked_steps(&self) -> usize {
        let binary_count = self.binary.read().map(|m| m.len()).unwrap_or(0);
        let nway_count = self.nway.read().map(|m| m.len()).unwrap_or(0);
        binary_count + nway_count
    }
}

impl Default for BranchPredictorTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Outcome of a speculative execution.
#[derive(Debug, Clone)]
pub struct SpeculativeResult {
    /// The predicted route index.
    pub predicted_route: usize,
    /// The actual route index (after condition evaluation).
    pub actual_route: usize,
    /// Whether the prediction was correct.
    pub hit: bool,
    /// The speculative execution result (if hit, this is the final result).
    pub speculative_output: Option<String>,
}

// ───────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_2bit_predictor_converges() {
        let mut state = PredictorState::StronglyNotTaken;

        // Repeatedly observe "taken" → should converge to StronglyTaken.
        state = state.update(true); // SN → WN
        assert_eq!(state, PredictorState::WeaklyNotTaken);
        state = state.update(true); // WN → WT
        assert_eq!(state, PredictorState::WeaklyTaken);
        state = state.update(true); // WT → ST
        assert_eq!(state, PredictorState::StronglyTaken);
        state = state.update(true); // ST → ST (saturated)
        assert_eq!(state, PredictorState::StronglyTaken);
    }

    #[test]
    fn test_2bit_predictor_misprediction() {
        let state = PredictorState::StronglyTaken;
        // One misprediction doesn't flip the prediction.
        let state = state.update(false); // ST → WT
        assert_eq!(state, PredictorState::WeaklyTaken);
        assert_eq!(state.predicted_route(), 1); // Still predicts "taken".
    }

    #[test]
    fn test_nway_predictor() {
        let mut pred = NWayPredictor::new(4);
        for _ in 0..10 {
            pred.update(2);
        }
        for _ in 0..3 {
            pred.update(0);
        }
        assert_eq!(pred.predict(), 2);
        assert!(pred.accuracy() > 0.5);
    }

    #[test]
    fn test_predictor_table() {
        let table = BranchPredictorTable::new();

        // Binary Router.
        for _ in 0..10 {
            table.record_outcome("pipeline:step3", 1, 2);
        }
        assert_eq!(table.predict("pipeline:step3", 2), 1);

        // N-way Router.
        for _ in 0..10 {
            table.record_outcome("pipeline:step7", 2, 5);
        }
        assert_eq!(table.predict("pipeline:step7", 5), 2);

        assert!(table.accuracy() > 0.3);
    }

    #[test]
    fn test_speculation_disabled_for_poor_accuracy() {
        let table = BranchPredictorTable::new();

        let mut pred = NWayPredictor::new(3);
        // Record many misses.
        pred.total = 100;
        pred.correct = 20; // 20% accuracy.
        pred.counts = vec![40, 30, 30];

        {
            let mut nway = table.nway.write().unwrap();
            nway.insert("bad_step".into(), pred);
        }

        assert!(!table.should_speculate("bad_step", 3));
    }
}
