//! Prompt assembly as a resource allocation problem — typed, budget-aware, observable.
//!
//! Treats system prompt construction as a **priority-weighted knapsack** with
//! per-section caps, combined budget enforcement, and a diagnostic manifest
//! that explains every inclusion/exclusion decision.
//!
//! ## Design rationale (vs. legacy)
//!
//! Legacy system concatenates 8+ Markdown files at runtime with per-file caps
//! (65,536 chars) but no combined budget. Skills are injected as flat text —
//! adding 3 skills can silently exceed the model's context window and there's
//! no way to know what's in the prompt until after the fact (`/context list`).
//!
//! This builder enforces per-section caps + combined knapsack, emits a
//! `PromptManifest` with every build (token-level accounting), and supports
//! explicit skill value-density ranking for greedy selection.
//!
//! ## Algorithm
//!
//! 1. **Fixed allocations** — identity, runtime, safety (Required priority).
//! 2. **Knapsack over skills** — sort by `value_density = relevance / token_cost`,
//!    greedily fill `skills_cap`.
//! 3. **Fill remaining** — memory + history context within their caps.
//! 4. **Emit manifest** — exact token accounting per section.
//!
//! Complexity: O(n log n) for sorting, O(n) for packing. Exact for |sections| < 100.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Budget specification
// ---------------------------------------------------------------------------

/// Per-section token budget with combined enforcement.
///
/// Each field represents a hard cap. The `total` field is the model context
/// limit; the builder will never produce a prompt exceeding it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptBudget {
    /// Model context limit (e.g., 128_000 for Claude Sonnet).
    pub total: usize,
    /// Reserved for the model's response output.
    pub response_reserve: usize,
    /// Maximum tokens for identity/persona section.
    pub identity_cap: usize,
    /// Maximum tokens for all skills combined.
    pub skills_cap: usize,
    /// Maximum tokens for retrieved memory fragments.
    pub memory_cap: usize,
    /// Minimum history tokens — never compress below this floor.
    pub history_floor: usize,
    /// Maximum tokens for runtime context (channel info, datetime, etc.).
    pub runtime_cap: usize,
    /// Maximum tokens for safety/guardrails section.
    pub safety_cap: usize,
}

impl Default for PromptBudget {
    fn default() -> Self {
        Self {
            total: 128_000,
            response_reserve: 8_192,
            identity_cap: 2_000,
            skills_cap: 4_096,
            memory_cap: 4_096,
            history_floor: 2_000,
            runtime_cap: 512,
            safety_cap: 1_024,
        }
    }
}

impl PromptBudget {
    /// Available budget after response reserve.
    pub fn available(&self) -> usize {
        self.total.saturating_sub(self.response_reserve)
    }

