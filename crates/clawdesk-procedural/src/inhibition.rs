//! Contextual inhibition — suppressing actions that consistently fail.
//!
//! Unlike a hard deny-list, inhibition is:
//! - **Context-sensitive**: `rm -rf` might be fine in a temp dir but not in `/etc`
//! - **Temporally decaying**: old failures become less relevant
//! - **Overridable**: if the LLM is highly confident, it can override suppression

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A suppressed action pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InhibitedAction {
    /// The tool that's being suppressed.
    pub tool_name: String,
    /// Context keywords where this suppression applies.
    pub context_keywords: Vec<String>,
    /// Number of consecutive failures that built this inhibition.
    pub failure_count: u32,
    /// When the most recent failure occurred.
    pub last_failure: DateTime<Utc>,
    /// Current suppression strength (0.0–1.0). Decays over time.
    pub suppression_strength: f64,
    /// Human-readable reason for suppression.
    pub reason: String,
}

impl InhibitedAction {
    /// Compute current suppression strength with temporal decay.
    pub fn current_strength(&self, half_life_days: f64) -> f64 {
        let days_since = (Utc::now() - self.last_failure).num_hours() as f64 / 24.0;
        let decay = (-days_since * (2.0_f64.ln()) / half_life_days).exp();
        self.suppression_strength * decay
    }
}

/// The inhibition gate — sits before tool execution and filters
/// actions that have a history of failure in the current context.
pub struct InhibitionGate {
    /// All known inhibitions, keyed by tool name.
    inhibitions: HashMap<String, Vec<InhibitedAction>>,
    /// Confidence threshold needed to override suppression.
    pub override_threshold: f64,
    /// Half-life for suppression decay (in days).
    pub decay_half_life_days: f64,
    /// Minimum strength to actually suppress (below this, inhibition expires).
    pub min_active_strength: f64,
}

impl InhibitionGate {
    pub fn new() -> Self {
        Self {
            inhibitions: HashMap::new(),
            override_threshold: 0.9,
            decay_half_life_days: 14.0,
            min_active_strength: 0.1,
        }
    }

    /// Record a tool failure in a specific context.
    pub fn record_failure(
        &mut self,
        tool_name: &str,
        context_keywords: &[String],
        reason: &str,
    ) {
        let entries = self.inhibitions.entry(tool_name.to_string()).or_default();

        // Find existing inhibition for this context
        let matching = entries.iter_mut().find(|e| {
            keyword_overlap(&e.context_keywords, context_keywords) >= 0.5
        });

        if let Some(existing) = matching {
            existing.failure_count += 1;
            existing.last_failure = Utc::now();
            // Strengthen suppression (capped at 1.0)
            existing.suppression_strength = (existing.suppression_strength + 0.2).min(1.0);
            existing.reason = reason.to_string();
        } else {
            entries.push(InhibitedAction {
                tool_name: tool_name.to_string(),
                context_keywords: context_keywords.to_vec(),
                failure_count: 1,
                last_failure: Utc::now(),
                suppression_strength: 0.3, // initial suppression after first failure
                reason: reason.to_string(),
            });
        }
    }

    /// Record a tool success — weakens any existing inhibition.
    pub fn record_success(&mut self, tool_name: &str, context_keywords: &[String]) {
        if let Some(entries) = self.inhibitions.get_mut(tool_name) {
            for entry in entries.iter_mut() {
                if keyword_overlap(&entry.context_keywords, context_keywords) >= 0.5 {
                    entry.suppression_strength = (entry.suppression_strength - 0.15).max(0.0);
                }
            }
        }
    }

    /// Check if a tool should be suppressed in the current context.
    /// Returns the inhibited action if suppression is active.
    pub fn check(&self, tool_name: &str, context_keywords: &[String]) -> Option<&InhibitedAction> {
        let entries = self.inhibitions.get(tool_name)?;
        entries.iter().find(|e| {
            let overlap = keyword_overlap(&e.context_keywords, context_keywords);
            let strength = e.current_strength(self.decay_half_life_days);
            overlap >= 0.5 && strength >= self.min_active_strength
        })
    }

    /// Get all active inhibitions for a given context.
    pub fn active_inhibitions(&self, context_keywords: &[String]) -> Vec<&InhibitedAction> {
        self.inhibitions.values().flat_map(|entries| {
            entries.iter().filter(|e| {
                let overlap = keyword_overlap(&e.context_keywords, context_keywords);
                let strength = e.current_strength(self.decay_half_life_days);
                overlap >= 0.3 && strength >= self.min_active_strength
            })
        }).collect()
    }

    /// Remove expired inhibitions (below minimum strength).
    pub fn gc(&mut self) {
        let half_life = self.decay_half_life_days;
        let min = self.min_active_strength;
        for entries in self.inhibitions.values_mut() {
            entries.retain(|e| e.current_strength(half_life) >= min);
        }
        self.inhibitions.retain(|_, v| !v.is_empty());
    }

    /// Number of active inhibitions across all tools.
    pub fn len(&self) -> usize {
        self.inhibitions.values().map(|v| v.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for InhibitionGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Keyword Jaccard overlap between two keyword sets.
fn keyword_overlap(a: &[String], b: &[String]) -> f64 {
    use std::collections::HashSet;
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let sa: HashSet<&str> = a.iter().map(|s| s.as_str()).collect();
    let sb: HashSet<&str> = b.iter().map(|s| s.as_str()).collect();
    let intersection = sa.intersection(&sb).count();
    let union = sa.union(&sb).count();
    if union == 0 { 0.0 } else { intersection as f64 / union as f64 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inhibition_builds_on_failure() {
        let mut gate = InhibitionGate::new();
        let ctx = vec!["rust".into(), "build".into()];

        gate.record_failure("execute_command", &ctx, "permission denied");
        assert!(gate.check("execute_command", &ctx).is_some());

        // Different context should not be inhibited
        let other_ctx = vec!["python".into(), "test".into()];
        assert!(gate.check("execute_command", &other_ctx).is_none());
    }

    #[test]
    fn success_weakens_inhibition() {
        let mut gate = InhibitionGate::new();
        let ctx = vec!["deploy".into()];

        gate.record_failure("execute_command", &ctx, "failed");
        gate.record_failure("execute_command", &ctx, "failed again");
        let before = gate.check("execute_command", &ctx).unwrap().suppression_strength;

        gate.record_success("execute_command", &ctx);
        let after = gate.check("execute_command", &ctx).unwrap().suppression_strength;
        assert!(after < before);
    }

    #[test]
    fn gc_removes_expired() {
        let mut gate = InhibitionGate::new();
        let ctx = vec!["test".into()];
        gate.record_failure("old_tool", &ctx, "old failure");
        // Force the inhibition to be very weak
        if let Some(entries) = gate.inhibitions.get_mut("old_tool") {
            entries[0].suppression_strength = 0.01;
        }
        gate.gc();
        assert!(gate.is_empty());
    }
}
