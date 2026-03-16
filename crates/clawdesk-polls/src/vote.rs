//! Vote counting with G-Counter CRDT for eventual consistency.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A single ballot cast by a voter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ballot {
    pub voter_id: String,
    pub selections: Vec<usize>,
    pub timestamp: String,
    pub channel_instance: String,
}

/// G-Counter (grow-only counter) per option — CRDT for vote aggregation.
///
/// Merge: `V[i] = max(V_local[i], V_remote[i])`
/// Properties: commutative, associative, idempotent → eventual consistency.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VoteCounter {
    /// instance_id → per-option counts.
    counters: HashMap<String, Vec<u64>>,
    option_count: usize,
}

impl VoteCounter {
    pub fn new(option_count: usize) -> Self {
        Self { counters: HashMap::new(), option_count }
    }

    /// Record a vote from a specific channel instance.
    pub fn increment(&mut self, instance_id: &str, option_index: usize) {
        let counts = self.counters
            .entry(instance_id.to_string())
            .or_insert_with(|| vec![0u64; self.option_count]);
        if option_index < counts.len() {
            counts[option_index] += 1;
        }
    }

    /// Merge with a remote counter (CRDT merge).
    pub fn merge(&mut self, other: &VoteCounter) {
        for (instance, remote_counts) in &other.counters {
            let local = self.counters
                .entry(instance.clone())
                .or_insert_with(|| vec![0u64; self.option_count]);
            for (i, &remote_val) in remote_counts.iter().enumerate() {
                if i < local.len() {
                    local[i] = local[i].max(remote_val);
                }
            }
        }
    }

    /// Get total vote count for an option across all instances.
    pub fn total(&self, option_index: usize) -> u64 {
        self.counters.values()
            .map(|counts| counts.get(option_index).copied().unwrap_or(0))
            .sum()
    }

    /// Get tally across all options.
    pub fn tally(&self) -> VoteTally {
        let totals: Vec<u64> = (0..self.option_count)
            .map(|i| self.total(i))
            .collect();
        let grand_total: u64 = totals.iter().sum();
        let percentages: Vec<f64> = totals.iter()
            .map(|&t| if grand_total > 0 { t as f64 / grand_total as f64 * 100.0 } else { 0.0 })
            .collect();
        VoteTally { counts: totals, percentages, total_votes: grand_total }
    }
}

/// Aggregated vote tally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoteTally {
    pub counts: Vec<u64>,
    pub percentages: Vec<f64>,
    pub total_votes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_voting() {
        let mut counter = VoteCounter::new(3);
        counter.increment("discord", 0);
        counter.increment("discord", 0);
        counter.increment("slack", 1);
        assert_eq!(counter.total(0), 2);
        assert_eq!(counter.total(1), 1);
        assert_eq!(counter.total(2), 0);
    }

    #[test]
    fn crdt_merge_takes_max() {
        let mut a = VoteCounter::new(2);
        a.increment("inst1", 0); // inst1: [1, 0]
        a.increment("inst1", 0); // inst1: [2, 0]

        let mut b = VoteCounter::new(2);
        b.increment("inst1", 0); // inst1: [1, 0]
        b.increment("inst1", 1); // inst1: [1, 1]

        a.merge(&b);
        // After merge: inst1: [max(2,1), max(0,1)] = [2, 1]
        assert_eq!(a.total(0), 2);
        assert_eq!(a.total(1), 1);
    }

    #[test]
    fn crdt_merge_is_idempotent() {
        let mut a = VoteCounter::new(2);
        a.increment("inst1", 0);
        let b = a.clone();
        a.merge(&b);
        a.merge(&b); // merge twice — should be same
        assert_eq!(a.total(0), 1);
    }

    #[test]
    fn tally_percentages() {
        let mut counter = VoteCounter::new(2);
        counter.increment("inst", 0);
        counter.increment("inst", 0);
        counter.increment("inst", 0);
        counter.increment("inst", 1);
        let tally = counter.tally();
        assert_eq!(tally.total_votes, 4);
        assert!((tally.percentages[0] - 75.0).abs() < 0.1);
        assert!((tally.percentages[1] - 25.0).abs() < 0.1);
    }
}
