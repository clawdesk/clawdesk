//! Concrete `SkillProvider` implementation — bridges `SkillOrchestrator`
//! + `EnvResolver` to the runner's `SkillProvider` trait.
//!
//! This module provides `OrchestratorSkillProvider`, the production implementation
//! of `clawdesk_agents::SkillProvider`. It wraps:
//! - `SkillOrchestrator` for per-turn skill selection
//! - `EnvResolver` for env var injection when skills require API keys
//!
//! ## Architecture
//!
//! ```text
//! AgentRunner ─→ SkillProvider trait ─→ OrchestratorSkillProvider
//!                                              │
//!                                              ├── SkillOrchestrator (trigger + knapsack)
//!                                              └── EnvResolver (apiKey → env mapping)
//! ```

use crate::env_injection::EnvResolver;
use crate::orchestrator::{SkillOrchestrator, TurnContext};
use clawdesk_agents::runner::{SkillInjection, SkillProvider};
use std::sync::Mutex;
use tracing::{debug, info};

/// Production `SkillProvider` that bridges orchestrator + env injection.
///
/// Uses internal `Mutex` for `SkillOrchestrator` (which requires `&mut self`
/// for history tracking) and `EnvResolver` (which is stateless but needs
/// consistent config reads).
pub struct OrchestratorSkillProvider {
    /// The skill orchestrator (behind Mutex for &mut self on select_for_turn).
    orchestrator: Mutex<SkillOrchestrator>,
    /// Env resolver for API key injection.
    env_resolver: EnvResolver,
}

impl OrchestratorSkillProvider {
    /// Create a new provider wrapping an orchestrator and env resolver.
    pub fn new(orchestrator: SkillOrchestrator, env_resolver: EnvResolver) -> Self {
        Self {
            orchestrator: Mutex::new(orchestrator),
            env_resolver,
        }
    }
}

#[async_trait::async_trait]
impl SkillProvider for OrchestratorSkillProvider {
    async fn select_skills(
        &self,
        user_message: &str,
        session_id: &str,
        channel_id: Option<&str>,
        turn_number: u32,
        _token_budget: usize,
    ) -> SkillInjection {
        // Build turn context
        let mut ctx = TurnContext::new(session_id, user_message)
            .with_turn_number(turn_number);

        if let Some(ch) = channel_id {
            ctx = ctx.with_channel(ch);
        }

        // Run skill selection (needs &mut self → behind Mutex)
        let result = {
            let mut orch = self.orchestrator.lock().expect("orchestrator lock");
            orch.select_for_turn(&ctx)
        };

        // Collect prompt fragments and apply env injection for selected skills
        let mut prompt_fragments = Vec::new();
        let mut selected_ids = Vec::new();
        let mut tool_names = Vec::new();

        for selected in &result.selected {
            let skill_id = selected.skill.manifest.id.as_str().to_string();

            // Check if this skill is disabled via config
            if self.env_resolver.is_skill_disabled(&skill_id) {
                debug!(skill = %skill_id, "skill disabled via config, skipping");
                continue;
            }

            // Apply env injection for skills that need API keys.
            // The EnvGuard applies env vars when created and restores them
            // on drop. For prompt injection (non-executing skills), we only
            // need to verify env availability, not actually inject.
            // For skills with required_tools (executing skills), the env
            // injection happens at tool execution time, not here.
            //
            // Here we just log a warning if required env vars are missing.
            if !selected.skill.manifest.triggers.is_empty() {
                debug!(
                    skill = %skill_id,
                    "skill selected for prompt injection"
                );
            }

            prompt_fragments.push(selected.skill.prompt_fragment.clone());
            selected_ids.push(skill_id);

            // Skills = prompts, not tools. Skills teach the LLM which
            // builtin tools to use via their prompt_fragment. We no longer
            // collect provided_tools names since stub handlers are removed.
        }

        let excluded_ids: Vec<String> = result
            .excluded
            .iter()
            .map(|(id, _reason)| id.as_str().to_string())
            .collect();

        info!(
            selected = selected_ids.len(),
            excluded = excluded_ids.len(),
            tools = tool_names.len(),
            "OrchestratorSkillProvider: skills selected"
        );

        SkillInjection {
            prompt_fragments,
            selected_skill_ids: selected_ids,
            excluded_skill_ids: excluded_ids,
            total_tokens: result.total_tokens,
            tool_names,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::{Skill, SkillId, SkillManifest, SkillTrigger};
    use std::sync::Arc;

    fn make_skill(id: &str, keywords: Vec<&str>, token_estimate: usize) -> Arc<Skill> {
        Arc::new(Skill {
            manifest: SkillManifest {
                id: SkillId::from(id),
                display_name: id.to_string(),
                description: format!("Skill {}", id),
                version: "0.1.0".into(),
                author: None,
                triggers: vec![SkillTrigger::Keywords {
                    words: keywords.into_iter().map(String::from).collect(),
                    threshold: 0.3,
                }],
                parameters: vec![],
                dependencies: vec![],
                tags: vec![],
                estimated_tokens: token_estimate,
                priority_weight: 1.0,
                required_tools: vec![],
                signature: None,
                publisher_key: None,
                content_hash: None,
                schema_version: 1,
            },
            prompt_fragment: "Test prompt fragment".into(),
            provided_tools: vec![],
            parameter_values: serde_json::Value::Null,
            source_path: None,
        })
    }

    #[tokio::test]
    async fn orchestrator_provider_selects_skills() {
        let skills = vec![
            make_skill("test/search", vec!["search", "find"], 100),
            make_skill("test/code", vec!["code", "program"], 100),
        ];

        let orchestrator = SkillOrchestrator::new(skills, 5000);
        let env_resolver = EnvResolver::new();
        let provider = OrchestratorSkillProvider::new(orchestrator, env_resolver);

        let injection = provider
            .select_skills("search for files", "sess-1", None, 1, 5000)
            .await;

        assert!(!injection.prompt_fragments.is_empty());
        assert!(injection.selected_skill_ids.contains(&"test/search".to_string()));
    }

    #[tokio::test]
    async fn orchestrator_provider_respects_disabled() {
        let skills = vec![
            make_skill("test/disabled", vec!["test"], 100),
        ];

        let orchestrator = SkillOrchestrator::new(skills, 5000);
        let mut env_resolver = EnvResolver::new();
        use crate::env_injection::SkillConfigEntry;
        env_resolver.add_skill_config(
            "test/disabled",
            SkillConfigEntry {
                api_key: None,
                env: Default::default(),
                enabled: Some(false),
            },
        );
        let provider = OrchestratorSkillProvider::new(orchestrator, env_resolver);

        let injection = provider
            .select_skills("test message", "sess-1", None, 1, 5000)
            .await;

        // Skill was selected by trigger but filtered by disabled config
        assert!(injection.prompt_fragments.is_empty());
    }
}
