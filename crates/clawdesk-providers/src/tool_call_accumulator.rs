//! Streaming tool-call accumulator — handles interleaved partial deltas.
//!
//! LLM providers stream tool calls as interleaved fragments indexed by position.
//! This module accumulates partial deltas into complete tool calls, handling:
//!
//! - **Interleaved indices**: Multiple tool calls in-flight simultaneously,
//!   deltas arriving out-of-order by index.
//! - **JSON fragment assembly**: Partial argument strings concatenated in-order.
//! - **Completion detection**: Tool call is complete when `input_json_delta`
//!   stops arriving and the accumulated JSON parses successfully.
//!
//! ## Architecture
//!
//! `ToolCallAccumulator` uses a `HashMap<usize, PartialToolCall>` keyed by
//! the tool-call index. Each delta appends to the correct slot. When a
//! content_block_stop event arrives (or stream ends), the accumulated
//! fragments are validated and emitted.
//!
//! ## Complexity
//! - Per delta: O(1) amortised (HashMap lookup + string append)
//! - Finalization: O(K) where K = number of tool calls
//! - Memory: O(total_argument_bytes) across all in-flight tool calls

use std::collections::HashMap;
use serde::{Deserialize, Serialize};

/// A complete, validated tool call extracted from streaming deltas.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletedToolCall {
    /// Tool call ID (e.g., `toolu_01abc...` for Anthropic).
    pub id: String,
    /// Tool name (e.g., `get_weather`).
    pub name: String,
    /// Complete JSON arguments string.
    pub arguments_json: String,
    /// Parsed arguments (if valid JSON).
    pub arguments: Option<serde_json::Value>,
    /// The index position in the response.
    pub index: usize,
}

/// A partial tool call being accumulated.
#[derive(Debug, Clone)]
struct PartialToolCall {
    /// Tool call ID.
    id: Option<String>,
    /// Tool name.
    name: Option<String>,
    /// Accumulated JSON argument fragments.
    arguments_buffer: String,
    /// Number of deltas received for this tool call.
    delta_count: usize,
    /// Whether this tool call has received its stop signal.
    stopped: bool,
}

impl PartialToolCall {
    fn new() -> Self {
        Self {
            id: None,
            name: None,
            arguments_buffer: String::new(),
            delta_count: 0,
            stopped: false,
        }
    }
}

/// Accumulates streaming tool-call deltas into complete tool calls.
///
/// ## Usage
///
/// ```ignore
/// let mut acc = ToolCallAccumulator::new();
///
/// // Feed deltas as they arrive from the stream
/// acc.push_id(0, "toolu_01abc");
/// acc.push_name(0, "get_weather");
/// acc.push_arguments_delta(0, "{\"city\":");
/// acc.push_arguments_delta(0, "\"London\"}");
/// acc.mark_stopped(0);
///
/// // Finalize and get completed tool calls
/// let completed = acc.finalize();
/// ```
pub struct ToolCallAccumulator {
    slots: HashMap<usize, PartialToolCall>,
}

impl ToolCallAccumulator {
    pub fn new() -> Self {
        Self {
            slots: HashMap::new(),
        }
    }

    /// Set the tool call ID for the given index.
    pub fn push_id(&mut self, index: usize, id: &str) {
        let slot = self.slots.entry(index).or_insert_with(PartialToolCall::new);
        slot.id = Some(id.to_string());
        slot.delta_count += 1;
    }

    /// Set the tool name for the given index.
    pub fn push_name(&mut self, index: usize, name: &str) {
        let slot = self.slots.entry(index).or_insert_with(PartialToolCall::new);
        slot.name = Some(name.to_string());
        slot.delta_count += 1;
    }

    /// Append a JSON argument fragment for the given index.
    pub fn push_arguments_delta(&mut self, index: usize, delta: &str) {
        let slot = self.slots.entry(index).or_insert_with(PartialToolCall::new);
        slot.arguments_buffer.push_str(delta);
        slot.delta_count += 1;
    }

    /// Mark a tool call as stopped (content_block_stop received).
    pub fn mark_stopped(&mut self, index: usize) {
        if let Some(slot) = self.slots.get_mut(&index) {
            slot.stopped = true;
        }
    }

    /// Check if all known tool calls have been stopped.
    pub fn all_stopped(&self) -> bool {
        !self.slots.is_empty() && self.slots.values().all(|s| s.stopped)
    }

    /// Number of tool calls being accumulated.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Finalize all tool calls, returning completed ones.
    ///
    /// Tool calls with missing ID or name are included with generated defaults.
    /// Invalid JSON arguments are included as-is (the caller can decide how
    /// to handle validation failures).
    pub fn finalize(self) -> Vec<CompletedToolCall> {
        let mut result: Vec<CompletedToolCall> = self
            .slots
            .into_iter()
            .map(|(index, slot)| {
                let arguments_json = slot.arguments_buffer.clone();
                let arguments = serde_json::from_str(&arguments_json).ok();

                CompletedToolCall {
                    id: slot.id.unwrap_or_else(|| format!("tool_{index}")),
                    name: slot.name.unwrap_or_else(|| "unknown".to_string()),
                    arguments_json,
                    arguments,
                    index,
                }
            })
            .collect();

        result.sort_by_key(|tc| tc.index);
        result
    }

