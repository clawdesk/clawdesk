//! Harness-aware task routing with contextual LinUCB scoring.

use crate::harness::HarnessKind;
use aho_corasick::AhoCorasick;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::OnceLock;

// ── Static Aho-Corasick automata (built once) ──────────────

static CODING_AC: OnceLock<AhoCorasick> = OnceLock::new();
static RESEARCH_AC: OnceLock<AhoCorasick> = OnceLock::new();
static ACTION_AC: OnceLock<AhoCorasick> = OnceLock::new();

fn coding_ac() -> &'static AhoCorasick {
    CODING_AC.get_or_init(|| {
        AhoCorasick::new([
            "refactor",
            "implement",
            "fix",
            "debug",
            "compile",
            "build",
            "cargo",
            "pytest",
            "typescript",
            "rust",
            "python",
            "review",
            "pr",
        ])
        .expect("valid coding patterns")
    })
}

fn research_ac() -> &'static AhoCorasick {
    RESEARCH_AC.get_or_init(|| {
        AhoCorasick::new([
            "research",
            "summarize",
            "compare",
            "analyze",
            "investigate",
            "brief",
            "report",
            "document",
        ])
        .expect("valid research patterns")
    })
}

fn action_ac() -> &'static AhoCorasick {
    ACTION_AC.get_or_init(|| {
        AhoCorasick::new([
            "create",
            "write",
            "edit",
            "run",
            "execute",
            "refactor",
            "fix",
            "send",
        ])
        .expect("valid action patterns")
    })
}

/// Execution target.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ExecutionPath {
    ApiProvider { provider: String, model: String },
    Harness(HarnessKind),
    Native,
}

/// Candidate route with estimated metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingCandidate {
    /// Stable key for online learning updates.
    pub key: String,
    pub path: ExecutionPath,
    /// Higher is better.
    pub estimated_quality: f64,
    /// Lower is better.
    pub estimated_cost_usd: f64,
    /// Lower is better.
    pub estimated_latency_ms: f64,
}

/// Feature extraction output for routing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskFeatures {
    pub estimated_tokens: usize,
    pub is_coding: bool,
    pub is_research: bool,
    pub is_simple_question: bool,
    pub mentions_files: bool,
    pub workspace_size: usize,
    pub has_refactor_intent: bool,
    pub has_test_intent: bool,
    pub has_debug_intent: bool,
    pub has_review_intent: bool,
}

impl TaskFeatures {
    /// Extract lightweight routing features from task text.
    pub fn from_task_text(task: &str, workspace_size: Option<usize>) -> Self {
        let lower = task.to_ascii_lowercase();
        let token_est = std::cmp::max(1, lower.split_whitespace().count()) * 4 / 3;

        let mentions_files =
            lower.contains('/') || lower.contains(".rs") || lower.contains(".ts") || lower.contains(".py");
        let is_coding = coding_ac().find(&lower).is_some();
        let is_research = research_ac().find(&lower).is_some();
        let has_action = action_ac().find(&lower).is_some();
        let short_prompt = lower.split_whitespace().count() < 20;
        let is_simple_question = short_prompt && !has_action && !is_coding && !is_research;

        Self {
            estimated_tokens: token_est,
            is_coding,
            is_research,
            is_simple_question,
            mentions_files,
            workspace_size: workspace_size.unwrap_or(0),
            has_refactor_intent: lower.contains("refactor"),
            has_test_intent: lower.contains("test"),
            has_debug_intent: lower.contains("debug"),
            has_review_intent: lower.contains("review"),
        }
    }

    fn to_vector(&self) -> Vec<f64> {
        vec![
            self.estimated_tokens as f64,
            boolf(self.is_coding),
            boolf(self.is_research),
            boolf(self.is_simple_question),
            boolf(self.mentions_files),
            self.workspace_size as f64,
            boolf(self.has_refactor_intent),
            boolf(self.has_test_intent),
            boolf(self.has_debug_intent),
            boolf(self.has_review_intent),
            1.0, // bias term
        ]
    }
}

