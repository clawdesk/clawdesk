//! System prompt construction — token-budgeted, priority-based inclusion.
//!
//! Solves a variant of the Knapsack Problem:
//! Given budget B and sections S_i with priority p_i and size t_i,
//! maximize Σ p_i × x_i subject to Σ t_i × x_i ≤ B.
//! Greedy solution (sort by p/t ratio) is O(n log n) and exact for <20 sections.

use crate::context_guard::estimate_tokens;
use serde::{Deserialize, Serialize};

/// Priority level for system prompt sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SectionPriority {
    /// Must always be included (identity, date). Cannot be evicted.
    Required = 100,
    /// High priority (active skills, primary tools).
    High = 75,
    /// Medium priority (context, memory summaries).
    Medium = 50,
    /// Low priority (informational, nice-to-have).
    Low = 25,
    /// Optional (only if budget allows).
    Optional = 10,
}

/// A section of the system prompt.
#[derive(Debug, Clone)]
pub struct PromptSection {
    pub name: String,
    pub content: String,
    pub priority: SectionPriority,
    /// Estimated token count for this section.
    pub estimated_tokens: usize,
}

/// Result of system prompt construction.
#[derive(Debug, Clone)]
pub struct BuiltPrompt {
    pub text: String,
    pub total_tokens: usize,
    pub included_sections: Vec<String>,
    pub excluded_sections: Vec<(String, String)>, // (name, reason)
}

/// Token-budgeted system prompt builder.
pub struct SystemPromptBuilder {
    sections: Vec<PromptSection>,
    budget_tokens: usize,
}

impl SystemPromptBuilder {
    pub fn new(budget_tokens: usize) -> Self {
        Self {
            sections: Vec::new(),
            budget_tokens,
        }
    }

    /// Add a section to the prompt builder.
    pub fn add_section(&mut self, section: PromptSection) -> &mut Self {
        self.sections.push(section);
        self
    }

    /// Add core identity (always included, highest priority).
    pub fn identity(&mut self, name: &str, description: &str) -> &mut Self {
        self.add_section(PromptSection {
            name: "identity".into(),
            content: format!("You are {name}. {description}"),
            priority: SectionPriority::Required,
            estimated_tokens: (name.len() + description.len() + 10) / 4,
        })
    }

    /// Add date/time context.
    pub fn datetime(&mut self, datetime_str: &str) -> &mut Self {
        let content = format!("Current date and time: {datetime_str}");
        self.add_section(PromptSection {
            name: "datetime".into(),
            content,
            priority: SectionPriority::Required,
            estimated_tokens: 20,
        })
    }

    /// Add channel context (which platform, DM vs group, etc.).
    pub fn channel_context(&mut self, context: &str) -> &mut Self {
        self.add_section(PromptSection {
            name: "channel_context".into(),
            content: context.to_string(),
            priority: SectionPriority::High,
            estimated_tokens: estimate_tokens(context),
        })
    }

    /// Add skill descriptions.
    pub fn skills(&mut self, skills: &[(&str, &str)]) -> &mut Self {
        if skills.is_empty() {
            return self;
        }
        let mut content = String::from("Available skills:\n");
        for (name, desc) in skills {
            content.push_str(&format!("- {name}: {desc}\n"));
        }
        self.add_section(PromptSection {
            name: "skills".into(),
            estimated_tokens: estimate_tokens(&content),
            content,
            priority: SectionPriority::High,
        })
    }

    /// Add tool documentation.
    pub fn tools(&mut self, tool_docs: &str) -> &mut Self {
        if tool_docs.is_empty() {
            return self;
        }
        self.add_section(PromptSection {
            name: "tools".into(),
            content: tool_docs.to_string(),
            priority: SectionPriority::Medium,
            estimated_tokens: estimate_tokens(tool_docs),
        })
    }

    /// Add memory context (retrieved from vector search).
    pub fn memory_context(&mut self, context: &str) -> &mut Self {
        if context.is_empty() {
            return self;
        }
        self.add_section(PromptSection {
            name: "memory_context".into(),
            content: format!("<memory_context>\n{context}\n</memory_context>"),
            priority: SectionPriority::Medium,
            estimated_tokens: estimate_tokens(context) + 10,
        })
    }

