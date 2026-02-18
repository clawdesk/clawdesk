//! Structured agent workspace with token budgeting.
//!
//! Each agent execution gets a workspace that manages:
//!
//! - `context/` — structured context files with priority-weighted token allocation
//! - `references/` — on-demand RAG retrieval for additional context
//! - Token budget — ensures the assembled prompt fits within model limits
//!
//! ```text
//! workspace/
//! ├── context/           # priority-ordered context files
//! │   ├── 00-system.md   # system prompt (priority: 100)
//! │   ├── 10-persona.md  # agent persona (priority: 90)
//! │   ├── 20-skills.md   # active skill prompts (priority: 80)
//! │   └── 30-history.md  # conversation history (priority: 50)
//! └── references/        # on-demand RAG
//!     ├── codebase.idx
//!     └── docs.idx
//! ```

use clawdesk_types::estimate_tokens;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Workspace context
// ---------------------------------------------------------------------------

/// A single context entry with priority and content.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextEntry {
    /// Unique key (e.g. "system", "persona", "skills", "history").
    pub key: String,
    /// Priority (0–100). Higher priority entries are included first.
    pub priority: u32,
    /// Human-readable label for the section.
    pub label: String,
    /// The content.
    pub content: String,
    /// Whether this entry is required (cannot be dropped).
    #[serde(default)]
    pub required: bool,
    /// Estimated token count (computed lazily).
    #[serde(default)]
    pub estimated_tokens: usize,
}

impl ContextEntry {
    /// Create a new context entry, auto-estimating tokens.
    pub fn new(key: &str, label: &str, content: &str, priority: u32, required: bool) -> Self {
        let estimated_tokens = estimate_tokens(content);
        Self {
            key: key.to_string(),
            label: label.to_string(),
            content: content.to_string(),
            priority,
            required,
            estimated_tokens,
        }
    }
}

// Token estimation consolidated in clawdesk_types::tokenizer::estimate_tokens

// ---------------------------------------------------------------------------
// Token budget
// ---------------------------------------------------------------------------

/// Token budget configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenBudget {
    /// Maximum total tokens for the assembled prompt.
    pub max_tokens: usize,
    /// Reserve this many tokens for the model's response.
    #[serde(default = "default_response_reserve")]
    pub response_reserve: usize,
    /// Reserve this many tokens for tool outputs.
    #[serde(default = "default_tool_reserve")]
    pub tool_reserve: usize,
}

fn default_response_reserve() -> usize { 4096 }
fn default_tool_reserve() -> usize { 2048 }

impl Default for TokenBudget {
    fn default() -> Self {
        Self {
            max_tokens: 128_000,
            response_reserve: default_response_reserve(),
            tool_reserve: default_tool_reserve(),
        }
    }
}

impl TokenBudget {
    /// Available tokens for context assembly.
    pub fn available(&self) -> usize {
        self.max_tokens
            .saturating_sub(self.response_reserve)
            .saturating_sub(self.tool_reserve)
    }
}

// ---------------------------------------------------------------------------
// Workspace
// ---------------------------------------------------------------------------

/// Agent workspace that manages context assembly.
#[derive(Debug, Clone)]
pub struct AgentWorkspace {
    /// Context entries keyed by their key.
    entries: BTreeMap<String, ContextEntry>,
    /// Token budget.
    pub budget: TokenBudget,
}

impl AgentWorkspace {
    /// Create a new workspace with the given budget.
    pub fn new(budget: TokenBudget) -> Self {
        Self {
            entries: BTreeMap::new(),
            budget,
        }
    }

    /// Create with default budget.
    pub fn with_defaults() -> Self {
        Self::new(TokenBudget::default())
    }

    /// Add or replace a context entry.
    pub fn set(&mut self, entry: ContextEntry) {
        self.entries.insert(entry.key.clone(), entry);
    }

    /// Remove a context entry.
    pub fn remove(&mut self, key: &str) -> Option<ContextEntry> {
        self.entries.remove(key)
    }

    /// Get a context entry.
    pub fn get(&self, key: &str) -> Option<&ContextEntry> {
        self.entries.get(key)
    }

    /// Total estimated tokens across all entries.
    pub fn total_tokens(&self) -> usize {
        self.entries.values().map(|e| e.estimated_tokens).sum()
    }

