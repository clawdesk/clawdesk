//! Session transcript repair — enforce structural invariants for provider APIs.
//!
//! Five repair passes (single-pass finite-state transducer design):
//!
//! 1. **Orphaned tool_result removal**: O(n) forward scan with O(k) HashSet.
//! 2. **tool_result.details stripping**: O(n) map with in-place mutation.
//! 3. **Turn alternation repair**: Merge consecutive same-role messages for
//!    providers requiring strict user→assistant alternation (e.g., Gemini).
//! 4. **Orphaned tool_use repair**: Append synthetic tool_result for unpaired
//!    tool_use blocks at the end of the transcript.
//! 5. **Duplicate tool_result removal**: Remove duplicate tool results with
//!    the same tool_call_id in a single assistant-tool sequence.
//! 6. **Oversized tool result truncation**: Truncate tool results exceeding
//!    a per-message token budget before they enter the context window.
//!
//! Designed to run after any message mutation (compaction, pruning, history injection)
//! and on session reload from storage.

use clawdesk_providers::{ChatMessage, MessageRole};
use std::collections::HashSet;
use tracing::debug;

/// Configuration for transcript repair.
#[derive(Debug, Clone)]
pub struct RepairConfig {
    /// Remove orphaned tool results.
    pub repair_orphans: bool,
    /// Strip tool result details.
    pub strip_details: bool,
    /// Enforce turn alternation (merge consecutive same-role messages).
    pub enforce_alternation: bool,
    /// Repair orphaned tool_use blocks (append synthetic results).
    pub repair_orphaned_tool_use: bool,
    /// Remove duplicate tool results.
    pub remove_duplicate_results: bool,
    /// Truncate oversized tool results.
    pub truncate_oversized: bool,
    /// Maximum token count per tool result (for truncation).
    pub max_result_tokens: usize,
    /// Provider name (controls which repairs are applied).
    pub provider: String,
}

impl Default for RepairConfig {
    fn default() -> Self {
        Self {
            repair_orphans: true,
            strip_details: true,
            enforce_alternation: false,
            repair_orphaned_tool_use: true,
            remove_duplicate_results: true,
            truncate_oversized: true,
            max_result_tokens: 8192,
            provider: String::new(),
        }
    }
}

/// Result of transcript repair.
#[derive(Debug, Default)]
pub struct RepairResult {
    /// Number of orphaned tool results removed.
    pub orphans_removed: usize,
    /// Number of messages with details stripped.
    pub details_stripped: usize,
    /// Number of consecutive messages merged for alternation.
    pub messages_merged: usize,
    /// Number of synthetic tool results added for orphaned tool_use blocks.
    pub synthetic_results_added: usize,
    /// Number of duplicate tool results removed.
    pub duplicates_removed: usize,
    /// Number of tool results truncated for size.
    pub results_truncated: usize,
}

/// Run all configured repair passes on the message history.
///
/// Returns `RepairResult` with counts of modifications made.
pub fn repair_transcript(
    messages: &mut Vec<ChatMessage>,
    config: &RepairConfig,
) -> RepairResult {
    let mut result = RepairResult::default();

    if config.repair_orphans {
        result.orphans_removed = remove_orphaned_tool_results(messages);
    }

    if config.repair_orphaned_tool_use {
        result.synthetic_results_added = repair_orphaned_tool_use(messages);
    }

    if config.remove_duplicate_results {
        result.duplicates_removed = remove_duplicate_tool_results(messages);
    }

    if config.strip_details {
        result.details_stripped = strip_tool_result_details(messages);
    }

    if config.truncate_oversized {
        result.results_truncated = truncate_oversized_results(messages, config.max_result_tokens);
    }

    if config.enforce_alternation {
        result.messages_merged = enforce_turn_alternation(messages);
    }

    result
}

