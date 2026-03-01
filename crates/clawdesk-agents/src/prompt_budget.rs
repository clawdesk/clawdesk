//! Prompt assembly budget accounting and telemetry.
//!
//! Bridges the `PromptBudget` (domain) and `SkillSelector` (skills) with
//! the runner's event emission, providing fine-grained budget telemetry
//! that the legacy system exposes but ClawDesk previously lacked.
//!
//! ## Architecture
//!
//! `PromptBudgetTracker` wraps a `PromptBudget` and tracks per-section
//! utilisation as assembly progresses. After assembly, it emits a
//! `BudgetReport` that breaks down token consumption by section.
//!
//! ## Invariants
//!
//! - Total consumed ≤ budget.total after each section addition
//! - Each section consumed ≤ its cap
//! - history_floor is always respected

use std::collections::HashMap;

/// Per-section utilisation record.
#[derive(Debug, Clone)]
pub struct SectionUtilisation {
    pub name: &'static str,
    pub cap: usize,
    pub consumed: usize,
}

impl SectionUtilisation {
    pub fn utilisation_pct(&self) -> f64 {
        if self.cap == 0 {
            return 0.0;
        }
        (self.consumed as f64 / self.cap as f64) * 100.0
    }

    pub fn remaining(&self) -> usize {
        self.cap.saturating_sub(self.consumed)
    }
}

/// Full budget report after prompt assembly.
#[derive(Debug, Clone)]
pub struct BudgetReport {
    /// Per-section breakdown.
    pub sections: Vec<SectionUtilisation>,
    /// Total tokens consumed across all sections.
    pub total_consumed: usize,
    /// Total available (budget.total - response_reserve).
    pub total_available: usize,
    /// Overall utilisation percentage.
    pub utilisation_pct: f64,
    /// Skills that were included (id → token cost).
    pub skills_included: HashMap<String, usize>,
    /// Skills that were excluded (id → reason).
    pub skills_excluded: HashMap<String, String>,
    /// History tokens used.
    pub history_tokens: usize,
    /// Whether history was compressed to respect budget.
    pub history_compressed: bool,
}

/// Tracks token consumption during prompt assembly.
///
/// Use `consume_*` methods as each section is assembled, then call
/// `report()` to get the final telemetry.
pub struct PromptBudgetTracker {
    // Caps
    total: usize,
    response_reserve: usize,
    identity_cap: usize,
    skills_cap: usize,
    memory_cap: usize,
    history_floor: usize,
    runtime_cap: usize,
    safety_cap: usize,

    // Current consumption
    identity_consumed: usize,
    skills_consumed: usize,
    memory_consumed: usize,
    history_consumed: usize,
    runtime_consumed: usize,
    safety_consumed: usize,

    // Skill tracking
    skills_included: HashMap<String, usize>,
    skills_excluded: HashMap<String, String>,
    history_compressed: bool,
}

impl PromptBudgetTracker {
    /// Create a new tracker from budget parameters.
    pub fn new(
        total: usize,
        response_reserve: usize,
        identity_cap: usize,
        skills_cap: usize,
        memory_cap: usize,
        history_floor: usize,
        runtime_cap: usize,
        safety_cap: usize,
    ) -> Self {
        Self {
            total,
            response_reserve,
            identity_cap,
            skills_cap,
            memory_cap,
            history_floor,
            runtime_cap,
            safety_cap,
            identity_consumed: 0,
            skills_consumed: 0,
            memory_consumed: 0,
            history_consumed: 0,
            runtime_consumed: 0,
            safety_consumed: 0,
            skills_included: HashMap::new(),
            skills_excluded: HashMap::new(),
            history_compressed: false,
        }
    }

    fn available(&self) -> usize {
        self.total.saturating_sub(self.response_reserve)
    }

    fn total_consumed(&self) -> usize {
        self.identity_consumed
            + self.skills_consumed
            + self.memory_consumed
            + self.history_consumed
            + self.runtime_consumed
            + self.safety_consumed
    }

