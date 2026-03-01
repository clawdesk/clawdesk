//! Task-derived system prompt assembly for ephemeral sub-agents.
//!
//! Generates focused, role-specific system prompts from runtime parameters.
//! The prompt structure follows a hierarchical information architecture:
//!
//! 1. **Identity** — what you are
//! 2. **Purpose** — what you do (the task)
//! 3. **Constraints** — what you don't do
//! 4. **Output format** — how to report back
//! 5. **Capabilities** — what tools you have (depth-conditional)
//!
//! This ordering places the highest-entropy information (task description) in
//! the most-attended position, following the primacy effect in transformer
//! attention distributions.

/// Parameters for building an ephemeral agent's system prompt.
#[derive(Debug, Clone)]
pub struct EphemeralPromptParams {
    /// The task description — the semantic core of the prompt.
    pub task: String,
    /// Optional human-readable label (e.g. "code-reviewer").
    pub label: Option<String>,
    /// Current depth in the spawn tree (0 = root, 1 = first child, etc.).
    pub depth: u32,
    /// Maximum allowed depth (children at this depth cannot spawn further).
    pub max_depth: u32,
    /// Whether the agent has tools available.
    pub has_tools: bool,
    /// Names of available tools (for the capabilities section).
    pub tool_names: Vec<String>,
    /// Optional parent session ID for context linking.
    pub parent_session: Option<String>,
}

/// Build a focused system prompt for an ephemeral sub-agent.
///
/// The prompt communicates identity, task, constraints, output format,
/// and spawn capability in a structured layout that maximizes the child
/// agent's task completion quality.
///
/// Prompt length is bounded by `~800 + |task| + |label|` tokens.
pub fn build_ephemeral_system_prompt(params: &EphemeralPromptParams) -> String {
    let mut prompt = String::with_capacity(2048);

    // ── 1. Identity ──────────────────────────────────────────────
    let role = params.label.as_deref().unwrap_or("specialist");
    let spawner = if params.depth >= 2 {
        "a parent orchestrator"
    } else {
        "the main agent"
    };
    prompt.push_str(&format!(
        "You are an ephemeral {role} sub-agent, created by {spawner} to handle a specific task.\n\n"
    ));

    // ── 2. Purpose (task) ────────────────────────────────────────
    prompt.push_str("## Your Task\n\n");
    prompt.push_str(&params.task);
    prompt.push_str("\n\n");

    // ── 3. Constraints ───────────────────────────────────────────
    prompt.push_str("## Rules\n\n");
    prompt.push_str("- You are ephemeral: you exist only for this task and will be destroyed after responding.\n");
    prompt.push_str("- Focus exclusively on the task above. Do not ask follow-up questions.\n");
    prompt.push_str("- Do not initiate side conversations or proactive actions.\n");
    prompt.push_str("- Be thorough but concise. Your response is automatically returned to the caller.\n");

    // ── 4. Output format ─────────────────────────────────────────
    prompt.push_str("\n## Output\n\n");
    prompt.push_str("Provide your final answer directly. Do not wrap it in JSON or any special format unless the task explicitly requires it. ");
    prompt.push_str("Your entire response will be passed back to the agent that spawned you.\n");

    // ── 5. Capabilities (depth-conditional) ──────────────────────
    let can_spawn = params.depth + 1 < params.max_depth;
    if can_spawn {
        prompt.push_str("\n## Delegation\n\n");
        prompt.push_str("You CAN create your own sub-agents using `dynamic_spawn` if the task benefits from further decomposition. ");
        prompt.push_str("Only delegate when the subtask is genuinely independent and benefits from specialization.\n");
    } else {
        prompt.push_str("\n## Delegation\n\n");
        prompt.push_str("You CANNOT spawn further sub-agents. You are at the maximum delegation depth. ");
        prompt.push_str("Complete the task directly using your available tools and reasoning.\n");
    }

    if params.has_tools && !params.tool_names.is_empty() {
        prompt.push_str(&format!(
            "\nAvailable tools: {}\n",
            params.tool_names.join(", ")
        ));
    } else if !params.has_tools {
        prompt.push_str("\nYou have no tools. Use your reasoning and knowledge to complete the task.\n");
    }

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_prompt_generation() {
        let params = EphemeralPromptParams {
            task: "Summarize the key points of quantum computing".into(),
            label: Some("summarizer".into()),
            depth: 1,
            max_depth: 3,
            has_tools: false,
            tool_names: vec![],
            parent_session: None,
        };
        let prompt = build_ephemeral_system_prompt(&params);
        assert!(prompt.contains("ephemeral summarizer sub-agent"));
        assert!(prompt.contains("the main agent"));
        assert!(prompt.contains("Summarize the key points"));
        assert!(prompt.contains("You CAN create your own sub-agents"));
        assert!(prompt.contains("no tools"));
    }

    #[test]
    fn test_leaf_agent_no_spawn() {
        let params = EphemeralPromptParams {
            task: "Do something".into(),
            label: None,
            depth: 3,
            max_depth: 3,
            has_tools: true,
            tool_names: vec!["read_file".into(), "grep".into()],
            parent_session: None,
        };
        let prompt = build_ephemeral_system_prompt(&params);
        assert!(prompt.contains("CANNOT spawn further"));
        assert!(prompt.contains("maximum delegation depth"));
        assert!(prompt.contains("read_file, grep"));
    }

    #[test]
    fn test_deep_spawner_wording() {
        let params = EphemeralPromptParams {
            task: "Analyze code".into(),
            label: Some("analyzer".into()),
            depth: 2,
            max_depth: 5,
            has_tools: true,
            tool_names: vec!["read_file".into()],
            parent_session: None,
        };
        let prompt = build_ephemeral_system_prompt(&params);
        assert!(prompt.contains("a parent orchestrator"));
    }

    #[test]
    fn test_default_label() {
        let params = EphemeralPromptParams {
            task: "Do work".into(),
            label: None,
            depth: 1,
            max_depth: 3,
            has_tools: false,
            tool_names: vec![],
            parent_session: None,
        };
        let prompt = build_ephemeral_system_prompt(&params);
        assert!(prompt.contains("ephemeral specialist sub-agent"));
    }

    #[test]
    fn test_prompt_contains_all_sections() {
        let params = EphemeralPromptParams {
            task: "Test task".into(),
            label: Some("tester".into()),
            depth: 0,
            max_depth: 3,
            has_tools: true,
            tool_names: vec!["echo".into()],
            parent_session: Some("session-123".into()),
        };
        let prompt = build_ephemeral_system_prompt(&params);
        assert!(prompt.contains("## Your Task"));
        assert!(prompt.contains("## Rules"));
        assert!(prompt.contains("## Output"));
        assert!(prompt.contains("## Delegation"));
    }
}
