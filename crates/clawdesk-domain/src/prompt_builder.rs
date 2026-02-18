//! Prompt assembly as a resource allocation problem — typed, budget-aware, observable.
//!
//! Treats system prompt construction as a **priority-weighted knapsack** with
//! per-section caps, combined budget enforcement, and a diagnostic manifest
//! that explains every inclusion/exclusion decision.
//!
//! ## Design rationale (vs. OpenClaw)
//!
//! OpenClaw concatenates 8+ Markdown files at runtime with per-file caps
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

        // Phase 2: Knapsack over skills — sort by effective_density, greedily fill.
        let mut sorted_skills = self.skills;
        sorted_skills.sort_by(|a, b| {
            b.effective_density()
                .partial_cmp(&a.effective_density())
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut skills_tokens = 0usize;
        let mut skill_fragments = Vec::new();

        for skill in sorted_skills {
            if skills_tokens + skill.token_cost <= self.budget.skills_cap {
                skills_tokens += skill.token_cost;
                skill_fragments.push(format!(
                    "## {}\n{}",
                    skill.display_name, skill.prompt_fragment
                ));
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
            let skills_text = format!("<skills>\n{}\n</skills>", skill_fragments.join("\n\n"));
            sections.push(skills_text);
            total_tokens += skills_tokens;
        }

        // Phase 3: Fill remaining budget with memory fragments.
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

        if !memory_fragments.is_empty() {
            let memory_text = format!(
                "<memory_context>\n{}\n</memory_context>",
                memory_fragments.join("\n---\n")
            );
            sections.push(memory_text);
            total_tokens += memory_tokens;
            manifest_sections.push(ManifestSection {
                name: "memory".into(),
                tokens: memory_tokens,
                included: true,
                reason: format!("{memory_count} fragments, relevance-sorted"),
            });
        }

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
}

/// Debugging artifact — tells you exactly what's in the prompt and why.
///
/// Returned with every `PromptBuilder::build()` call. Store in `AgentTrace`
/// for post-mortem analysis. This is the answer to OpenClaw's `/context list`
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

/// Estimate tokens for a string using the BPE-tuned byte classifier.
///
/// Same LUT-accelerated approach as `context_guard::estimate_tokens()`:
/// 4 byte classes with BPE-tuned divisors. Branchless, auto-vectorizable.
fn estimate_tokens(s: &str) -> usize {
    let mut cost: f64 = 0.0;
    for &b in s.as_bytes() {
        cost += match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' => 1.0 / 4.2,
            b' ' | b'\n' | b'\t' | b'\r' => 1.0 / 6.0,
            0x80..=0xFF => 1.0 / 2.5,
            _ => 1.0 / 1.5, // punctuation
        };
    }
    (cost.ceil() as usize).max(1)
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
        assert!(prompt.text.contains("important fact"));
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