    /// Finalize only stopped tool calls, leaving in-flight ones.
    ///
    /// Returns the completed tool calls and retains unfinished ones.
    pub fn finalize_stopped(&mut self) -> Vec<CompletedToolCall> {
        let stopped_indices: Vec<usize> = self
            .slots
            .iter()
            .filter(|(_, slot)| slot.stopped)
            .map(|(idx, _)| *idx)
            .collect();

        let mut result = Vec::new();
        for index in stopped_indices {
            if let Some(slot) = self.slots.remove(&index) {
                let arguments_json = slot.arguments_buffer;
                let arguments = serde_json::from_str(&arguments_json).ok();

                result.push(CompletedToolCall {
                    id: slot.id.unwrap_or_else(|| format!("tool_{index}")),
                    name: slot.name.unwrap_or_else(|| "unknown".to_string()),
                    arguments_json,
                    arguments,
                    index,
                });
            }
        }

        result.sort_by_key(|tc| tc.index);
        result
    }

    /// Get the accumulated arguments so far for a specific index (for debugging).
    pub fn peek_arguments(&self, index: usize) -> Option<&str> {
        self.slots.get(&index).map(|s| s.arguments_buffer.as_str())
    }

    /// Total deltas received across all tool calls.
    pub fn total_deltas(&self) -> usize {
        self.slots.values().map(|s| s.delta_count).sum()
    }
}

impl Default for ToolCallAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

/// Validate accumulated JSON arguments, attempting repair for common issues.
///
/// Returns the repaired JSON string if fixable, or the original if not.
pub fn repair_json_arguments(json: &str) -> String {
    // Try parsing as-is first
    if serde_json::from_str::<serde_json::Value>(json).is_ok() {
        return json.to_string();
    }

    let trimmed = json.trim();

    // Common issue: trailing comma before closing brace
    let repaired = trimmed
        .strip_suffix(",}")
        .map(|s| format!("{s}}}"))
        .unwrap_or_else(|| trimmed.to_string());

    if serde_json::from_str::<serde_json::Value>(&repaired).is_ok() {
        return repaired;
    }

    // Common issue: unclosed string — append closing quote
    let mut attempt = repaired.clone();
    if attempt.matches('"').count() % 2 != 0 {
        attempt.push('"');
    }
    // Try closing unclosed braces/brackets
    let open_braces = attempt.matches('{').count();
    let close_braces = attempt.matches('}').count();
    for _ in 0..(open_braces.saturating_sub(close_braces)) {
        attempt.push('}');
    }
    let open_brackets = attempt.matches('[').count();
    let close_brackets = attempt.matches(']').count();
    for _ in 0..(open_brackets.saturating_sub(close_brackets)) {
        attempt.push(']');
    }

    if serde_json::from_str::<serde_json::Value>(&attempt).is_ok() {
        return attempt;
    }

    // Give up — return original
    json.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_accumulation() {
        let mut acc = ToolCallAccumulator::new();

        acc.push_id(0, "toolu_01abc");
        acc.push_name(0, "get_weather");
        acc.push_arguments_delta(0, "{\"city\":");
        acc.push_arguments_delta(0, "\"London\"}");
        acc.mark_stopped(0);

        assert!(acc.all_stopped());

        let completed = acc.finalize();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].id, "toolu_01abc");
        assert_eq!(completed[0].name, "get_weather");
        assert_eq!(completed[0].arguments_json, "{\"city\":\"London\"}");
        assert!(completed[0].arguments.is_some());
    }

    #[test]
    fn test_interleaved_tool_calls() {
        let mut acc = ToolCallAccumulator::new();

        // Tool 0 starts
        acc.push_id(0, "tool_a");
        acc.push_name(0, "search");

        // Tool 1 starts (interleaved!)
        acc.push_id(1, "tool_b");
        acc.push_name(1, "calendar");

        // Tool 0 gets arguments
        acc.push_arguments_delta(0, "{\"q\":");
        acc.push_arguments_delta(0, "\"test\"}");

        // Tool 1 gets arguments
        acc.push_arguments_delta(1, "{\"date\":\"2024-01-01\"}");

        acc.mark_stopped(0);
        acc.mark_stopped(1);

        let completed = acc.finalize();
        assert_eq!(completed.len(), 2);
        assert_eq!(completed[0].index, 0);
        assert_eq!(completed[0].name, "search");
        assert_eq!(completed[1].index, 1);
        assert_eq!(completed[1].name, "calendar");
    }

    #[test]
    fn test_finalize_stopped_only() {
        let mut acc = ToolCallAccumulator::new();

        acc.push_id(0, "tool_a");
        acc.push_name(0, "search");
        acc.push_arguments_delta(0, "{\"q\":\"test\"}");
        acc.mark_stopped(0);

        acc.push_id(1, "tool_b");
        acc.push_name(1, "calendar");
        // Tool 1 NOT stopped yet

        let completed = acc.finalize_stopped();
        assert_eq!(completed.len(), 1);
        assert_eq!(completed[0].name, "search");

        // Tool 1 still in accumulator
        assert_eq!(acc.len(), 1);
    }

    #[test]
    fn test_repair_json_trailing_comma() {
        let repaired = repair_json_arguments("{\"a\": 1,}");
        assert_eq!(repaired, "{\"a\": 1}");
    }

    #[test]
    fn test_repair_json_unclosed_brace() {
        let repaired = repair_json_arguments("{\"a\": 1");
        let parsed: serde_json::Value = serde_json::from_str(&repaired).unwrap();
        assert_eq!(parsed["a"], 1);
    }

    #[test]
    fn test_peek_arguments() {
        let mut acc = ToolCallAccumulator::new();
        acc.push_arguments_delta(0, "{\"partial\":");
        assert_eq!(acc.peek_arguments(0), Some("{\"partial\":"));
        assert_eq!(acc.peek_arguments(1), None);
    }
}
