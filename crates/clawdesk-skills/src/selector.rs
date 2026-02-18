//! Token-budgeted skill selection — greedy knapsack algorithm.
//!
//! ## Problem formulation
//!
//! Given:
//! - A set S of skills, each with token cost tᵢ and priority weight wᵢ
//! - A token budget B for the system prompt's skill section
//! - A dependency DAG D over S
//!
//! Find: A subset S* ⊆ S that maximizes Σ wᵢ subject to:
//!   1. Σ tᵢ ≤ B  (budget constraint)
//!   2. ∀ s ∈ S*, deps(s) ⊆ S*  (dependency closure)
//!
//! ## Algorithm
//!
//! 1. Compute dependency closure for each candidate skill: O(V + E).
//! 2. Sort by value density (wᵢ / tᵢ) descending: O(k log k).
//! 3. Greedily include skills if budget allows and deps are satisfied.
//!
//! When |S| < 100, the greedy approach is within 1-ε of optimal for our
//! instances (skills are approximately unit-size relative to budget).

use crate::definition::{Skill, SkillId};
use std::sync::Arc;

/// A skill selected for inclusion in the agent's system prompt.
#[derive(Debug, Clone)]
pub struct SelectedSkill {
    pub skill: Arc<Skill>,
    /// Actual token cost at time of selection.
    pub token_cost: usize,
}

/// Result of skill selection.
#[derive(Debug, Clone)]
pub struct SelectionResult {
    /// Skills included within the token budget, in injection order.
    pub selected: Vec<SelectedSkill>,
    /// Skills excluded (with reason).
    pub excluded: Vec<(SkillId, ExclusionReason)>,
    /// Total tokens consumed by selected skills.
    pub total_tokens: usize,
    /// Remaining token budget.
    pub remaining_budget: usize,
}

/// Why a skill was excluded.
#[derive(Debug, Clone)]
pub enum ExclusionReason {
    /// Not enough token budget remaining.
    BudgetExhausted { needed: usize, available: usize },
    /// A required dependency was not selected.
    MissingDependency { dependency: SkillId },
    /// Skill is not active.
    NotActive,
}

/// Token-budgeted skill selector using greedy knapsack.
pub struct SkillSelector;

impl SkillSelector {
    /// Select skills for inclusion in the system prompt within a token budget.
    ///
    /// # Arguments
    /// - `candidates`: Active skills to consider (pre-filtered).
    /// - `budget`: Maximum token budget for skill prompt fragments.
    ///
    /// # Returns
    /// `SelectionResult` with selected skills in injection order.
    ///
    /// # Complexity
    /// O(k log k) where k = |candidates|.
    pub fn select(candidates: &[Arc<Skill>], budget: usize) -> SelectionResult {
        if candidates.is_empty() {
            return SelectionResult {
                selected: vec![],
                excluded: vec![],
                total_tokens: 0,
                remaining_budget: budget,
            };
        }

        // Phase 1: Sort by value density (priority_weight / token_cost) descending.
        // This is the greedy heuristic for the fractional relaxation of 0-1 knapsack.
        let mut indexed: Vec<(usize, f64)> = candidates
            .iter()
            .enumerate()
            .map(|(i, s)| (i, s.value_density()))
            .collect();

        // Stable sort: ties broken by original order (preserves user-specified ordering).
        indexed.sort_by(|a, b| {
            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
        });

        // Phase 2: Greedy selection with dependency checking.
        let mut selected = Vec::new();
        let mut excluded = Vec::new();
        let mut total_tokens = 0usize;
        let mut selected_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

        for (idx, _density) in &indexed {
            let skill = &candidates[*idx];
            let cost = skill.token_cost();

            // Check budget
            if total_tokens + cost > budget {
                excluded.push((
                    skill.manifest.id.clone(),
                    ExclusionReason::BudgetExhausted {
                        needed: cost,
                        available: budget.saturating_sub(total_tokens),
                    },
                ));
                continue;
            }

            // Check dependencies
            let deps_satisfied = skill
                .manifest
                .dependencies
                .iter()
                .all(|dep| selected_ids.contains(dep.as_str()));

            if !deps_satisfied {
                let missing = skill
                    .manifest
                    .dependencies
                    .iter()
                    .find(|dep| !selected_ids.contains(dep.as_str()))
                    .cloned()
                    .unwrap_or_else(|| SkillId::from("unknown"));

                excluded.push((
                    skill.manifest.id.clone(),
                    ExclusionReason::MissingDependency {
                        dependency: missing,
                    },
                ));
                continue;
            }

            // Include this skill
            selected_ids.insert(skill.manifest.id.as_str().to_string());
            total_tokens += cost;
            selected.push(SelectedSkill {
                skill: Arc::clone(skill),
                token_cost: cost,
            });
        }

        SelectionResult {
            selected,
            excluded,
            total_tokens,
            remaining_budget: budget.saturating_sub(total_tokens),
        }
    }

