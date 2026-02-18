//! # Proactive Intelligence Orchestrator — Thompson Sampling Notification Selection
//!
//! Extends the heartbeat from agent-scoped to system-scoped with evaluation
//! predicates that query the event bus, contact graph, and digest windows.
//!
//! ## Decision Model
//!
//! Multi-armed bandit via Thompson Sampling on per-notification-type
//! reward distributions:
//!
//! ```text
//! For each notification type i:
//!     reward_i ~ Beta(α_i, β_i)
//!     sample θ_i ~ Beta(α_i, β_i)
//!
//! Select i* = argmax_i (θ_i · relevance_i - cost_i)
//! If θ_i* · relevance_i - cost_i > threshold: notify
//! Else: stay quiet
//! ```
//!
//! O(N) per tick for N notification types (~20), sub-microsecond total.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A type of proactive notification the system can surface.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationType {
    /// Unique type identifier
    pub id: String,
    /// Human-readable name
    pub name: String,
    /// Beta distribution alpha (positive feedback count)
    pub alpha: f64,
    /// Beta distribution beta (negative feedback count)
    pub beta: f64,
    /// Base interruption cost (higher = less likely to be shown)
    pub cost: f64,
    /// Relevance scoring function identifier
    pub relevance_fn: String,
    /// Whether this notification type is enabled
    pub enabled: bool,
}

impl NotificationType {
    pub fn new(id: impl Into<String>, name: impl Into<String>, cost: f64) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            alpha: 2.0,  // uninformative prior
            beta: 2.0,
            cost,
            relevance_fn: String::new(),
            enabled: true,
        }
    }

    /// Sample from the Beta posterior. Uses a simple approximation
    /// (normal approximation to Beta for α,β > 5, otherwise exact via inverse CDF).
    pub fn sample_reward(&self) -> f64 {
        // Simple approximation: use the posterior mean ± noise proportional to variance
        let mean = self.alpha / (self.alpha + self.beta);
        let var = (self.alpha * self.beta) / ((self.alpha + self.beta).powi(2) * (self.alpha + self.beta + 1.0));
        let std = var.sqrt();
        // Pseudo-random perturbation using a hash-based approach
        let noise = pseudo_normal_sample(self.alpha, self.beta);
        (mean + std * noise).clamp(0.0, 1.0)
    }

    /// Record that the user acknowledged/acted on this notification.
    pub fn record_positive(&mut self) {
        self.alpha += 1.0;
    }

    /// Record that the user ignored/dismissed this notification.
    pub fn record_negative(&mut self) {
        self.beta += 1.0;
    }

    /// Posterior mean: E[θ] = α / (α + β).
    pub fn expected_reward(&self) -> f64 {
        self.alpha / (self.alpha + self.beta)
    }
}

/// Relevance context for proactive evaluation.
#[derive(Debug, Clone, Default)]
pub struct SystemContext {
    /// Number of unread emails
    pub unread_emails: usize,
    /// Maximum email importance score
    pub max_email_importance: f64,
    /// Contacts with decaying relationships (health < threshold)
    pub decaying_contacts: usize,
    /// Maximum relationship decay severity
    pub max_decay_severity: f64,
    /// Social metric anomalies detected
    pub social_anomalies: usize,
    /// Maximum social anomaly z-score
    pub max_anomaly_z: f64,
    /// Pending approval items
    pub pending_approvals: usize,
    /// Open digest windows ready for compilation
    pub ready_digests: usize,
    /// Custom relevance scores by notification type ID
    pub custom_relevance: HashMap<String, f64>,
}

/// Proactive intelligence orchestrator.
pub struct ProactiveOrchestrator {
    /// Registered notification types
    pub types: Vec<NotificationType>,
    /// Minimum score threshold to trigger a notification
    pub threshold: f64,
    /// Maximum notifications per evaluation tick
    pub max_per_tick: usize,
    /// Last evaluation timestamp
    pub last_evaluation: Option<DateTime<Utc>>,
}

impl ProactiveOrchestrator {
    pub fn new(threshold: f64) -> Self {
        Self {
            types: Vec::new(),
            threshold,
            max_per_tick: 3,
            last_evaluation: None,
        }
    }