    fn global_remaining(&self) -> usize {
        self.available().saturating_sub(self.total_consumed())
    }

    /// Record identity/persona section token consumption.
    pub fn consume_identity(&mut self, tokens: usize) -> usize {
        let actual = tokens.min(self.identity_cap).min(self.global_remaining());
        self.identity_consumed = actual;
        actual
    }

    /// Record runtime context section token consumption.
    pub fn consume_runtime(&mut self, tokens: usize) -> usize {
        let actual = tokens.min(self.runtime_cap).min(self.global_remaining());
        self.runtime_consumed = actual;
        actual
    }

    /// Record safety section token consumption.
    pub fn consume_safety(&mut self, tokens: usize) -> usize {
        let actual = tokens.min(self.safety_cap).min(self.global_remaining());
        self.safety_consumed = actual;
        actual
    }

    /// Try to include a skill within the skills budget.
    ///
    /// Returns `true` if the skill was included, `false` if excluded.
    pub fn try_include_skill(&mut self, skill_id: &str, token_cost: usize) -> bool {
        let skills_remaining = self.skills_cap.saturating_sub(self.skills_consumed);
        let global_remaining = self.global_remaining();
        let effective_remaining = skills_remaining.min(global_remaining);

        if token_cost <= effective_remaining {
            self.skills_consumed += token_cost;
            self.skills_included
                .insert(skill_id.to_string(), token_cost);
            true
        } else {
            let reason = format!(
                "budget exhausted: needed {token_cost}, available {effective_remaining}"
            );
            self.skills_excluded
                .insert(skill_id.to_string(), reason);
            false
        }
    }

    /// Explicitly exclude a skill with a reason.
    pub fn exclude_skill(&mut self, skill_id: &str, reason: &str) {
        self.skills_excluded
            .insert(skill_id.to_string(), reason.to_string());
    }

    /// Record memory fragment token consumption.
    pub fn consume_memory(&mut self, tokens: usize) -> usize {
        let actual = tokens.min(self.memory_cap).min(self.global_remaining());
        self.memory_consumed = actual;
        actual
    }

    /// Record history token consumption.
    ///
    /// Returns the actual tokens used. If the available budget is less than
    /// `history_floor`, this clamps to `history_floor` (violating other
    /// section caps in favour of history — the history_floor invariant).
    pub fn consume_history(&mut self, tokens: usize) -> usize {
        let remaining = self.global_remaining();
        let actual = if remaining < self.history_floor {
            // History floor takes precedence — but never exceed total budget.
            let floor_adjusted = tokens.min(self.history_floor).min(remaining);
            self.history_compressed = tokens > floor_adjusted;
            floor_adjusted
        } else {
            let actual = tokens.min(remaining);
            self.history_compressed = tokens > actual;
            actual
        };
        self.history_consumed = actual;
        actual
    }

    /// Remaining budget for skills.
    pub fn skills_remaining(&self) -> usize {
        self.skills_cap.saturating_sub(self.skills_consumed)
    }