/// Remove orphaned `tool_result` messages whose `tool_use` is no longer present.
///
/// Algorithm: O(n) forward scan, O(k) space for `HashSet<tool_use_id>`.
///
/// Uses `Vec::retain` for in-place removal without reallocation.
pub fn remove_orphaned_tool_results(messages: &mut Vec<ChatMessage>) -> usize {
    // Forward scan: collect all tool_use IDs from assistant messages
    let mut tool_use_ids: HashSet<String> = HashSet::new();

    for msg in messages.iter() {
        // Collect tool_use IDs from various message formats
        collect_tool_use_ids(&msg.content, msg.role, &mut tool_use_ids);
    }

    // Remove tool results whose tool_use_id is not in the set
    let initial_len = messages.len();
    messages.retain(|msg| {
        if msg.role != MessageRole::Tool {
            return true;
        }

        // Extract tool_call_id from tool result
        if let Some(id) = extract_tool_call_id(&msg.content) {
            if !tool_use_ids.contains(&id) {
                debug!(tool_call_id = %id, "removing orphaned tool_result");
                return false;
            }
        }
        true
    });

    initial_len - messages.len()
}

/// Collect tool_use IDs from a message's content.
fn collect_tool_use_ids(content: &str, role: MessageRole, ids: &mut HashSet<String>) {
    if role != MessageRole::Assistant {
        return;
    }

    // Try parsing as JSON
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(content) {
        collect_ids_recursive(&val, ids);
        return;
    }

    // Try line-by-line for multi-JSON content
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
                collect_ids_recursive(&val, ids);
            }
        }
    }
}

fn collect_ids_recursive(val: &serde_json::Value, ids: &mut HashSet<String>) {
    match val {
        serde_json::Value::Object(map) => {
            // tool_call_id field
            if let Some(id) = map.get("tool_call_id").and_then(|v| v.as_str()) {
                ids.insert(id.to_string());
            }
            // id field when type == "tool_use"
            if let Some(id) = map.get("id").and_then(|v| v.as_str()) {
                let is_tool_use = map
                    .get("type")
                    .and_then(|v| v.as_str())
                    .map_or(false, |t| t == "tool_use");
                if is_tool_use {
                    ids.insert(id.to_string());
                }
            }
            // Recurse
            for v in map.values() {
                collect_ids_recursive(v, ids);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_ids_recursive(v, ids);
            }
        }
        _ => {}
    }
}