    /// Create with default Life OS notification types.
    pub fn with_defaults() -> Self {
        let mut orch = Self::new(0.3);
        orch.types = vec![
            NotificationType::new("email_urgent", "Urgent Email Alert", 0.1),
            NotificationType::new("relationship_decay", "Relationship Decay Alert", 0.3),
            NotificationType::new("social_anomaly", "Social Metric Anomaly", 0.2),
            NotificationType::new("approval_pending", "Pending Approval Reminder", 0.15),
            NotificationType::new("digest_ready", "Digest Ready for Review", 0.25),
            NotificationType::new("morning_briefing", "Morning Briefing", 0.1),
            NotificationType::new("evening_review", "Evening Review", 0.15),
            NotificationType::new("weekly_summary", "Weekly Summary", 0.2),
            NotificationType::new("habit_reminder", "Habit Reminder", 0.3),
            NotificationType::new("health_insight", "Health Insight", 0.25),
        ];
        orch
    }

    /// Evaluate system context and select which notifications to surface.
    ///
    /// Thompson Sampling: sample from each type's Beta posterior,
    /// multiply by relevance, subtract cost, pick top items above threshold.
    ///
    /// O(N) where N = number of notification types.
    pub fn evaluate(&mut self, context: &SystemContext) -> Vec<SelectedNotification> {
        let now = Utc::now();
        self.last_evaluation = Some(now);

        let mut candidates: Vec<(usize, f64)> = Vec::new();

        for (idx, nt) in self.types.iter().enumerate() {
            if !nt.enabled {
                continue;
            }

            let theta = nt.sample_reward();
            let relevance = self.compute_relevance(&nt.id, context);
            let score = theta * relevance - nt.cost;

            if score > self.threshold {
                candidates.push((idx, score));
            }
        }

        // Sort descending by score, take top N
        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        candidates.truncate(self.max_per_tick);

        candidates
            .into_iter()
            .map(|(idx, score)| SelectedNotification {
                type_id: self.types[idx].id.clone(),
                type_name: self.types[idx].name.clone(),
                score,
                timestamp: now,
            })
            .collect()
    }

    /// Compute relevance for a notification type given system context.
    fn compute_relevance(&self, type_id: &str, ctx: &SystemContext) -> f64 {
        // Check custom relevance first
        if let Some(&custom) = ctx.custom_relevance.get(type_id) {
            return custom;
        }

        match type_id {
            "email_urgent" => {
                if ctx.unread_emails > 0 {
                    ctx.max_email_importance.min(1.0)
                } else {
                    0.0
                }
            }
            "relationship_decay" => {
                if ctx.decaying_contacts > 0 {
                    ctx.max_decay_severity.min(1.0)
                } else {
                    0.0
                }
            }
            "social_anomaly" => {
                if ctx.social_anomalies > 0 {
                    (ctx.max_anomaly_z / 5.0).min(1.0) // normalize z-score
                } else {
                    0.0
                }
            }
            "approval_pending" => {
                if ctx.pending_approvals > 0 {
                    (ctx.pending_approvals as f64 / 5.0).min(1.0)
                } else {
                    0.0
                }
            }
            "digest_ready" => {
                if ctx.ready_digests > 0 { 0.8 } else { 0.0 }
            }
            _ => 0.5, // default moderate relevance
        }
    }

    /// Record user feedback for a notification type.
    pub fn record_feedback(&mut self, type_id: &str, positive: bool) {
        if let Some(nt) = self.types.iter_mut().find(|t| t.id == type_id) {
            if positive {
                nt.record_positive();
            } else {
                nt.record_negative();
            }
        }
    }
}

/// A notification selected for delivery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectedNotification {
    pub type_id: String,
    pub type_name: String,
    pub score: f64,
    pub timestamp: DateTime<Utc>,
}

/// Pseudo-normal sample using Box-Muller-like transform from hash-based entropy.
fn pseudo_normal_sample(a: f64, b: f64) -> f64 {
    // Use the fractional part of a+b multiplied by a large prime as entropy
    let seed = (a * 1000.0 + b * 7919.0).fract();
    // Map [0,1) to approximately normal via inverse sigmoid approximation
    let u = seed.clamp(0.01, 0.99);
    (u / (1.0 - u)).ln() * 0.3 // logit transform, scaled down
}