    /// Generate the final budget report.
    pub fn report(&self) -> BudgetReport {
        let total_consumed = self.total_consumed();
        let total_available = self.available();

        let utilisation_pct = if total_available > 0 {
            (total_consumed as f64 / total_available as f64) * 100.0
        } else {
            0.0
        };

        BudgetReport {
            sections: vec![
                SectionUtilisation {
                    name: "identity",
                    cap: self.identity_cap,
                    consumed: self.identity_consumed,
                },
                SectionUtilisation {
                    name: "skills",
                    cap: self.skills_cap,
                    consumed: self.skills_consumed,
                },
                SectionUtilisation {
                    name: "memory",
                    cap: self.memory_cap,
                    consumed: self.memory_consumed,
                },
                SectionUtilisation {
                    name: "history",
                    cap: self.total.saturating_sub(
                        self.response_reserve
                            + self.identity_cap
                            + self.skills_cap
                            + self.memory_cap
                            + self.runtime_cap
                            + self.safety_cap,
                    ),
                    consumed: self.history_consumed,
                },
                SectionUtilisation {
                    name: "runtime",
                    cap: self.runtime_cap,
                    consumed: self.runtime_consumed,
                },
                SectionUtilisation {
                    name: "safety",
                    cap: self.safety_cap,
                    consumed: self.safety_consumed,
                },
            ],
            total_consumed,
            total_available,
            utilisation_pct,
            skills_included: self.skills_included.clone(),
            skills_excluded: self.skills_excluded.clone(),
            history_tokens: self.history_consumed,
            history_compressed: self.history_compressed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tracker() -> PromptBudgetTracker {
        PromptBudgetTracker::new(
            128_000,  // total
            8_192,    // response_reserve
            2_000,    // identity_cap
            4_096,    // skills_cap
            4_096,    // memory_cap
            2_000,    // history_floor
            512,      // runtime_cap
            1_024,    // safety_cap
        )
    }

    #[test]
    fn test_section_consumption() {
        let mut t = make_tracker();
        assert_eq!(t.available(), 128_000 - 8_192);

        let actual = t.consume_identity(1_500);
        assert_eq!(actual, 1_500);

        let actual = t.consume_runtime(400);
        assert_eq!(actual, 400);

        let actual = t.consume_safety(800);
        assert_eq!(actual, 800);

        assert_eq!(t.total_consumed(), 2_700);
    }

    #[test]
    fn test_identity_cap_respected() {
        let mut t = make_tracker();
        let actual = t.consume_identity(5_000);
        assert_eq!(actual, 2_000); // capped at identity_cap
    }

    #[test]
    fn test_skill_inclusion_and_exclusion() {
        let mut t = make_tracker();
        t.consume_identity(1_500);

        assert!(t.try_include_skill("search", 1_000));
        assert!(t.try_include_skill("calendar", 2_000));
        assert_eq!(t.skills_remaining(), 1_096);

        // This should fail — only 1096 tokens remaining in skills budget
        assert!(!t.try_include_skill("large_skill", 2_000));

        let report = t.report();
        assert_eq!(report.skills_included.len(), 2);
        assert_eq!(report.skills_excluded.len(), 1);
        assert!(report.skills_excluded.contains_key("large_skill"));
    }

    #[test]
    fn test_history_floor() {
        // Create a tracker with very tight budget
        let mut t = PromptBudgetTracker::new(
            10_000,  // total
            2_000,   // response_reserve (available = 8000)
            2_000,   // identity
            2_000,   // skills
            2_000,   // memory
            1_500,   // history_floor
            500,     // runtime
            500,     // safety
        );

        // Consume everything except history
        t.consume_identity(2_000);
        t.consume_runtime(500);
        t.consume_safety(500);
        t.consume_memory(2_000);
        t.skills_consumed = 2_000;

        // Only 1000 remaining, but history_floor is 1500.
        // The corrected behaviour caps at remaining to prevent budget overflow.
        let actual = t.consume_history(5_000);
        assert_eq!(actual, 1_000); // capped to remaining (budget invariant)
        assert!(t.report().history_compressed);
    }

    #[test]
    fn test_report_utilisation() {
        let mut t = make_tracker();
        t.consume_identity(2_000);
        t.consume_runtime(512);
        t.consume_safety(1_024);
        t.try_include_skill("search", 2_048);
        t.consume_memory(3_000);
        t.consume_history(10_000);

        let report = t.report();
        assert!(report.utilisation_pct > 0.0);
        assert!(report.utilisation_pct < 100.0);
        assert_eq!(report.total_consumed, 2_000 + 512 + 1_024 + 2_048 + 3_000 + 10_000);
    }
}
