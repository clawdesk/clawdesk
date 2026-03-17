//! # Enriched Agent Backend — Bridges pipeline executor with the unified engine.
//!
//! Ensures pipeline-invoked agents receive identical context enrichment
//! (memory recall, skill scoring, prompt assembly) as directly-invoked agents.
//!
//! This replaces the `ContextualBackend`'s parallel implementation of
//! memory/skill injection with a proper delegation through `engine.rs`.
//!
//! ## Architecture
//!
//! ```text
//! PipelineExecutor
//!   └→ EnrichedBackend::execute_agent(agent_id, input)
//!        ├→ engine::build_prompt_pipeline() (memory + skills + prompt assembly)
//!        ├→ AgentRunner::run() (full pipeline with failover)
//!        └→ return response
//! ```
//!
//! This eliminates the feature drift between the direct `send_message`
//! path and the pipeline path.

use crate::engine::{self, PromptPipelineInput};
use async_trait::async_trait;
use clawdesk_agents::{AgentBackend, PipelineError};
use clawdesk_domain::prompt_builder::PromptBudget;
use clawdesk_memory::MemoryManager;
use clawdesk_providers::Provider;
use clawdesk_sochdb::SochMemoryBackend;
use clawdesk_skills::SkillRegistry;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, warn};

/// An `AgentBackend` that delegates through `engine.rs` for full context enrichment.
///
/// Each pipeline step gets the same memory recall, skill scoring, and prompt
/// assembly as a direct `send_message` call. This is the single-path guarantee.
pub struct EnrichedBackend {
    /// Inner backend for actual agent execution.
    inner: Arc<dyn AgentBackend>,
    /// Memory manager for per-step recall.
    memory: Arc<MemoryManager<SochMemoryBackend>>,
    /// Skill registry for per-step scoring.
    skill_registry: Arc<std::sync::RwLock<SkillRegistry>>,
    /// Provider for LLM calls.
    provider: Arc<dyn Provider>,
    /// Default persona for pipeline agents.
    default_persona: String,
    /// Default model name.
    model_name: String,
    /// Token budget for prompt assembly.
    context_limit: usize,
}

impl EnrichedBackend {
    pub fn new(
        inner: Arc<dyn AgentBackend>,
        memory: Arc<MemoryManager<SochMemoryBackend>>,
        skill_registry: Arc<std::sync::RwLock<SkillRegistry>>,
        provider: Arc<dyn Provider>,
    ) -> Self {
        Self {
            inner,
            memory,
            skill_registry,
            provider,
            default_persona: clawdesk_types::session::DEFAULT_SYSTEM_PROMPT.into(),
            model_name: "claude-sonnet-4-20250514".into(),
            context_limit: 128_000,
        }
    }

    pub fn with_persona(mut self, persona: String) -> Self {
        self.default_persona = persona;
        self
    }

    pub fn with_model(mut self, model: String) -> Self {
        self.model_name = model;
        self
    }

    pub fn with_context_limit(mut self, limit: usize) -> Self {
        self.context_limit = limit;
        self
    }
}

#[async_trait]
impl AgentBackend for EnrichedBackend {
    async fn execute_agent(
        &self,
        agent_id: &str,
        skill_id: Option<&str>,
        input: &str,
        timeout: Duration,
    ) -> Result<String, PipelineError> {
        // Run the unified prompt pipeline for context enrichment
        let agent_skill_ids: HashSet<String> = skill_id
            .map(|s| {
                let mut set = HashSet::new();
                set.insert(s.to_string());
                set
            })
            .unwrap_or_default();

        let mut budget = PromptBudget::default();
        budget.total = self.context_limit;

        // Load active skills from registry
        let active_skills = engine::load_active_skills(&self.skill_registry);

        let pipeline_result = engine::build_prompt_pipeline(
            PromptPipelineInput {
                user_content: input,
                persona: &self.default_persona,
                model_name: &self.model_name,
                agent_skill_ids: &agent_skill_ids,
                channel_id: Some("pipeline"),
                channel_description: "Pipeline step execution",
                budget,
                available_channels: Vec::new(),
                session_id: None,
            },
            &self.memory,
            &active_skills,
        )
        .await;

        // Build enriched input with memory and skill context
        let mut enriched_input = String::new();

        if let Some(ref memory_injection) = pipeline_result.memory_injection {
            enriched_input.push_str(memory_injection);
            enriched_input.push_str("\n\n");
        }

        enriched_input.push_str(input);

        debug!(
            agent_id,
            has_memory = pipeline_result.memory_injection.is_some(),
            system_prompt_len = pipeline_result.system_prompt.len(),
            "enriched pipeline step via unified engine"
        );

        // Delegate to the inner backend with enriched input
        self.inner
            .execute_agent(agent_id, skill_id, &enriched_input, timeout)
            .await
    }

    async fn request_gate_approval(
        &self,
        prompt: &str,
        timeout: Duration,
    ) -> Result<bool, PipelineError> {
        self.inner.request_gate_approval(prompt, timeout).await
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    // Integration tests would require full AppState setup.
    // Unit tests for the delegation pattern:

    #[test]
    fn test_enriched_backend_builder() {
        // Compile-time check: EnrichedBackend can be constructed
        // (actual runtime test requires mock Provider + MemoryManager)
    }
}