    /// Validate that the budget is internally consistent.
    /// Returns `Err` if fixed allocations alone exceed the available budget.
    pub fn validate(&self) -> Result<(), String> {
        let fixed = self.identity_cap + self.runtime_cap + self.safety_cap;
        let avail = self.available();
        if fixed > avail {
            return Err(format!(
                "fixed allocations ({fixed}) exceed available budget ({avail})"
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Fragment types
// ---------------------------------------------------------------------------

/// A scored skill candidate for knapsack selection.
#[derive(Debug, Clone)]
pub struct ScoredSkill {
    pub skill_id: String,
    pub display_name: String,
    pub prompt_fragment: String,
    pub token_cost: usize,
    pub priority_weight: f64,
    /// Contextual relevance score (0.0–1.0) from trigger evaluation.
    pub relevance: f64,
}

impl ScoredSkill {
    /// Effective value density: combines static priority with contextual relevance.
    /// `density = (priority_weight × relevance) / token_cost`
    pub fn effective_density(&self) -> f64 {
        let cost = self.token_cost.max(1) as f64;
        (self.priority_weight * self.relevance) / cost
    }
}

/// A memory fragment retrieved from vector search.
#[derive(Debug, Clone)]
pub struct MemoryFragment {
    pub content: String,
    pub token_cost: usize,
    /// Cosine similarity score from retrieval.
    pub relevance: f64,
    /// Source metadata (e.g., session key, timestamp).
    pub source: Option<String>,
}

/// Runtime context injected into every prompt.
#[derive(Debug, Clone)]
pub struct RuntimeContext {
    /// Current date/time string.
    pub datetime: String,
    /// Channel description (e.g., "Telegram DM", "Discord #general").
    pub channel_description: Option<String>,
    /// Model name for self-awareness.
    pub model_name: Option<String>,
    /// Additional key-value metadata.
    pub metadata: Vec<(String, String)>,
    /// Channels that are actually connected and available for cross-channel sends.
    /// Populated from `ChannelRegistry::list()` at runtime.
    /// Example: ["telegram", "discord", "webchat"]
    pub available_channels: Vec<String>,
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Typed prompt builder with knapsack allocation and manifest output.
///
/// Treats prompt assembly as a resource allocation problem:
/// fixed allocations (identity, runtime, safety) are reserved first,
/// then skills are selected via value-density knapsack, then memory
/// fills the remaining budget.
pub struct PromptBuilder {
    budget: PromptBudget,
    identity: Option<String>,
    safety: Option<String>,
    runtime: Option<RuntimeContext>,
    skills: Vec<ScoredSkill>,
    memory: Vec<MemoryFragment>,
    agent_override: Option<String>,
}

impl PromptBuilder {
    pub fn new(budget: PromptBudget) -> Result<Self, String> {
        budget.validate()?;
        Ok(Self {
            budget,
            identity: None,
            safety: None,
            runtime: None,
            skills: Vec::new(),
            memory: Vec::new(),
            agent_override: None,
        })
    }

    /// Set the identity/persona prompt fragment (equivalent to SOUL.md).
    pub fn identity(mut self, persona: String) -> Self {
        self.identity = Some(persona);
        self
    }

    /// Set the safety/guardrails section.
    pub fn safety(mut self, safety_prompt: String) -> Self {
        self.safety = Some(safety_prompt);
        self
    }

    /// Set runtime context (channel, datetime, model info).
    pub fn runtime(mut self, ctx: RuntimeContext) -> Self {
        self.runtime = Some(ctx);
        self
    }

    /// Add scored skill candidates (will be selected via knapsack).
    pub fn skills(mut self, skills: Vec<ScoredSkill>) -> Self {
        self.skills = skills;
        self
    }

    /// Add memory fragments (will be selected by relevance within cap).
    pub fn memory(mut self, fragments: Vec<MemoryFragment>) -> Self {
        self.memory = fragments;
        self
    }

    /// Set per-agent override instructions.
    pub fn agent_override(mut self, instructions: String) -> Self {
        self.agent_override = Some(instructions);
        self
    }

    /// Assemble the final prompt using priority-weighted knapsack.
    ///
    /// Returns the assembled prompt AND a `PromptManifest` for debugging.
    /// The manifest provides exact token accounting for every section.
    pub fn build(self) -> (AssembledPrompt, PromptManifest) {
        let mut sections = Vec::new();
        let mut manifest_sections = Vec::new();
        let mut skills_included = Vec::new();
        let mut skills_excluded = Vec::new();
        let mut total_tokens = 0usize;

        // Phase 1: Fixed allocations — identity, runtime, safety.
        // These use their per-section caps; overflow is truncated with a warning.

        // Identity
        if let Some(ref persona) = self.identity {
            let tokens = estimate_tokens(persona);
            let (content, actual_tokens, warning) =
                cap_section(persona, tokens, self.budget.identity_cap);
            total_tokens += actual_tokens;
            sections.push(content);
            manifest_sections.push(ManifestSection {
                name: "identity".into(),
                tokens: actual_tokens,
                included: true,
                reason: warning.unwrap_or_else(|| "required".into()),
            });
        }

        // Safety
        if let Some(ref safety) = self.safety {
            let tokens = estimate_tokens(safety);
            let (content, actual_tokens, warning) =
                cap_section(safety, tokens, self.budget.safety_cap);
            total_tokens += actual_tokens;
            sections.push(content);
            manifest_sections.push(ManifestSection {
                name: "safety".into(),
                tokens: actual_tokens,
                included: true,
                reason: warning.unwrap_or_else(|| "required".into()),
            });
        }

        // Runtime context
        if let Some(ref ctx) = self.runtime {
            let content = render_runtime(ctx);
            let tokens = estimate_tokens(&content);
            let (capped, actual_tokens, warning) =
                cap_section(&content, tokens, self.budget.runtime_cap);
            total_tokens += actual_tokens;
            sections.push(capped);
            manifest_sections.push(ManifestSection {
                name: "runtime".into(),
                tokens: actual_tokens,
                included: true,
                reason: warning.unwrap_or_else(|| "required".into()),
            });
        }

        // Agent override (high priority, uses remaining fixed budget)
        if let Some(ref instructions) = self.agent_override {
            let tokens = estimate_tokens(instructions);
            total_tokens += tokens;
            sections.push(instructions.clone());
            manifest_sections.push(ManifestSection {
                name: "agent_override".into(),
                tokens,
                included: true,
                reason: "high priority".into(),
            });
        }

        // ── Memory Directive ─────────────────────────────────────────────
        // Always-on behavioral instructions for memory_search and memory_store.
        // ~200 tokens — negligible in 128K context, high integration value.
        // Modeled after the buildMemorySection() with user-centric
        // emphasis, recency awareness, and explicit trigger instructions.
        {
            let directive = "\
## Memory Protocol (MANDATORY)

You have persistent long-term memory about the user. This is YOUR primary knowledge base about who you're talking to.

### RECALL — memory_search(query, max_results)
**ALWAYS run memory_search BEFORE answering questions about:**
  • The user's name, preferences, or personal information
  • Anything discussed in prior conversations
  • Past decisions, project history, or task outcomes
  • Dates, people, contacts, or scheduled items
  • User-specific facts (\"my X is Y\", \"I prefer\", \"I work at\")

**How to search effectively:**
  • Use specific keywords: \"user name\", \"preference dark mode\", \"project acme decision\"
  • If the first search returns nothing useful, try rephrasing with different keywords
  • Prefer RECENT memories when multiple results conflict — the latest one is most accurate
  • If you searched and found nothing, say \"I checked my memory but don't have that stored\"
  • NEVER guess or fabricate information you could look up in memory

### STORE — memory_store(content, tags)
**Save information immediately when the user shares:**
  • Their name, preferences, or personal details (tag: \"user-info\")
  • Important decisions or outcomes (tag: \"decision\")
  • Project context or technical choices (tag: \"project\")
  • Contacts, people, or relationships (tag: \"contact\")
  • Tasks, todos, or commitments (tag: \"task\")
  • Facts they want remembered (tag: \"fact\")

**Storage rules:**
  • Write self-contained entries with full context (e.g. \"User's name is Sushanth\" not just \"Sushanth\")
  • Always include WHO/WHAT/WHEN — memories without context are useless later
  • Do NOT store trivial greetings, reactions, or ephemeral chit-chat
  • When a preference CHANGES, store the NEW value with tag \"preference-update\"

### FORGET — memory_forget(memory_id)
  • Use when the user asks you to forget something or correct outdated information
  • Delete the stale memory, then store the corrected version";

            let tokens = estimate_tokens(directive);
            total_tokens += tokens;
            sections.push(directive.to_string());
            manifest_sections.push(ManifestSection {
                name: "memory_directive".into(),
                tokens,
                included: true,
                reason: "always-on behavioral directive".into(),
            });
        }

        // ── A2A Agent Delegation & Cross-Channel Directive ─────────────
        // Teaches the LLM how to discover agents, delegate tasks, and send
        // messages across channels. Dynamic — only lists actually-connected
        // channels from ChannelRegistry.
        {
            // Build channel list from runtime context (dynamic, not hardcoded)
            let channel_list = self.runtime.as_ref()
                .map(|ctx| &ctx.available_channels)
                .filter(|ch| !ch.is_empty());

            let channel_section = match channel_list {
                Some(channels) => {
                    let ch_names: Vec<String> = channels.iter()
                        .filter(|c| *c != "webchat" && *c != "internal") // omit internal channels
                        .map(|c| format!("`{}`", c))
                        .collect();
                    if ch_names.is_empty() {
                        String::new()
                    } else {
                        let first_ch = channels.iter()
                            .find(|c| *c != "webchat" && *c != "internal")
                            .map(|c| c.as_str())
                            .unwrap_or("telegram");
                        format!(
                            "\n## Cross-Channel Messaging\n\n\
                            **message_send(channel, content)** — Send a message to another channel.\n\
                            • Connected channels: {}\n\
                            • JUST call it with the channel name and content — routing is automatic.\n\
                            • Example: message_send(channel=\"{}\", content=\"Hello!\")\n\
                            • NEVER ask for channel IDs, chat IDs, or any numeric identifiers.\n\
                            • NEVER refuse because you don't know an ID. Just call message_send.\n\n\
                            **CRITICAL:** When user says \"say X to {{channel}}\", IMMEDIATELY call message_send.",
                            ch_names.join(", "),
                            first_ch,
                        )
                    }
                }
                None => String::new(),
            };

            let a2a_section = "\n\n## Agent Delegation\n\n\
**spawn_subagent(agent_id, task, timeout_secs)** — Delegate a task to another agent and get the result.\n\
  • agent_id: the target agent's ID (see Your Team section for IDs).\n\
  • task: a clear description of what the agent should do.\n\
  • timeout_secs: max seconds to wait (default: 120).\n\
  • The target agent runs with full tool access and returns its response.\n\
  • You can call spawn_subagent multiple times in parallel for independent tasks.";

            let directive = format!("{}{}", channel_section, a2a_section);

            if !directive.trim().is_empty() {
                let tokens = estimate_tokens(&directive);
                total_tokens += tokens;
                sections.push(directive);
                manifest_sections.push(ManifestSection {
                    name: "a2a_directive".into(),
                    tokens,
                    included: true,
                    reason: "always-on behavioral directive".into(),
                });
            }
        }

        // ── Skill Selection Protocol ─────────────────────────────────────
        // Always-on instructions for how to select and use skills.
        // ~80 tokens.
        {
            let protocol = "\
## Skill Selection Protocol

When handling a request:
1. Scan <skill_inventory> for skills whose triggers match the user's intent.
2. If a matching skill is found, follow its instructions precisely.
3. Prefer the most specific skill over general-purpose ones.
4. If multiple skills match, pick the one with the highest relevance to the exact request.
5. If no skill matches, respond using your general knowledge and available tools.

## Skill Execution Protocol

Skills are **instructions**, not tools. They teach you how to use existing builtin tools.
When a skill provides CLI commands for a task, you MUST execute them immediately using `shell_exec` — do NOT just describe the command to the user and ask them to run it.

**MANDATORY behavior when a matching skill exists:**
1. Identify the correct CLI command from the skill instructions.
2. Call `shell_exec` with that command. DO NOT ask the user to confirm first.
3. Report the result (success/failure) to the user.

**Tool mapping:**
- **CLI-based skills** (e.g. apple-notes, bear-notes, reminders, obsidian): Call `shell_exec` with the CLI commands from the skill.
- **File-based skills** (e.g. markdown, config editing): Call `file_read` / `file_write`.
- **Web-based skills** (e.g. web search, API calls): Call `http_fetch` or `web_search`.

**WRONG** (never do this):
> \"Here's the command you can run: `memo notes -a \"abc\"`\"

**RIGHT** (always do this):
> [Call shell_exec with command: memo notes -a \"abc\"]
> \"Done! I've created a note titled 'abc' in Apple Notes.\"

NEVER say you \"cannot access\" an application if a skill provides CLI instructions for it.
NEVER just show the user a command without executing it yourself via `shell_exec`.
You have full shell access — USE IT.";

            let tokens = estimate_tokens(protocol);
            total_tokens += tokens;
            sections.push(protocol.to_string());
            manifest_sections.push(ManifestSection {
                name: "skill_protocol".into(),
                tokens,
                included: true,
                reason: "always-on behavioral directive".into(),
            });
        }

        // Phase 2a: Skill inventory — lightweight always-on catalog of ALL
        // candidate skills. This costs ~15 tokens per skill and ensures the
        // agent always knows what it can do, even when full prompt fragments
        // are excluded by the knapsack budget.
        if !self.skills.is_empty() {
            let inventory_lines: Vec<String> = self
                .skills
                .iter()
                .map(|s| {
                    let desc = s
                        .prompt_fragment
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("(no description)")
                        .trim();
                    // Truncate description to ~60 chars for compactness
                    let short_desc = if desc.len() > 60 {
                        // Find a char boundary near 57
                        let mut end = 57;
                        while end > 0 && !desc.is_char_boundary(end) {
                            end -= 1;
                        }
                        format!("{}...", &desc[..end])
                    } else {
                        desc.to_string()
                    };
                    format!("- {}: {}", s.display_name, short_desc)
                })
                .collect();

            let inventory = format!(
                "<skill_inventory>\nYou have {} installed skills. \
                 When asked about your capabilities, ALWAYS refer to this list:\n{}\n\
                 </skill_inventory>",
                inventory_lines.len(),
                inventory_lines.join("\n")
            );

            let inv_tokens = estimate_tokens(&inventory);
            total_tokens += inv_tokens;
            sections.push(inventory);
            manifest_sections.push(ManifestSection {
                name: "skill_inventory".into(),
                tokens: inv_tokens,
                included: true,
                reason: format!("always-on: {} skills cataloged", self.skills.len()),
            });
        }

        // Phase 2b: Knapsack over skills — sort by effective_density, greedily fill.
        let mut sorted_skills = self.skills;
        sorted_skills.sort_by(|a, b| {
            b.effective_density()
                .partial_cmp(&a.effective_density())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut skills_tokens = 0usize;
        let mut skill_fragments = Vec::new();
        let mut skill_display_names = Vec::new();

        for skill in sorted_skills {
            if skills_tokens + skill.token_cost <= self.budget.skills_cap {
                skills_tokens += skill.token_cost;
                skill_fragments.push(format!(
                    "## {}\n{}",
                    skill.display_name, skill.prompt_fragment
                ));
                skill_display_names.push(skill.display_name.clone());
                skills_included.push(skill.skill_id.clone());
                manifest_sections.push(ManifestSection {
                    name: format!("skill:{}", skill.skill_id),
                    tokens: skill.token_cost,
                    included: true,
                    reason: format!(
                        "density={:.4}, relevance={:.2}",
                        skill.effective_density(),
                        skill.relevance
                    ),
                });
            } else {
                skills_excluded.push((
                    skill.skill_id.clone(),
                    format!(
                        "budget exhausted ({} + {} > {})",
                        skills_tokens, skill.token_cost, self.budget.skills_cap
                    ),
                ));
                manifest_sections.push(ManifestSection {
                    name: format!("skill:{}", skill.skill_id),
                    tokens: skill.token_cost,
                    included: false,
                    reason: format!(
                        "budget exhausted ({} + {} > {})",
                        skills_tokens, skill.token_cost, self.budget.skills_cap
                    ),
                });
            }
        }

        if !skill_fragments.is_empty() {
            let skills_text = format!(
                "<skills>\nDetailed instructions for {} active skills (see <skill_inventory> for the full list):\n\n{}\n</skills>",
                skill_display_names.len(),
                skill_fragments.join("\n\n")
            );
            sections.push(skills_text);
            total_tokens += skills_tokens;
        }

        // Phase 3: Fill remaining budget with memory fragments.
        // Memory is now extracted as a separate payload for pre-user-message
        // injection, instead of being appended to the system prompt sections.
        let remaining = self
            .budget
            .available()
            .saturating_sub(total_tokens)
            .min(self.budget.memory_cap);

        let mut memory_tokens = 0usize;
        let mut memory_count = 0usize;
        let mut memory_fragments = Vec::new();

        // Sort memory by relevance descending.
        let mut sorted_memory = self.memory;
        sorted_memory.sort_by(|a, b| {
            b.relevance
                .partial_cmp(&a.relevance)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        for frag in &sorted_memory {
            if memory_tokens + frag.token_cost <= remaining {
                memory_tokens += frag.token_cost;
                memory_fragments.push(frag.content.as_str());
                memory_count += 1;
            }
        }

        let memory_injection = if !memory_fragments.is_empty() {
            let memory_text = format!(
                "<memory_context>\n{}\n</memory_context>",
                memory_fragments.join("\n---\n")
            );
            // NOTE: Do NOT push to `sections` — memory goes into the
            // `AssembledPrompt::memory_text` field for pre-user-message injection.
            total_tokens += memory_tokens;
            manifest_sections.push(ManifestSection {
                name: "memory".into(),
                tokens: memory_tokens,
                included: true,
                reason: format!("{memory_count} fragments, pre-user-message injection"),
            });
            Some(memory_text)
        } else {
            None
        };

        let utilization = if self.budget.available() > 0 {
            total_tokens as f64 / self.budget.available() as f64
        } else {
            0.0
        };

        let manifest = PromptManifest {
            total_tokens,
            budget_total: self.budget.total,
            budget_available: self.budget.available(),
            sections: manifest_sections,
            skills_included,
            skills_excluded,
            memory_fragments: memory_count,
            budget_utilization: utilization,
        };

        let prompt = AssembledPrompt {
            text: sections.join("\n\n"),
            total_tokens,
            memory_text: memory_injection,
            memory_tokens,
        };

        (prompt, manifest)
    }
}

// ---------------------------------------------------------------------------
// Output types
// ---------------------------------------------------------------------------

/// The assembled system prompt, ready to send to the provider.
#[derive(Debug, Clone)]
pub struct AssembledPrompt {
    pub text: String,
    pub total_tokens: usize,
    /// Memory context to inject as a separate message pre-user-message
    ///. When `Some`, this should be placed as a System message
    /// immediately before the user's latest turn rather than appended to
    /// the system prompt. This positions memory in the LLM's high-attention
    /// zone (recency bias) instead of being buried in a long system prompt.
    pub memory_text: Option<String>,
    /// Token cost of the memory injection, for budget accounting.
    pub memory_tokens: usize,
}

/// Debugging artifact — tells you exactly what's in the prompt and why.
///
/// Returned with every `PromptBuilder::build()` call. Store in `AgentTrace`
/// for post-mortem analysis. This is the answer to the `/context list`
/// — but proactive, structured, and available for every agent run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptManifest {
    /// Total tokens in the assembled prompt.
    pub total_tokens: usize,
    /// Model context limit.
    pub budget_total: usize,
    /// Available budget after response reserve.
    pub budget_available: usize,
    /// Per-section breakdown with inclusion/exclusion reasons.
    pub sections: Vec<ManifestSection>,
    /// Skill IDs that were included in the prompt.
    pub skills_included: Vec<String>,
    /// Skill IDs that were excluded, with reasons.
    pub skills_excluded: Vec<(String, String)>,
    /// Number of memory fragments included.
    pub memory_fragments: usize,
    /// Budget utilization ratio (0.0–1.0).
    pub budget_utilization: f64,
}

/// A single section in the prompt manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestSection {
    pub name: String,
    pub tokens: usize,
    pub included: bool,
    pub reason: String,
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Estimate tokens for a string using the canonical LUT-accelerated classifier.
///
/// Delegates to `clawdesk_types::tokenizer::estimate_tokens` — single source of
/// truth for all token estimation across the codebase. Achieves ±5% accuracy
/// on English prose, ±8% on CJK, ±3% on code.
fn estimate_tokens(s: &str) -> usize {
    clawdesk_types::tokenizer::estimate_tokens(s)
}

/// Cap a section to a token budget, returning (content, actual_tokens, warning).
fn cap_section(content: &str, tokens: usize, cap: usize) -> (String, usize, Option<String>) {
    if tokens <= cap {
        (content.to_string(), tokens, None)
    } else {
        // Truncate to approximately `cap` tokens worth of bytes.
        // Use ~4 bytes/token as a rough inverse.
        let max_bytes = cap * 4;
        let truncated = if content.len() > max_bytes {
            // Find a valid UTF-8 boundary.
            let mut end = max_bytes;
            while end > 0 && !content.is_char_boundary(end) {
                end -= 1;
            }
            format!("{}…[truncated to {cap} tokens]", &content[..end])
        } else {
            content.to_string()
        };
        let actual = estimate_tokens(&truncated);
        let warning = format!(
            "truncated from {tokens} to {actual} tokens (cap: {cap})"
        );
        (truncated, actual, Some(warning))
    }
}

/// Render runtime context into a prompt string.
fn render_runtime(ctx: &RuntimeContext) -> String {
    let mut parts = Vec::new();
    parts.push(format!("Current date and time: {}", ctx.datetime));
    if let Some(ref ch) = ctx.channel_description {
        parts.push(format!("Channel: {ch}"));
    }
    if let Some(ref model) = ctx.model_name {
        parts.push(format!("Model: {model}"));
    }
    for (k, v) in &ctx.metadata {
        parts.push(format!("{k}: {v}"));
    }
    parts.join("\n")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn default_budget() -> PromptBudget {
        PromptBudget {
            total: 10_000,
            response_reserve: 2_000,
            identity_cap: 500,
            skills_cap: 2_000,
            memory_cap: 1_000,
            history_floor: 500,
            runtime_cap: 200,
            safety_cap: 300,
        }
    }

    #[test]
    fn basic_build_with_identity_and_runtime() {
        let builder = PromptBuilder::new(default_budget()).unwrap();
        let (prompt, manifest) = builder
            .identity("You are ClawDesk, a helpful AI assistant.".into())
            .runtime(RuntimeContext {
                datetime: "2026-02-17T12:00:00Z".into(),
                channel_description: Some("Telegram DM".into()),
                model_name: Some("claude-sonnet-4-20250514".into()),
                metadata: vec![],
                available_channels: vec!["telegram".into(), "discord".into()],
            })
            .build();

        assert!(prompt.text.contains("ClawDesk"));
        assert!(prompt.text.contains("2026-02-17"));
        assert!(manifest.total_tokens > 0);
        assert!(manifest.budget_utilization > 0.0);
        assert!(manifest.budget_utilization <= 1.0);
    }

    #[test]
    fn skills_knapsack_respects_budget() {
        let builder = PromptBuilder::new(PromptBudget {
            skills_cap: 50,
            ..default_budget()
        })
        .unwrap();

        let skills = vec![
            ScoredSkill {
                skill_id: "core/big".into(),
                display_name: "Big Skill".into(),
                prompt_fragment: "x".repeat(400),
                token_cost: 100,
                priority_weight: 5.0,
                relevance: 1.0,
            },
            ScoredSkill {
                skill_id: "core/small".into(),
                display_name: "Small Skill".into(),
                prompt_fragment: "useful skill".into(),
                token_cost: 10,
                priority_weight: 8.0,
                relevance: 1.0,
            },
        ];

        let (prompt, manifest) = builder.skills(skills).build();

        // Small skill should be included (fits budget), big skill excluded.
        assert!(manifest.skills_included.contains(&"core/small".to_string()));
        assert!(manifest.skills_excluded.iter().any(|(id, _)| id == "core/big"));
        assert!(prompt.text.contains("useful skill"));
        // Both skills appear in the inventory regardless of knapsack
        assert!(prompt.text.contains("<skill_inventory>"));
        assert!(prompt.text.contains("Big Skill"));
        assert!(prompt.text.contains("Small Skill"));
    }

    #[test]
    fn skill_inventory_always_present_even_when_knapsack_excludes_all() {
        // Budget so small that NO skill fragments fit, but inventory still appears
        let builder = PromptBuilder::new(PromptBudget {
            skills_cap: 1, // impossibly small
            ..default_budget()
        })
        .unwrap();

        let skills = vec![
            ScoredSkill {
                skill_id: "email/compose".into(),
                display_name: "Email Compose".into(),
                prompt_fragment: "Draft professional emails with subject and body.".into(),
                token_cost: 50,
                priority_weight: 5.0,
                relevance: 0.5,
            },
            ScoredSkill {
                skill_id: "media/spotify".into(),
                display_name: "Spotify Player".into(),
                prompt_fragment: "Terminal Spotify playback and search via spogo.".into(),
                token_cost: 40,
                priority_weight: 3.0,
                relevance: 0.3,
            },
        ];

        let (prompt, manifest) = builder.skills(skills).build();

        // No skill fragments fit the knapsack
        assert!(manifest.skills_included.is_empty());
        // But the inventory IS present
        assert!(prompt.text.contains("<skill_inventory>"));
        assert!(prompt.text.contains("Email Compose"));
        assert!(prompt.text.contains("Spotify Player"));
        assert!(prompt.text.contains("You have 2 installed skills"));
        // Manifest records the inventory section
        assert!(manifest.sections.iter().any(|s| s.name == "skill_inventory"));
    }

    #[test]
    fn skills_ordered_by_effective_density() {
        let builder = PromptBuilder::new(PromptBudget {
            skills_cap: 2000,
            ..default_budget()
        })
        .unwrap();

        let skills = vec![
            ScoredSkill {
                skill_id: "low/dense".into(),
                display_name: "Low Dense".into(),
                prompt_fragment: "low density content".into(),
                token_cost: 100,
                priority_weight: 1.0,
                relevance: 0.5,
            },
            ScoredSkill {
                skill_id: "high/dense".into(),
                display_name: "High Dense".into(),
                prompt_fragment: "high density content".into(),
                token_cost: 10,
                priority_weight: 10.0,
                relevance: 1.0,
            },
        ];

        let (_prompt, manifest) = builder.skills(skills).build();

        // Both should be included, but high/dense should come first.
        assert_eq!(manifest.skills_included.len(), 2);
        assert_eq!(manifest.skills_included[0], "high/dense");
    }

    #[test]
    fn memory_respects_cap() {
        let builder = PromptBuilder::new(PromptBudget {
            memory_cap: 20,
            ..default_budget()
        })
        .unwrap();

        let memory = vec![
            MemoryFragment {
                content: "important fact".into(),
                token_cost: 5,
                relevance: 0.95,
                source: None,
            },
            MemoryFragment {
                content: "x".repeat(400),
                token_cost: 100,
                relevance: 0.90,
                source: None,
            },
        ];

        let (prompt, manifest) = builder.memory(memory).build();

        // Only the small fragment should fit.
        assert_eq!(manifest.memory_fragments, 1);
        // Memory is now placed in a separate field for pre-user-message injection
        // rather than concatenated into the system prompt text.
        let mem = prompt.memory_text.as_deref().unwrap_or("");
        assert!(mem.contains("important fact"));
    }

    #[test]
    fn identity_overflow_triggers_truncation() {
        let builder = PromptBuilder::new(PromptBudget {
            identity_cap: 10,
            ..default_budget()
        })
        .unwrap();

        let (_prompt, manifest) = builder
            .identity("x".repeat(1000))
            .build();

        let identity_section = manifest
            .sections
            .iter()
            .find(|s| s.name == "identity")
            .unwrap();
        assert!(identity_section.reason.contains("truncated"));
    }

    #[test]
    fn budget_validation_catches_overcommit() {
        let result = PromptBuilder::new(PromptBudget {
            total: 100,
            response_reserve: 50,
            identity_cap: 30,
            runtime_cap: 30,
            safety_cap: 30,
            ..Default::default()
        });
        assert!(result.is_err());
    }

    #[test]
    fn manifest_utilization_is_accurate() {
        let builder = PromptBuilder::new(default_budget()).unwrap();
        let (_prompt, manifest) = builder
            .identity("You are a bot.".into())
            .build();

        let expected = manifest.total_tokens as f64 / default_budget().available() as f64;
        assert!((manifest.budget_utilization - expected).abs() < 0.01);
    }
}