fn boolf(v: bool) -> f64 {
    if v { 1.0 } else { 0.0 }
}

/// Scalarization weights.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct RoutingWeights {
    pub quality: f64,
    pub cost: f64,
    pub latency: f64,
}

impl Default for RoutingWeights {
    fn default() -> Self {
        Self {
            quality: 0.6,
            cost: 0.2,
            latency: 0.2,
        }
    }
}

/// Router decision details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingDecision {
    pub selected_key: String,
    pub selected_path: ExecutionPath,
    pub score: f64,
    pub normalized_quality: f64,
    pub normalized_cost: f64,
    pub normalized_latency: f64,
}

/// Feature vector dimension — must match `TaskFeatures::to_vector().len()`.
const LINUCB_D: usize = 11;

#[derive(Debug, Clone)]
struct LinUcbArm {
    /// A matrix stored as flat row-major [f64; D*D].
    a: [f64; LINUCB_D * LINUCB_D],
    /// Cached inverse of A — maintained via Sherman-Morrison on each update.
    a_inv: [f64; LINUCB_D * LINUCB_D],
    b: [f64; LINUCB_D],
}

impl LinUcbArm {
    fn new(d: usize) -> Self {
        debug_assert_eq!(d, LINUCB_D, "dimension mismatch: expected {LINUCB_D}, got {d}");
        let mut a = [0.0; LINUCB_D * LINUCB_D];
        let mut a_inv = [0.0; LINUCB_D * LINUCB_D];
        for i in 0..LINUCB_D {
            a[i * LINUCB_D + i] = 1.0;
            a_inv[i * LINUCB_D + i] = 1.0;
        }
        Self { a, a_inv, b: [0.0; LINUCB_D] }
    }

    /// Update arm with observation (x, reward).
    ///
    /// A_new = A_old + x * x^T  →  updates A in O(d²)
    /// A_inv_new via Sherman-Morrison: A_inv - (A_inv * x)(x^T * A_inv) / (1 + x^T * A_inv * x)
    fn update(&mut self, x: &[f64], reward: f64) {
        debug_assert_eq!(x.len(), LINUCB_D);
        // Update b: b += reward * x
        for i in 0..LINUCB_D {
            self.b[i] += reward * x[i];
        }
        // Update A: A += x * x^T
        for i in 0..LINUCB_D {
            for j in 0..LINUCB_D {
                self.a[i * LINUCB_D + j] += x[i] * x[j];
            }
        }
        // Sherman-Morrison update for A_inv:
        // u = A_inv * x
        let u = mat_vec_fixed(&self.a_inv, x);
        // denom = 1 + x^T * u
        let denom = 1.0 + dot(x, &u);
        if denom.abs() < 1e-12 {
            return;
        }
        let inv_denom = 1.0 / denom;
        // A_inv -= (u * u^T) / denom
        for i in 0..LINUCB_D {
            for j in 0..LINUCB_D {
                self.a_inv[i * LINUCB_D + j] -= u[i] * u[j] * inv_denom;
            }
        }
    }

    /// Predict reward and UCB bonus using cached A_inv.
    ///
    /// theta = A_inv * b  →  O(d²) matrix-vector multiply
    /// bonus = alpha * sqrt(x^T * A_inv * x)  →  O(d²)
    fn predict_and_bonus(&self, x: &[f64], alpha: f64) -> (f64, f64) {
        debug_assert_eq!(x.len(), LINUCB_D);
        let theta = mat_vec_fixed(&self.a_inv, &self.b);
        let pred = dot(&theta, x);
        let z = mat_vec_fixed(&self.a_inv, x);
        let quad = dot(x, &z).max(0.0);
        let bonus = alpha * quad.sqrt();
        (pred, bonus)
    }
}

/// Task router with contextual online learning.
pub struct TaskRouter {
    alpha: f64,
    weights: RoutingWeights,
    dims: usize,
    arms: HashMap<String, LinUcbArm>,
    /// Total feedback observations for alpha decay schedule.
    total_feedback_count: u64,
}