/// Extract tool_call_id from a tool result message's content.
fn extract_tool_call_id(content: &str) -> Option<String> {
    let val: serde_json::Value = serde_json::from_str(content).ok()?;
    val.get("tool_call_id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Strip verbose metadata fields from tool_result messages.
///
/// Removes: debug, stack_trace, raw_response, headers, request_body,
/// timing, metadata, trace, http_status, request_url.
///
/// O(n) over messages, in-place mutation.
pub fn strip_tool_result_details(messages: &mut [ChatMessage]) -> usize {
    let mut stripped = 0;

    let fields_to_strip = [
        "debug",
        "stack_trace",
        "raw_response",
        "headers",
        "request_body",
        "timing",
        "metadata",
        "trace",
        "http_status",
        "request_url",
        "elapsed_ms",
        "stderr",
    ];

    for msg in messages.iter_mut() {
        if msg.role != MessageRole::Tool {
            continue;
        }

        if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&msg.content) {
            let mut changed = false;
            if let Some(obj) = val.as_object_mut() {
                for field in &fields_to_strip {
                    if obj.remove(*field).is_some() {
                        changed = true;
                    }
                }
            }
            if changed {
                if let Ok(new_content) = serde_json::to_string(&val) {
                    msg.content = std::sync::Arc::from(new_content);
                    msg.invalidate_token_cache();
                    stripped += 1;
                }
            }
        }
    }

    stripped
}

/// Enforce turn alternation by merging consecutive same-role messages.
///
/// For providers requiring strict user→assistant alternation (e.g., Gemini),
/// consecutive same-role messages are merged with newline separator.
///
/// O(n) with a single pass.
pub fn enforce_turn_alternation(messages: &mut Vec<ChatMessage>) -> usize {
    if messages.len() <= 1 {
        return 0;
    }

    let mut merged_count = 0;
    let mut result: Vec<ChatMessage> = Vec::with_capacity(messages.len());

    for msg in messages.drain(..) {
        if let Some(last) = result.last_mut() {
            if last.role == msg.role && msg.role != MessageRole::System {
                // Merge: append content with newline
                let mut content = String::from(&*last.content);
                content.push('\n');
                content.push_str(&msg.content);
                last.content = std::sync::Arc::from(content);
                last.invalidate_token_cache();
                merged_count += 1;
                continue;
            }
        }
        result.push(msg);
    }

    *messages = result;
    merged_count
}

/// Convenience: repair for Anthropic API requirements.
pub fn repair_for_anthropic(messages: &mut Vec<ChatMessage>) -> RepairResult {
    repair_transcript(
        messages,
        &RepairConfig {
            repair_orphans: true,
            strip_details: true,
            enforce_alternation: false,
            repair_orphaned_tool_use: true,
            remove_duplicate_results: true,
            truncate_oversized: true,
            max_result_tokens: 8192,
            provider: "anthropic".to_string(),
        },
    )
}

/// Convenience: repair for Gemini API requirements.
pub fn repair_for_gemini(messages: &mut Vec<ChatMessage>) -> RepairResult {
    repair_transcript(
        messages,
        &RepairConfig {
            repair_orphans: true,
            strip_details: true,
            enforce_alternation: true,
            repair_orphaned_tool_use: true,
            remove_duplicate_results: true,
            truncate_oversized: true,
            max_result_tokens: 8192,
            provider: "gemini".to_string(),
        },
    )
}

/// Repair orphaned tool_use blocks at the end of the transcript.
///
/// If the last assistant message contains tool_use blocks but no corresponding
/// tool results follow, append synthetic error results. This prevents provider
/// API rejection (Anthropic 400 error on unpaired tool_use).
///
/// Finite-state transducer: O(n) time, O(t) space where t = pending tool calls.
pub fn repair_orphaned_tool_use(messages: &mut Vec<ChatMessage>) -> usize {
    // Collect tool_use IDs from the last assistant message
    let mut pending_tool_ids: Vec<String> = Vec::new();

    // Scan backwards to find the last assistant message
    for msg in messages.iter().rev() {
        if msg.role == MessageRole::Assistant {
            collect_tool_use_ids_into(&msg.content, &mut pending_tool_ids);
            break;
        }
        if msg.role == MessageRole::Tool {
            // If there are already tool results after the last assistant,
            // remove matching IDs from pending
            if let Some(id) = extract_tool_call_id(&msg.content) {
                pending_tool_ids.retain(|pid| pid != &id);
            }
            // Don't break — there may be more tool results
        }
    }

    // After the backward scan, re-verify: check which tool_use IDs
    // from the last assistant message actually have matching tool results
    if !pending_tool_ids.is_empty() {
        let last_assistant_idx = messages
            .iter()
            .rposition(|m| m.role == MessageRole::Assistant);

        if let Some(idx) = last_assistant_idx {
            // Collect all tool result IDs after this assistant message
            let existing_result_ids: HashSet<String> = messages[idx + 1..]
                .iter()
                .filter(|m| m.role == MessageRole::Tool)
                .filter_map(|m| extract_tool_call_id(&m.content))
                .collect();

            // Only keep IDs that don't have results
            pending_tool_ids.retain(|id| !existing_result_ids.contains(id));
        }
    }

    let count = pending_tool_ids.len();
    if count > 0 {
        debug!(count, "Appending synthetic tool results for orphaned tool_use blocks");
    }

    // Append synthetic tool results for each orphaned tool_use
    for tool_id in &pending_tool_ids {
        let synthetic_result = serde_json::json!({
            "tool_call_id": tool_id,
            "content": "[Tool execution interrupted — session was terminated or crashed before completion]",
            "is_error": true,
            "synthetic": true,
        });

        messages.push(ChatMessage::new(
            MessageRole::Tool,
            serde_json::to_string(&synthetic_result).unwrap_or_default(),
        ));
    }

    count
}

/// Helper to collect tool_use IDs into a vec (preserving order).
fn collect_tool_use_ids_into(content: &str, ids: &mut Vec<String>) {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(content) {
        collect_ids_ordered(&val, ids);
        return;
    }
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('{') || trimmed.starts_with('[') {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
                collect_ids_ordered(&val, ids);
            }
        }
    }
}

