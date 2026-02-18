//! # Contact Graph — Hawkes-Process Relationship Decay
//!
//! First-class `Contact` domain entity with cross-channel identity resolution
//! and temporal interaction tracking that decays naturally.
//!
//! ## Entity Resolution
//!
//! Uses a union-find (disjoint set) with weighted quick-union and path
//! compression: O(α(n)) amortized per merge/find, where α is the inverse
//! Ackermann function (effectively O(1) for all practical inputs).
//!
//! ## Relationship Health
//!
//! Modeled as a self-exciting Hawkes point process:
//!
//! ```text
//! λ_c(t) = μ_c + Σ_{t_i < t} α · exp(-β(t - t_i))
//! ```
//!
//! Recursive update gives O(1) per new interaction:
//! ```text
//! R_c(t_n) = exp(-β(t_n - t_{n-1})) · (R_c(t_{n-1}) + α)
//! λ_c(t) = μ_c + R_c(t_last) · exp(-β(t - t_last))
//! ```
//!
//! Storage: one f64 accumulator `R_c` per contact.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Contact Entity ──────────────────────────────────────────────────────────

/// A persistent contact entity with cross-channel identity resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    /// Stable contact identifier (UUID)
    pub id: String,
    /// Display name (best guess from available identifiers)
    pub display_name: String,
    /// All known identifiers for this contact across channels
    pub identifiers: Vec<ContactIdentifier>,
    /// Hawkes process accumulator R_c (excitation state)
    pub hawkes_r: f64,
    /// Baseline interaction rate μ_c (estimated via MLE from history)
    pub baseline_rate: f64,
    /// Timestamp of last interaction (needed for O(1) decay computation)
    pub last_interaction: Option<DateTime<Utc>>,
    /// Total interaction count (for baseline rate estimation)
    pub interaction_count: u64,
    /// First interaction timestamp
    pub first_seen: DateTime<Utc>,
    /// Arbitrary metadata (company, role, notes)
    pub metadata: HashMap<String, String>,
    /// Relationship tags (family, coworker, friend, etc.)
    pub tags: Vec<String>,
}

/// A channel-specific identifier for a contact.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContactIdentifier {
    /// Channel type (telegram, email, slack, discord, etc.)
    pub channel: String,
    /// The identifier value (email address, username, phone number, etc.)
    pub value: String,
    /// When this identifier was first observed
    pub observed_at: DateTime<Utc>,
}

/// A recorded interaction between the user and a contact.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interaction {
    /// Contact ID
    pub contact_id: String,
    /// Channel where interaction occurred
    pub channel: String,
    /// Interaction direction
    pub direction: InteractionDirection,
    /// Timestamp
    pub timestamp: DateTime<Utc>,
    /// Optional summary/context
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InteractionDirection {
    Inbound,
    Outbound,
    Mutual,
}

// ── Hawkes Process Parameters ───────────────────────────────────────────────

/// Tuning parameters for the Hawkes process relationship health model.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct HawkesParams {
    /// α — excitation magnitude per interaction (default: 1.0)
    pub alpha: f64,
    /// β — decay rate in inverse-days (default: 0.1 ≈ 10-day half-life)
    pub beta: f64,
    /// Default baseline rate μ for new contacts (interactions/day)
    pub default_baseline: f64,
}

impl Default for HawkesParams {
    fn default() -> Self {
        Self {
            alpha: 1.0,
            beta: 0.1, // 10-day half-life: ln(2)/0.1 ≈ 6.93 days
            default_baseline: 0.05, // ~once per 20 days baseline
        }
    }
}

// ── Core Algorithms ─────────────────────────────────────────────────────────

impl Contact {
    /// Create a new contact with an initial identifier.
    pub fn new(
        id: impl Into<String>,
        display_name: impl Into<String>,
        identifier: ContactIdentifier,
        params: &HawkesParams,
    ) -> Self {
        Self {
            id: id.into(),
            display_name: display_name.into(),
            identifiers: vec![identifier],
            hawkes_r: 0.0,
            baseline_rate: params.default_baseline,
            last_interaction: None,
            interaction_count: 0,
            first_seen: Utc::now(),
            metadata: HashMap::new(),
            tags: Vec::new(),
        }
    }

    /// Record a new interaction. Updates the Hawkes accumulator in O(1).
    ///
    /// ```text
    /// R_c(t_n) = exp(-β(t_n - t_{n-1})) · (R_c(t_{n-1}) + α)
    /// ```
    pub fn record_interaction(&mut self, timestamp: DateTime<Utc>, params: &HawkesParams) {
        if let Some(last) = self.last_interaction {
            let dt_days = (timestamp - last).num_seconds() as f64 / 86400.0;
            if dt_days > 0.0 {
                self.hawkes_r = (-params.beta * dt_days).exp() * (self.hawkes_r + params.alpha);
            }
        } else {
            self.hawkes_r = params.alpha;
        }
        self.last_interaction = Some(timestamp);
        self.interaction_count += 1;

        // Update baseline rate estimate (MLE for Poisson process)
        let total_days = (timestamp - self.first_seen).num_seconds() as f64 / 86400.0;
        if total_days > 1.0 {
            self.baseline_rate = self.interaction_count as f64 / total_days;
        }
    }