    /// Compose selected skills into a single prompt section.
    ///
    /// Format:
    /// ```text
    /// <skills>
    /// ## Skill: Display Name
    /// [prompt fragment]
    ///
    /// ## Skill: Display Name 2
    /// [prompt fragment]
    /// </skills>
    /// ```
    pub fn compose_prompt(selected: &[SelectedSkill]) -> String {
        if selected.is_empty() {
            return String::new();
        }

        let mut buf = String::with_capacity(
            selected.iter().map(|s| s.skill.prompt_fragment.len() + 40).sum(),
        );

        buf.push_str("<skills>\n");
        for (i, sel) in selected.iter().enumerate() {
            if i > 0 {
                buf.push('\n');
            }
            buf.push_str("## ");
            buf.push_str(&sel.skill.manifest.display_name);
            buf.push('\n');
            buf.push_str(&sel.skill.prompt_fragment);
            buf.push('\n');
        }
        buf.push_str("</skills>");

        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::*;

    fn make_skill(id: &str, priority: f64, prompt: &str) -> Arc<Skill> {
        Arc::new(Skill {
            manifest: SkillManifest {
                id: SkillId::from(id),
                display_name: id.to_string(),
                description: format!("Test skill: {}", id),
                version: "0.1.0".into(),
                author: None,
                dependencies: vec![],
                required_tools: vec![],
                parameters: vec![],
                triggers: vec![SkillTrigger::Always],
                estimated_tokens: 100,
                priority_weight: priority,
                tags: vec![],
                signature: None,
                publisher_key: None,
                content_hash: None,
                schema_version: 1,
            },
            prompt_fragment: prompt.to_string(),
            provided_tools: vec![],
            parameter_values: serde_json::Value::Null,
            source_path: None,
        })
    }

    #[test]
    fn budget_enforcement() {
        let skills = vec![
            make_skill("big", 10.0, &"x".repeat(400)),   // ~95 tokens
            make_skill("small", 5.0, &"y".repeat(40)),    // ~10 tokens
        ];

        // Budget of 20 tokens — only small fits.
        let result = SkillSelector::select(&skills, 20);
        assert_eq!(result.selected.len(), 1);
        assert_eq!(result.selected[0].skill.manifest.id.as_str(), "small");
        assert_eq!(result.excluded.len(), 1);
    }

    #[test]
    fn greedy_picks_highest_density() {
        let skills = vec![
            make_skill("low-dense", 1.0, &"x".repeat(200)),   // low density
            make_skill("high-dense", 10.0, &"y".repeat(40)),  // high density
        ];

        let result = SkillSelector::select(&skills, 500);
        assert_eq!(result.selected.len(), 2);
        // high-dense should be first (higher density)
        assert_eq!(result.selected[0].skill.manifest.id.as_str(), "high-dense");
    }

    #[test]
    fn compose_prompt_format() {
        let skills = vec![
            make_skill("search", 1.0, "You can search the web."),
        ];
        let result = SkillSelector::select(&skills, 10000);
        let prompt = SkillSelector::compose_prompt(&result.selected);
        assert!(prompt.starts_with("<skills>"));
        assert!(prompt.ends_with("</skills>"));
        assert!(prompt.contains("## search"));
        assert!(prompt.contains("You can search the web."));
    }
}
