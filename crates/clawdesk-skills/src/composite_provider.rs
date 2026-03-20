//! Composite skill provider — chains multiple `SkillProvider` implementations.
//!
//! Runs each inner provider in sequence and merges their `SkillInjection` results.
//! This allows combining the `OrchestratorSkillProvider` (user-installed skills)
//! with built-in providers like `BrowserSkillProvider` (hardware-gated skills).

use clawdesk_agents::runner::{SkillInjection, SkillProvider};
use std::sync::Arc;

/// Chains multiple [`SkillProvider`] implementations, merging their results.
pub struct CompositeSkillProvider {
    providers: Vec<Arc<dyn SkillProvider>>,
}

impl CompositeSkillProvider {
    pub fn new(providers: Vec<Arc<dyn SkillProvider>>) -> Self {
        Self { providers }
    }
}

#[async_trait::async_trait]
impl SkillProvider for CompositeSkillProvider {
    async fn select_skills(
        &self,
        user_message: &str,
        session_id: &str,
        channel_id: Option<&str>,
        turn_number: u32,
        token_budget: usize,
    ) -> SkillInjection {
        let mut merged = SkillInjection::default();
        let mut remaining_budget = token_budget;

        for provider in &self.providers {
            let injection = provider
                .select_skills(user_message, session_id, channel_id, turn_number, remaining_budget)
                .await;

            remaining_budget = remaining_budget.saturating_sub(injection.total_tokens);
            merged.prompt_fragments.extend(injection.prompt_fragments);
            merged.selected_skill_ids.extend(injection.selected_skill_ids);
            merged.excluded_skill_ids.extend(injection.excluded_skill_ids);
            merged.total_tokens += injection.total_tokens;
            merged.tool_names.extend(injection.tool_names);
        }

        merged
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct StubProvider {
        skill_id: String,
        fragment: String,
    }

    #[async_trait::async_trait]
    impl SkillProvider for StubProvider {
        async fn select_skills(
            &self,
            _user_message: &str,
            _session_id: &str,
            _channel_id: Option<&str>,
            _turn_number: u32,
            _token_budget: usize,
        ) -> SkillInjection {
            SkillInjection {
                prompt_fragments: vec![self.fragment.clone()],
                selected_skill_ids: vec![self.skill_id.clone()],
                excluded_skill_ids: vec![],
                total_tokens: 100,
                tool_names: vec![],
            }
        }
    }

    #[tokio::test]
    async fn composite_merges_providers() {
        let a: Arc<dyn SkillProvider> = Arc::new(StubProvider {
            skill_id: "skill_a".into(),
            fragment: "fragment_a".into(),
        });
        let b: Arc<dyn SkillProvider> = Arc::new(StubProvider {
            skill_id: "skill_b".into(),
            fragment: "fragment_b".into(),
        });
        let composite = CompositeSkillProvider::new(vec![a, b]);

        let result = composite
            .select_skills("test", "s1", None, 1, 4000)
            .await;
        assert_eq!(result.selected_skill_ids.len(), 2);
        assert_eq!(result.prompt_fragments.len(), 2);
        assert_eq!(result.total_tokens, 200);
    }
}