    /// Compute current relationship health score in [0, 1].
    ///
    /// ```text
    /// λ_c(t) = μ_c + R_c(t_last) · exp(-β(t - t_last))
    /// health  = λ_c(now) / λ_c_max
    /// ```
    ///
    /// O(1) — one exponential evaluation.
    pub fn health_score(&self, now: DateTime<Utc>, params: &HawkesParams) -> f64 {
        let intensity = self.current_intensity(now, params);
        // Normalize: max intensity occurs right after an interaction
        // λ_max ≈ μ + α (at t = t_last)
        let lambda_max = self.baseline_rate + params.alpha + self.hawkes_r;
        if lambda_max <= 0.0 {
            return 0.0;
        }
        (intensity / lambda_max).clamp(0.0, 1.0)
    }

    /// Current Hawkes intensity λ_c(t).
    pub fn current_intensity(&self, now: DateTime<Utc>, params: &HawkesParams) -> f64 {
        let decay_contribution = if let Some(last) = self.last_interaction {
            let dt_days = (now - last).num_seconds() as f64 / 86400.0;
            if dt_days > 0.0 {
                self.hawkes_r * (-params.beta * dt_days).exp()
            } else {
                self.hawkes_r
            }
        } else {
            0.0
        };
        self.baseline_rate + decay_contribution
    }

    /// Days since last interaction (for stale contact alerts).
    pub fn days_since_interaction(&self, now: DateTime<Utc>) -> Option<f64> {
        self.last_interaction
            .map(|last| (now - last).num_seconds() as f64 / 86400.0)
    }

    /// Merge another identifier into this contact (union operation).
    pub fn merge_identifier(&mut self, id: ContactIdentifier) {
        if !self.identifiers.contains(&id) {
            self.identifiers.push(id);
        }
    }
}

// ── Union-Find for Entity Resolution ────────────────────────────────────────

/// Disjoint set (union-find) for clustering contact identifiers.
///
/// Weighted quick-union with path compression:
/// O(α(n)) amortized per merge/find.
pub struct ContactUnionFind {
    parent: Vec<usize>,
    rank: Vec<usize>,
    /// Maps identifier hash → index in the union-find
    id_to_index: HashMap<String, usize>,
}

impl ContactUnionFind {
    pub fn new() -> Self {
        Self {
            parent: Vec::new(),
            rank: Vec::new(),
            id_to_index: HashMap::new(),
        }
    }

    /// Ensure an identifier has an entry; returns its index.
    pub fn ensure(&mut self, identifier: &str) -> usize {
        if let Some(&idx) = self.id_to_index.get(identifier) {
            return idx;
        }
        let idx = self.parent.len();
        self.parent.push(idx);
        self.rank.push(0);
        self.id_to_index.insert(identifier.to_string(), idx);
        idx
    }

    /// Find the root of the set containing `identifier`.
    pub fn find(&mut self, identifier: &str) -> Option<usize> {
        let idx = *self.id_to_index.get(identifier)?;
        Some(self.find_root(idx))
    }

    fn find_root(&mut self, mut x: usize) -> usize {
        while self.parent[x] != x {
            self.parent[x] = self.parent[self.parent[x]]; // path compression
            x = self.parent[x];
        }
        x
    }

    /// Union two identifiers into the same contact cluster.
    /// Returns true if they were previously in different sets.
    pub fn union(&mut self, a: &str, b: &str) -> bool {
        let ia = self.ensure(a);
        let ib = self.ensure(b);
        let ra = self.find_root(ia);
        let rb = self.find_root(ib);
        if ra == rb {
            return false;
        }
        // Weighted union by rank
        if self.rank[ra] < self.rank[rb] {
            self.parent[ra] = rb;
        } else if self.rank[ra] > self.rank[rb] {
            self.parent[rb] = ra;
        } else {
            self.parent[rb] = ra;
            self.rank[ra] += 1;
        }
        true
    }

    /// Check whether two identifiers are in the same contact cluster.
    pub fn connected(&mut self, a: &str, b: &str) -> bool {
        match (self.find(a), self.find(b)) {
            (Some(ra), Some(rb)) => ra == rb,
            _ => false,
        }
    }

    /// Number of distinct clusters.
    pub fn cluster_count(&mut self) -> usize {
        let n = self.parent.len();
        let mut roots = std::collections::HashSet::new();
        for i in 0..n {
            roots.insert(self.find_root(i));
        }
        roots.len()
    }
}

impl Default for ContactUnionFind {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hawkes_decay() {
        let params = HawkesParams::default();
        let now = Utc::now();
        let id = ContactIdentifier {
            channel: "telegram".into(),
            value: "@alice".into(),
            observed_at: now,
        };
        let mut contact = Contact::new("c1", "Alice", id, &params);

        // Record interactions
        contact.record_interaction(now, &params);
        let h1 = contact.health_score(now, &params);
        assert!(h1 > 0.5, "Health should be high right after interaction");

        // 30 days later with no interaction
        let future = now + chrono::Duration::days(30);
        let h2 = contact.health_score(future, &params);
        assert!(h2 < h1, "Health should decay over time");
    }

    #[test]
    fn union_find_clustering() {
        let mut uf = ContactUnionFind::new();
        uf.ensure("alice@email.com");
        uf.ensure("@alice_tg");
        uf.ensure("bob@email.com");

        assert!(!uf.connected("alice@email.com", "@alice_tg"));
        uf.union("alice@email.com", "@alice_tg");
        assert!(uf.connected("alice@email.com", "@alice_tg"));
        assert!(!uf.connected("alice@email.com", "bob@email.com"));
    }
}