    /// Assemble the context within token budget.
    ///
    /// Algorithm:
    /// 1. Sort entries by priority (descending).
    /// 2. Include required entries first (always included).
    /// 3. Include optional entries in priority order until budget is exhausted.
    /// 4. If an optional entry exceeds remaining budget, try truncation.
    pub fn assemble(&self) -> AssembledContext {
        let available = self.budget.available();
        let mut included = Vec::new();
        let mut dropped = Vec::new();
        let mut used_tokens = 0;

        // Sort by priority descending.
        let mut sorted: Vec<&ContextEntry> = self.entries.values().collect();
        sorted.sort_by(|a, b| b.priority.cmp(&a.priority));

        // Pass 1: required entries.
        for entry in &sorted {
            if entry.required {
                used_tokens += entry.estimated_tokens;
                included.push(IncludedEntry {
                    key: entry.key.clone(),
                    label: entry.label.clone(),
                    content: entry.content.clone(),
                    tokens: entry.estimated_tokens,
                    truncated: false,
                });
            }
        }

        // Pass 2: optional entries by priority.
        for entry in &sorted {
            if entry.required {
                continue;
            }

            if used_tokens + entry.estimated_tokens <= available {
                used_tokens += entry.estimated_tokens;
                included.push(IncludedEntry {
                    key: entry.key.clone(),
                    label: entry.label.clone(),
                    content: entry.content.clone(),
                    tokens: entry.estimated_tokens,
                    truncated: false,
                });
            } else {
                // Try truncation: fit as much as we can.
                let remaining = available.saturating_sub(used_tokens);
                if remaining > 100 {
                    // At least 100 tokens to be useful.
                    let char_budget = remaining * 4;
                    let truncated_content = if entry.content.len() > char_budget {
                        let suffix = "\n\n[... truncated due to token budget]";
                        let cut = char_budget.saturating_sub(suffix.len());
                        format!("{}{}", &entry.content[..cut], suffix)
                    } else {
                        entry.content.clone()
                    };
                    let truncated_tokens = estimate_tokens(&truncated_content);
                    used_tokens += truncated_tokens;
                    included.push(IncludedEntry {
                        key: entry.key.clone(),
                        label: entry.label.clone(),
                        content: truncated_content,
                        tokens: truncated_tokens,
                        truncated: true,
                    });
                } else {
                    dropped.push(DroppedEntry {
                        key: entry.key.clone(),
                        label: entry.label.clone(),
                        tokens: entry.estimated_tokens,
                        reason: "Insufficient token budget".into(),
                    });
                }
            }
        }

        AssembledContext {
            entries: included,
            dropped,
            total_tokens: used_tokens,
            budget_available: available,
        }
    }
}

/// Result of context assembly.
#[derive(Debug, Clone)]
pub struct AssembledContext {
    /// Included entries in priority order.
    pub entries: Vec<IncludedEntry>,
    /// Dropped entries.
    pub dropped: Vec<DroppedEntry>,
    /// Total tokens used.
    pub total_tokens: usize,
    /// Total budget available.
    pub budget_available: usize,
}

impl AssembledContext {
    /// Produce the concatenated prompt text.
    pub fn to_prompt(&self) -> String {
        self.entries
            .iter()
            .map(|e| format!("<!-- {} -->\n{}", e.label, e.content))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Utilisation percentage.
    pub fn utilisation_pct(&self) -> f64 {
        if self.budget_available == 0 {
            return 0.0;
        }
        (self.total_tokens as f64 / self.budget_available as f64) * 100.0
    }
}

/// An entry that was included in the assembled context.
#[derive(Debug, Clone)]
pub struct IncludedEntry {
    pub key: String,
    pub label: String,
    pub content: String,
    pub tokens: usize,
    pub truncated: bool,
}

/// An entry that was dropped due to budget constraints.
#[derive(Debug, Clone)]
pub struct DroppedEntry {
    pub key: String,
    pub label: String,
    pub tokens: usize,
    pub reason: String,
}

// ---------------------------------------------------------------------------
// Reference index (placeholder for RAG)
// ---------------------------------------------------------------------------

/// A reference source for on-demand retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceSource {
    /// Source identifier.
    pub id: String,
    /// Source type.
    pub source_type: ReferenceType,
    /// Path to index or data.
    pub path: String,
    /// Whether this source is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool { true }

/// Type of reference source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReferenceType {
    /// Local codebase files.
    Codebase,
    /// Documentation files.
    Documentation,
    /// External API references.
    Api,
    /// Custom embedding index.
    Custom,
}

/// A retrieved reference chunk.
#[derive(Debug, Clone)]
pub struct ReferenceChunk {
    pub source_id: String,
    pub content: String,
    pub relevance_score: f64,
    pub estimated_tokens: usize,
}