fn collect_ids_ordered(val: &serde_json::Value, ids: &mut Vec<String>) {
    match val {
        serde_json::Value::Object(map) => {
            if let Some(id) = map.get("id").and_then(|v| v.as_str()) {
                let is_tool_use = map
                    .get("type")
                    .and_then(|v| v.as_str())
                    .map_or(false, |t| t == "tool_use");
                if is_tool_use && !ids.contains(&id.to_string()) {
                    ids.push(id.to_string());
                }
            }
            for v in map.values() {
                collect_ids_ordered(v, ids);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_ids_ordered(v, ids);
            }
        }
        _ => {}
    }
}

/// Remove duplicate tool results with the same tool_call_id.
///
/// Maintains a HashSet of seen tool_call_ids within each assistant→tool
/// sequence. Second occurrence of the same ID is removed.
///
/// O(n) time, O(t) space where t = max tool calls per round.
pub fn remove_duplicate_tool_results(messages: &mut Vec<ChatMessage>) -> usize {
    let mut seen_ids: HashSet<String> = HashSet::new();
    let initial_len = messages.len();

    messages.retain(|msg| {
        if msg.role == MessageRole::Assistant {
            // New assistant message — reset the seen set
            seen_ids.clear();
            return true;
        }
        if msg.role != MessageRole::Tool {
            return true;
        }
        if let Some(id) = extract_tool_call_id(&msg.content) {
            if seen_ids.contains(&id) {
                debug!(tool_call_id = %id, "Removing duplicate tool_result");
                return false;
            }
            seen_ids.insert(id);
        }
        true
    });

    initial_len - messages.len()
}

/// Truncate oversized tool results to fit within a per-message token budget.
///
/// For each tool result message, if its token count exceeds `max_tokens`,
/// truncate the content at the nearest paragraph/line boundary before the limit.
///
/// This ensures the invariant:
/// ∀ msg ∈ stored_transcript: token_count(msg) ≤ max_result_tokens
///
/// O(n) over messages.
pub fn truncate_oversized_results(messages: &mut [ChatMessage], max_tokens: usize) -> usize {
    let mut truncated = 0;

    for msg in messages.iter_mut() {
        if msg.role != MessageRole::Tool {
            continue;
        }

        let tokens = msg.token_count();
        if tokens <= max_tokens {
            continue;
        }

        // Estimate character position for max_tokens
        // Using ~4 chars per token as a rough estimate
        let max_chars = max_tokens * 4;

        if msg.content.len() <= max_chars {
            continue;
        }

        // Try to truncate at a sensible boundary
        if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&msg.content) {
            if let Some(obj) = val.as_object_mut() {
                if let Some(content) = obj.get("content").and_then(|v| v.as_str()) {
                    if content.len() > max_chars {
                        let truncated_content = truncate_at_boundary(content, max_chars);
                        obj.insert("content".to_string(), serde_json::Value::String(truncated_content));
                        obj.insert("truncated".to_string(), serde_json::Value::Bool(true));
                        obj.insert("original_tokens".to_string(), serde_json::Value::Number(tokens.into()));

                        if let Ok(new_content) = serde_json::to_string(&val) {
                            msg.content = std::sync::Arc::from(new_content);
                            msg.invalidate_token_cache();
                            truncated += 1;
                        }
                    }
                }
            }
        } else {
            // Non-JSON content — truncate raw string
            let truncated_str = truncate_at_boundary(&msg.content, max_chars);
            msg.content = std::sync::Arc::from(truncated_str);
            msg.invalidate_token_cache();
            truncated += 1;
        }
    }

    if truncated > 0 {
        debug!(count = truncated, max_tokens, "Truncated oversized tool results");
    }

    truncated
}