impl TaskRouter {
    pub fn new(alpha: f64, weights: RoutingWeights) -> Self {
        // Keep in sync with TaskFeatures::to_vector.
        let dims = 11;
        Self {
            alpha,
            weights,
            dims,
            arms: HashMap::new(),
            total_feedback_count: 0,
        }
    }

    /// Compute the effective alpha with Lai-Robbins decay: alpha / (1 + 0.1 * sqrt(t)).
    fn effective_alpha(&self) -> f64 {
        self.alpha / (1.0 + 0.1 * (self.total_feedback_count as f64).sqrt())
    }

    /// Select best route from a candidate set.
    pub fn select(
        &mut self,
        features: &TaskFeatures,
        candidates: &[RoutingCandidate],
    ) -> Option<RoutingDecision> {
        if candidates.is_empty() {
            return None;
        }
        let x = features.to_vector();

        let qualities: Vec<f64> = candidates.iter().map(|c| c.estimated_quality).collect();
        let costs: Vec<f64> = candidates.iter().map(|c| c.estimated_cost_usd).collect();
        let latencies: Vec<f64> = candidates.iter().map(|c| c.estimated_latency_ms).collect();

        let (q_min, q_max) = min_max(&qualities);
        let (c_min, c_max) = min_max(&costs);
        let (l_min, l_max) = min_max(&latencies);

        let alpha = self.effective_alpha();
        let mut best: Option<RoutingDecision> = None;
        for c in candidates {
            let arm = self
                .arms
                .entry(c.key.clone())
                .or_insert_with(|| LinUcbArm::new(self.dims));

            let (pred, bonus) = arm.predict_and_bonus(&x, alpha);
            let q = normalize(c.estimated_quality, q_min, q_max);
            let c_norm = normalize(c.estimated_cost_usd, c_min, c_max);
            let l_norm = normalize(c.estimated_latency_ms, l_min, l_max);

            let score = (self.weights.quality * q)
                - (self.weights.cost * c_norm)
                - (self.weights.latency * l_norm)
                + pred
                + bonus;

            let decision = RoutingDecision {
                selected_key: c.key.clone(),
                selected_path: c.path.clone(),
                score,
                normalized_quality: q,
                normalized_cost: c_norm,
                normalized_latency: l_norm,
            };

            if let Some(current) = &best {
                if decision.score > current.score {
                    best = Some(decision);
                }
            } else {
                best = Some(decision);
            }
        }
        best
    }

    /// Record reward feedback for a selected route.
    pub fn record_feedback(
        &mut self,
        selected_key: &str,
        features: &TaskFeatures,
        reward: f64,
    ) -> Result<(), String> {
        let x = features.to_vector();
        debug_assert_eq!(
            x.len(), self.dims,
            "feature vector dimension mismatch: expected {}, got {}",
            self.dims, x.len()
        );
        let arm = self
            .arms
            .entry(selected_key.to_string())
            .or_insert_with(|| LinUcbArm::new(self.dims));
        arm.update(&x, reward);
        self.total_feedback_count += 1;
        Ok(())
    }
}

fn min_max(xs: &[f64]) -> (f64, f64) {
    let mut min = f64::INFINITY;
    let mut max = f64::NEG_INFINITY;
    for x in xs {
        min = min.min(*x);
        max = max.max(*x);
    }
    if !min.is_finite() || !max.is_finite() {
        (0.0, 1.0)
    } else {
        (min, max)
    }
}