/// Simulate RAG retrieval: in production this queries an embedding index.
/// Here, returns empty (placeholder).
pub fn retrieve_references(
    _sources: &[ReferenceSource],
    _query: &str,
    _max_tokens: usize,
) -> Vec<ReferenceChunk> {
    // Placeholder: production implementation would query vector DB.
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens("hello"), 2); // 5 chars → 2 tokens
        assert_eq!(estimate_tokens(""), 0); // empty → 0 tokens
    }

    #[test]
    fn test_token_budget_available() {
        let budget = TokenBudget {
            max_tokens: 100_000,
            response_reserve: 4096,
            tool_reserve: 2048,
        };
        assert_eq!(budget.available(), 93_856);
    }

    #[test]
    fn test_workspace_basic_assembly() {
        let mut ws = AgentWorkspace::new(TokenBudget {
            max_tokens: 10_000,
            response_reserve: 1000,
            tool_reserve: 500,
        });

        ws.set(ContextEntry::new("system", "System", "You are a helpful assistant.", 100, true));
        ws.set(ContextEntry::new("persona", "Persona", "You specialise in UX design.", 90, false));
        ws.set(ContextEntry::new("history", "History", "User: Hi\nAssistant: Hello!", 50, false));

        let assembled = ws.assemble();
        assert_eq!(assembled.entries.len(), 3);
        assert!(assembled.dropped.is_empty());
        assert!(!assembled.to_prompt().is_empty());
    }

    #[test]
    fn test_required_always_included() {
        let mut ws = AgentWorkspace::new(TokenBudget {
            max_tokens: 100, // Very small budget.
            response_reserve: 0,
            tool_reserve: 0,
        });

        let big_content = "x".repeat(200);
        ws.set(ContextEntry::new("required", "Required", &big_content, 100, true));

        let assembled = ws.assemble();
        assert!(assembled.entries.iter().any(|e| e.key == "required"));
    }

    #[test]
    fn test_priority_ordering() {
        let mut ws = AgentWorkspace::new(TokenBudget {
            max_tokens: 10_000,
            response_reserve: 0,
            tool_reserve: 0,
        });

        ws.set(ContextEntry::new("low", "Low", "low priority content", 10, false));
        ws.set(ContextEntry::new("high", "High", "high priority content", 90, false));
        ws.set(ContextEntry::new("mid", "Mid", "mid priority content", 50, false));

        let assembled = ws.assemble();
        // First non-required entry should be highest priority.
        assert_eq!(assembled.entries[0].key, "high");
    }

    #[test]
    fn test_drop_low_priority() {
        let mut ws = AgentWorkspace::new(TokenBudget {
            max_tokens: 200,
            response_reserve: 0,
            tool_reserve: 0,
        });

        // Add entries that exceed budget.
        ws.set(ContextEntry::new("high", "High", &"a".repeat(600), 90, false));
        ws.set(ContextEntry::new("low", "Low", &"b".repeat(600), 10, false));

        let assembled = ws.assemble();
        // Low priority should be dropped or truncated.
        let has_low_full = assembled
            .entries
            .iter()
            .any(|e| e.key == "low" && !e.truncated);
        assert!(!has_low_full);
    }

    #[test]
    fn test_utilisation_pct() {
        let mut ws = AgentWorkspace::new(TokenBudget {
            max_tokens: 1000,
            response_reserve: 0,
            tool_reserve: 0,
        });

        ws.set(ContextEntry::new("a", "A", &"x".repeat(400), 50, false));

        let assembled = ws.assemble();
        assert!(assembled.utilisation_pct() > 0.0);
        assert!(assembled.utilisation_pct() <= 100.0);
    }

    #[test]
    fn test_set_and_remove() {
        let mut ws = AgentWorkspace::with_defaults();
        ws.set(ContextEntry::new("test", "Test", "content", 50, false));
        assert!(ws.get("test").is_some());

        ws.remove("test");
        assert!(ws.get("test").is_none());
    }

    #[test]
    fn test_total_tokens() {
        let mut ws = AgentWorkspace::with_defaults();
        ws.set(ContextEntry::new("a", "A", "hello", 50, false));
        ws.set(ContextEntry::new("b", "B", "world", 50, false));
        assert!(ws.total_tokens() > 0);
    }

    #[test]
    fn test_reference_type_serialization() {
        let source = ReferenceSource {
            id: "codebase".into(),
            source_type: ReferenceType::Codebase,
            path: "/project".into(),
            enabled: true,
        };
        let json = serde_json::to_string(&source).unwrap();
        assert!(json.contains("codebase"));
    }

    #[test]
    fn test_retrieve_references_placeholder() {
        let sources = vec![ReferenceSource {
            id: "test".into(),
            source_type: ReferenceType::Documentation,
            path: "/docs".into(),
            enabled: true,
        }];
        let chunks = retrieve_references(&sources, "query", 1000);
        assert!(chunks.is_empty()); // placeholder returns empty
    }
}