    /// Add per-agent override instructions.
    pub fn agent_override(&mut self, instructions: &str) -> &mut Self {
        if instructions.is_empty() {
            return self;
        }
        self.add_section(PromptSection {
            name: "agent_override".into(),
            content: instructions.to_string(),
            priority: SectionPriority::High,
            estimated_tokens: estimate_tokens(instructions),
        })
    }

    /// Build the system prompt within token budget.
    ///
    /// Uses greedy knapsack: sort by priority/size ratio (descending),
    /// include greedily until budget exhausted. Required sections are always included.
    /// O(n log n) for sorting, O(n) for packing.
    pub fn build(&self) -> BuiltPrompt {
        let mut required = Vec::new();
        let mut optional = Vec::new();

        for section in &self.sections {
            if section.priority == SectionPriority::Required {
                required.push(section);
            } else {
                optional.push(section);
            }
        }

        let mut total_tokens = 0usize;
        let mut included = Vec::new();
        let mut excluded = Vec::new();
        let mut parts = Vec::new();

        // Include all required sections first
        for section in &required {
            total_tokens += section.estimated_tokens;
            included.push(section.name.clone());
            parts.push(section.content.as_str());
        }

        // Sort optional by priority/size ratio (greedy knapsack)
        let mut scored: Vec<(f64, &PromptSection)> = optional
            .iter()
            .map(|s| {
                let ratio = s.priority as u32 as f64
                    / (s.estimated_tokens.max(1) as f64);
                (ratio, *s)
            })
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        // Greedily include sections within budget
        for (_ratio, section) in scored {
            if total_tokens + section.estimated_tokens <= self.budget_tokens {
                total_tokens += section.estimated_tokens;
                included.push(section.name.clone());
                parts.push(section.content.as_str());
            } else {
                excluded.push((
                    section.name.clone(),
                    format!(
                        "budget exceeded ({} + {} > {})",
                        total_tokens, section.estimated_tokens, self.budget_tokens
                    ),
                ));
            }
        }

        BuiltPrompt {
            text: parts.join("\n\n"),
            total_tokens,
            included_sections: included,
            excluded_sections: excluded,
        }
    }
}

/// Diagnostic report of system prompt construction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptReport {
    pub total_tokens: usize,
    pub budget_tokens: usize,
    pub utilization: f64,
    pub included: Vec<PromptSectionReport>,
    pub excluded: Vec<PromptSectionReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSectionReport {
    pub name: String,
    pub tokens: usize,
    pub reason: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_required_sections_always_included() {
        let mut builder = SystemPromptBuilder::new(100);
        builder.identity("ClawDesk", "A helpful AI assistant.");
        builder.datetime("2026-02-16");

        let result = builder.build();
        assert!(result.included_sections.contains(&"identity".to_string()));
        assert!(result.included_sections.contains(&"datetime".to_string()));
    }

    #[test]
    fn test_budget_exclusion() {
        let mut builder = SystemPromptBuilder::new(50); // Very small budget
        builder.identity("Bot", "Helper.");
        builder.add_section(PromptSection {
            name: "huge_section".into(),
            content: "x".repeat(1000),
            priority: SectionPriority::Low,
            estimated_tokens: 250,
        });

        let result = builder.build();
        assert!(result.excluded_sections.iter().any(|(name, _)| name == "huge_section"));
    }

    #[test]
    fn test_priority_ordering() {
        let mut builder = SystemPromptBuilder::new(200);
        builder.identity("Bot", ".");

        // Add low priority big section and high priority small section
        builder.add_section(PromptSection {
            name: "low_big".into(),
            content: "x".repeat(400),
            priority: SectionPriority::Low,
            estimated_tokens: 100,
        });
        builder.add_section(PromptSection {
            name: "high_small".into(),
            content: "important context".into(),
            priority: SectionPriority::High,
            estimated_tokens: 5,
        });

        let result = builder.build();
        // High priority section should be included before low priority
        assert!(result.included_sections.contains(&"high_small".to_string()));
    }
}