/// Truncate at paragraph or line boundary (semantic truncation).
fn truncate_at_boundary(content: &str, max_chars: usize) -> String {
    if content.len() <= max_chars {
        return content.to_string();
    }

    let search_region = &content[..max_chars.min(content.len())];

    // Try paragraph boundary
    if let Some(pos) = search_region.rfind("\n\n") {
        if pos > max_chars / 2 {
            let mut result = content[..pos].to_string();
            result.push_str("\n\n[… output truncated for context budget]");
            return result;
        }
    }

    // Try line boundary
    if let Some(pos) = search_region.rfind('\n') {
        if pos > max_chars / 2 {
            let mut result = content[..pos].to_string();
            result.push_str("\n[… output truncated for context budget]");
            return result;
        }
    }

    // Hard truncate
    let mut result = content[..max_chars].to_string();
    result.push_str(" [… truncated]");
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(role: MessageRole, content: &str) -> ChatMessage {
        ChatMessage::new(role, content)
    }

    #[test]
    fn test_orphan_removal() {
        let mut messages = vec![
            msg(MessageRole::User, "do something"),
            msg(
                MessageRole::Tool,
                r#"{"tool_call_id": "orphan-1", "content": "result"}"#,
            ),
            msg(MessageRole::Assistant, "done"),
        ];

        let removed = remove_orphaned_tool_results(&mut messages);
        assert_eq!(removed, 1);
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn test_matched_pair_kept() {
        let mut messages = vec![
            msg(
                MessageRole::Assistant,
                r#"{"tool_call_id": "call-1", "type": "tool_use"}"#,
            ),
            msg(
                MessageRole::Tool,
                r#"{"tool_call_id": "call-1", "content": "result"}"#,
            ),
        ];

        let removed = remove_orphaned_tool_results(&mut messages);
        assert_eq!(removed, 0);
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn test_detail_stripping() {
        let mut messages = vec![msg(
            MessageRole::Tool,
            r#"{"tool_call_id": "1", "content": "ok", "debug": {}, "headers": {"x": "y"}}"#,
        )];

        let stripped = strip_tool_result_details(&mut messages);
        assert_eq!(stripped, 1);

        let val: serde_json::Value = serde_json::from_str(&messages[0].content).unwrap();
        assert!(val.get("debug").is_none());
        assert!(val.get("headers").is_none());
        assert!(val.get("content").is_some());
    }

    #[test]
    fn test_turn_alternation() {
        let mut messages = vec![
            msg(MessageRole::User, "hello"),
            msg(MessageRole::User, "how are you"),
            msg(MessageRole::Assistant, "I'm fine"),
            msg(MessageRole::Assistant, "thanks for asking"),
        ];

        let merged = enforce_turn_alternation(&mut messages);
        assert_eq!(merged, 2);
        assert_eq!(messages.len(), 2);
        assert!(messages[0].content.contains("hello"));
        assert!(messages[0].content.contains("how are you"));
    }

    #[test]
    fn test_full_repair_pipeline() {
        let mut messages = vec![
            msg(MessageRole::User, "start"),
            msg(
                MessageRole::Tool,
                r#"{"tool_call_id": "orphan", "content": "x", "debug": {}}"#,
            ),
            msg(MessageRole::Assistant, "part 1"),
            msg(MessageRole::Assistant, "part 2"),
        ];

        let result = repair_transcript(
            &mut messages,
            &RepairConfig {
                repair_orphans: true,
                strip_details: true,
                enforce_alternation: true,
                provider: "gemini".to_string(),
                ..Default::default()
            },
        );

        assert_eq!(result.orphans_removed, 1);
        assert_eq!(result.messages_merged, 1);
    }

    #[test]
    fn test_orphaned_tool_use_repair() {
        let mut messages = vec![
            msg(MessageRole::User, "do something"),
            msg(
                MessageRole::Assistant,
                r#"[{"type": "tool_use", "id": "call-1", "name": "read_file", "input": {}}]"#,
            ),
            // No tool result — runner crashed
        ];

        let added = repair_orphaned_tool_use(&mut messages);
        assert_eq!(added, 1);
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[2].role, MessageRole::Tool);
        assert!(messages[2].content.contains("call-1"));
        assert!(messages[2].content.contains("interrupted"));
    }

    #[test]
    fn test_orphaned_tool_use_with_existing_result() {
        let mut messages = vec![
            msg(MessageRole::User, "do something"),
            msg(
                MessageRole::Assistant,
                r#"[{"type": "tool_use", "id": "call-1", "name": "read_file", "input": {}}]"#,
            ),
            msg(
                MessageRole::Tool,
                r#"{"tool_call_id": "call-1", "content": "file contents"}"#,
            ),
        ];

        let added = repair_orphaned_tool_use(&mut messages);
        assert_eq!(added, 0); // Result already exists
    }

    #[test]
    fn test_duplicate_tool_result_removal() {
        let mut messages = vec![
            msg(
                MessageRole::Assistant,
                r#"{"type": "tool_use", "id": "call-1"}"#,
            ),
            msg(
                MessageRole::Tool,
                r#"{"tool_call_id": "call-1", "content": "result 1"}"#,
            ),
            msg(
                MessageRole::Tool,
                r#"{"tool_call_id": "call-1", "content": "result 1 duplicate"}"#,
            ),
        ];

        let removed = remove_duplicate_tool_results(&mut messages);
        assert_eq!(removed, 1);
        assert_eq!(messages.len(), 2);
    }

    #[test]
    fn test_oversized_result_truncation() {
        let large_content = "x".repeat(100_000);
        let json_content = serde_json::json!({
            "tool_call_id": "call-1",
            "content": large_content,
        });

        let mut messages = vec![msg(
            MessageRole::Tool,
            &serde_json::to_string(&json_content).unwrap(),
        )];

        let truncated = truncate_oversized_results(&mut messages, 1000);
        assert_eq!(truncated, 1);
        assert!(messages[0].content.contains("truncated"));
    }

    #[test]
    fn test_comprehensive_repair() {
        let mut messages = vec![
            msg(MessageRole::User, "start"),
            // Orphaned tool result
            msg(
                MessageRole::Tool,
                r#"{"tool_call_id": "ghost", "content": "no parent"}"#,
            ),
            msg(
                MessageRole::Assistant,
                r#"[{"type": "tool_use", "id": "call-2", "name": "write_file", "input": {}}]"#,
            ),
            // Duplicate tool results
            msg(
                MessageRole::Tool,
                r#"{"tool_call_id": "call-2", "content": "ok"}"#,
            ),
            msg(
                MessageRole::Tool,
                r#"{"tool_call_id": "call-2", "content": "ok duplicate"}"#,
            ),
            // Consecutive assistant messages
            msg(MessageRole::Assistant, "thinking..."),
            msg(MessageRole::Assistant, "done!"),
        ];

        let result = repair_transcript(
            &mut messages,
            &RepairConfig {
                enforce_alternation: true,
                ..Default::default()
            },
        );

        assert!(result.orphans_removed >= 1);
        assert!(result.duplicates_removed >= 1);
        assert!(result.messages_merged >= 1);
    }
}