fn normalize(v: f64, min: f64, max: f64) -> f64 {
    if (max - min).abs() < f64::EPSILON {
        0.5
    } else {
        (v - min) / (max - min)
    }
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Stack-allocated matrix-vector multiply for flat row-major [f64; D*D].
fn mat_vec_fixed(m: &[f64; LINUCB_D * LINUCB_D], v: &[f64]) -> [f64; LINUCB_D] {
    let mut result = [0.0; LINUCB_D];
    for i in 0..LINUCB_D {
        let mut sum = 0.0;
        for j in 0..LINUCB_D {
            sum += m[i * LINUCB_D + j] * v[j];
        }
        result[i] = sum;
    }
    result
}

/// Matrix-vector multiply for Vec-based matrices (used by solve_linear).
fn mat_vec(m: &[Vec<f64>], v: &[f64]) -> Vec<f64> {
    m.iter().map(|row| dot(row, v)).collect()
}

/// Solve A x = b via Gaussian elimination with partial pivoting.
#[allow(dead_code)]
fn solve_linear(a: &[Vec<f64>], b: &[f64]) -> Option<Vec<f64>> {
    let n = b.len();
    if a.len() != n || a.iter().any(|row| row.len() != n) {
        return None;
    }

    let mut aug = vec![vec![0.0; n + 1]; n];
    for i in 0..n {
        for j in 0..n {
            aug[i][j] = a[i][j];
        }
        aug[i][n] = b[i];
    }

    for col in 0..n {
        let mut pivot = col;
        let mut best = aug[col][col].abs();
        for row in (col + 1)..n {
            let val = aug[row][col].abs();
            if val > best {
                best = val;
                pivot = row;
            }
        }
        if best < 1e-12 {
            return None;
        }
        if pivot != col {
            aug.swap(pivot, col);
        }

        let pivot_val = aug[col][col];
        for j in col..=n {
            aug[col][j] /= pivot_val;
        }

        for row in 0..n {
            if row == col {
                continue;
            }
            let factor = aug[row][col];
            if factor.abs() < 1e-12 {
                continue;
            }
            for j in col..=n {
                aug[row][j] -= factor * aug[col][j];
            }
        }
    }

    Some(aug.into_iter().map(|row| row[n]).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn features_detect_coding_task() {
        let f = TaskFeatures::from_task_text(
            "Refactor auth.rs and add unit tests for JWT parser",
            Some(500),
        );
        assert!(f.is_coding);
        assert!(f.has_refactor_intent);
        assert!(f.has_test_intent);
        assert!(f.mentions_files);
        assert_eq!(f.workspace_size, 500);
    }

    #[test]
    fn router_selects_high_quality_on_equal_cost_latency() {
        let mut router = TaskRouter::new(0.2, RoutingWeights::default());
        let features = TaskFeatures::from_task_text("debug failing rust tests", Some(2000));
        let candidates = vec![
            RoutingCandidate {
                key: "api_sonnet".into(),
                path: ExecutionPath::ApiProvider {
                    provider: "anthropic".into(),
                    model: "sonnet".into(),
                },
                estimated_quality: 0.7,
                estimated_cost_usd: 0.02,
                estimated_latency_ms: 2000.0,
            },
            RoutingCandidate {
                key: "claude_code".into(),
                path: ExecutionPath::Harness(HarnessKind::ClaudeCode),
                estimated_quality: 0.9,
                estimated_cost_usd: 0.02,
                estimated_latency_ms: 2000.0,
            },
        ];

        let decision = router.select(&features, &candidates).expect("decision");
        assert_eq!(decision.selected_key, "claude_code");
    }

    #[test]
    fn router_learns_feedback() {
        let mut router = TaskRouter::new(0.1, RoutingWeights::default());
        let features = TaskFeatures::from_task_text("implement feature flag rollout", Some(3000));
        let candidates = vec![
            RoutingCandidate {
                key: "api".into(),
                path: ExecutionPath::ApiProvider {
                    provider: "anthropic".into(),
                    model: "haiku".into(),
                },
                estimated_quality: 0.5,
                estimated_cost_usd: 0.005,
                estimated_latency_ms: 500.0,
            },
            RoutingCandidate {
                key: "codex".into(),
                path: ExecutionPath::Harness(HarnessKind::CodexCli),
                estimated_quality: 0.5,
                estimated_cost_usd: 0.005,
                estimated_latency_ms: 500.0,
            },
        ];

        // Train codex arm with positive rewards.
        for _ in 0..20 {
            router.record_feedback("codex", &features, 1.0).unwrap();
            router.record_feedback("api", &features, 0.1).unwrap();
        }
        let decision = router.select(&features, &candidates).expect("decision");
        assert_eq!(decision.selected_key, "codex");
    }
}

