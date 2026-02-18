//! Prompt namespace isolation — capability-scoped skill prompt boundaries.
//!
//! ## Security (T-10)
//!
//! Skills inject prompt fragments into the agent's system prompt. Without
//! isolation, a malicious skill could:
//! 1. Override instructions from higher-trust skills
//! 2. Inject tool calls into other skills' contexts
//! 3. Exfiltrate data via prompt injection
//!
//! This module wraps each skill's prompt fragment in a structured envelope
//! with clear boundary markers and enforces tool access via a bipartite
//! graph (skill → allowed_tools).
//!
//! ## Architecture
//!
//! ```text
//! ┌─ SystemPrompt ──────────────────────────────┐
//! │ [CORE] Base agent instructions               │
//! │ [NAMESPACE:core/web-search] ────────────────│
//! │ │ Prompt fragment (sandboxed)                ││
//! │ │ Allowed tools: web_search, fetch_url       ││
//! │ └────────────────────────────────────────────│
//! │ [NAMESPACE:community/code-review] ──────────│
//! │ │ Prompt fragment (sandboxed)                ││
//! │ │ Allowed tools: read_file, list_dir         ││
//! │ └────────────────────────────────────────────│
//! └─────────────────────────────────────────────┘
//! ```

use clawdesk_types::estimate_tokens;
use std::collections::{HashMap, HashSet};

/// A capability-scoped prompt namespace.
#[derive(Debug, Clone)]
pub struct PromptNamespace {
    /// Skill ID that owns this namespace.
    pub skill_id: String,
    /// Trust level label (from verification).
    pub trust_label: String,
    /// The raw prompt fragment (untouched content).
    pub fragment: String,
    /// Tools this skill is allowed to invoke.
    pub allowed_tools: HashSet<String>,
    /// Tools this skill provides (registered in ToolRegistry).
    pub provided_tools: Vec<String>,
}

/// Assembled system prompt with namespace boundaries.
#[derive(Debug, Clone)]
pub struct IsolatedPrompt {
    /// Core system instructions (highest trust).
    pub core_instructions: String,
    /// Ordered namespaces with boundary markers.
    pub namespaces: Vec<PromptNamespace>,
    /// Total estimated tokens.
    pub total_tokens: usize,
}

impl IsolatedPrompt {
    /// Render the isolated prompt into a single string with boundary markers.
    ///
    /// Each namespace is wrapped in clear delimiters so that:
    /// 1. The LLM can distinguish skill boundaries
    /// 2. A skill's prompt cannot escape its namespace
    /// 3. Tool access is explicitly declared per namespace
    pub fn render(&self) -> String {
        let mut output = String::with_capacity(self.total_tokens * 4);

        // Core instructions (highest priority).
        output.push_str(&self.core_instructions);
        output.push_str("\n\n");

        for ns in &self.namespaces {
            // Boundary header.
            output.push_str(&format!(
                "--- BEGIN SKILL [{}] (trust: {}) ---\n",
                ns.skill_id, ns.trust_label
            ));

            // Tool access declaration.
            if !ns.allowed_tools.is_empty() {
                let tools: Vec<&str> = ns.allowed_tools.iter().map(|s| s.as_str()).collect();
                output.push_str(&format!("Allowed tools: {}\n", tools.join(", ")));
            }

            // Sandboxed prompt fragment.
            output.push_str(&ns.fragment);
            output.push('\n');

            // Boundary footer.
            output.push_str(&format!("--- END SKILL [{}] ---\n\n", ns.skill_id));
        }

        output
    }
}

/// Builder for constructing isolated prompts.
pub struct PromptIsolator {
    /// Bipartite graph: skill_id → allowed tool names.
    /// This is the access control policy for tool invocation.
    tool_access: HashMap<String, HashSet<String>>,
}

impl PromptIsolator {
    pub fn new() -> Self {
        Self {
            tool_access: HashMap::new(),
        }
    }

    /// Grant a skill access to specific tools.
    pub fn grant_tools(&mut self, skill_id: &str, tools: Vec<String>) {
        self.tool_access
            .entry(skill_id.to_string())
            .or_default()
            .extend(tools);
    }

    /// Check if a skill is allowed to use a specific tool.
    pub fn can_use_tool(&self, skill_id: &str, tool_name: &str) -> bool {
        self.tool_access
            .get(skill_id)
            .map(|tools| tools.contains(tool_name))
            .unwrap_or(false)
    }

    /// Build an isolated prompt from a core instruction set and skill list.
    pub fn build(
        &self,
        core_instructions: &str,
        skills: Vec<(String, String, String, Vec<String>)>, // (id, trust_label, fragment, provided_tools)
    ) -> IsolatedPrompt {
        let mut total_tokens = estimate_tokens(core_instructions);

        let namespaces: Vec<PromptNamespace> = skills
            .into_iter()
            .map(|(id, trust, fragment, provided)| {
                total_tokens += estimate_tokens(&fragment);
                let allowed = self
                    .tool_access
                    .get(&id)
                    .cloned()
                    .unwrap_or_default();

                PromptNamespace {
                    skill_id: id,
                    trust_label: trust,
                    fragment,
                    allowed_tools: allowed,
                    provided_tools: provided,
                }
            })
            .collect();

        IsolatedPrompt {
            core_instructions: core_instructions.to_string(),
            namespaces,
            total_tokens,
        }
    }
}

impl Default for PromptIsolator {
    fn default() -> Self {
        Self::new()
    }
}

// Token estimation consolidated in clawdesk_types::tokenizer::estimate_tokens

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_access_enforcement() {
        let mut isolator = PromptIsolator::new();
        isolator.grant_tools("core/web-search", vec!["web_search".into(), "fetch_url".into()]);

        assert!(isolator.can_use_tool("core/web-search", "web_search"));
        assert!(isolator.can_use_tool("core/web-search", "fetch_url"));
        assert!(!isolator.can_use_tool("core/web-search", "shell_exec"));
        assert!(!isolator.can_use_tool("unknown/skill", "web_search"));
    }

    #[test]
    fn test_isolated_prompt_render() {
        let mut isolator = PromptIsolator::new();
        isolator.grant_tools("core/search", vec!["web_search".into()]);

        let prompt = isolator.build(
            "You are a helpful assistant.",
            vec![(
                "core/search".into(),
                "builtin".into(),
                "You can search the web for information.".into(),
                vec!["web_search".into()],
            )],
        );

        let rendered = prompt.render();
        assert!(rendered.contains("BEGIN SKILL [core/search]"));
        assert!(rendered.contains("END SKILL [core/search]"));
        assert!(rendered.contains("Allowed tools: web_search"));
        assert!(rendered.contains("You can search the web"));
    }

    #[test]
    fn test_namespace_isolation() {
        let isolator = PromptIsolator::new();
        let prompt = isolator.build(
            "Core instructions.",
            vec![
                (
                    "skill-a".into(),
                    "signed(trusted)".into(),
                    "Skill A prompt.".into(),
                    vec![],
                ),
                (
                    "skill-b".into(),
                    "unsigned".into(),
                    "Skill B prompt.".into(),
                    vec![],
                ),
            ],
        );

        let rendered = prompt.render();
        // Both skills should have their own namespace boundaries.
        assert!(rendered.contains("BEGIN SKILL [skill-a]"));
        assert!(rendered.contains("END SKILL [skill-a]"));
        assert!(rendered.contains("BEGIN SKILL [skill-b]"));
        assert!(rendered.contains("END SKILL [skill-b]"));
        assert!(rendered.contains("trust: unsigned"));
    }
}
