//! Multi-Provider Text-Based Tool Call Recovery.
//!
//! When an LLM outputs tool calls as plain text (e.g., `<function=NAME>JSON</function>`
//! or inline JSON), this module extracts them into structured `ToolCall` objects.
//!
//! This is provider-agnostic — it handles patterns from multiple providers:
//! - `<function=NAME>{"arg":"val"}</function>` (ChatGPT/GPT format)
//! - `<tool_call>{"name":"...", "arguments":{...}}</tool_call>` (generic XML)
//! - Inline JSON `{"name":"...", "arguments":{...}}` or `[{...}]` (Ollama, small models)
//! - `<|tool_call|>{"name":"...", "arguments":{...}}<|/tool_call|>` (Qwen/DeepSeek)
//!
//! The recovery runs as a fallback when `finish_reason != ToolUse` but the response
//! content contains tool-call-like patterns.

use crate::ToolCall;
use tracing::debug;

/// Attempt to recover tool calls from free-form text content.
///
/// Returns an empty `Vec` if no tool calls are found. This is designed
/// to be called as a fallback after streaming completes with no structured
/// tool calls.
pub fn recover_tool_calls(content: &str) -> Vec<ToolCall> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return vec![];
    }

    // Strategy 1: <function=NAME>JSON</function>
    let mut calls = extract_function_xml(trimmed);
    if !calls.is_empty() {
        debug!(count = calls.len(), "recovered tool calls via <function=NAME> pattern");
        return calls;
    }

    // Strategy 2: <tool_call>JSON</tool_call>
    calls = extract_tool_call_xml(trimmed);
    if !calls.is_empty() {
        debug!(count = calls.len(), "recovered tool calls via <tool_call> pattern");
        return calls;
    }

    // Strategy 3: <|tool_call|>JSON<|/tool_call|> (Qwen/DeepSeek)
    calls = extract_special_token_calls(trimmed);
    if !calls.is_empty() {
        debug!(count = calls.len(), "recovered tool calls via <|tool_call|> pattern");
        return calls;
    }

    // Strategy 4: Inline JSON (array or object with name/arguments fields)
    calls = extract_inline_json(trimmed);
    if !calls.is_empty() {
        debug!(count = calls.len(), "recovered tool calls via inline JSON");
        return calls;
    }

    vec![]
}

/// Extract `<function=NAME>{"arg":"val"}</function>` patterns.
fn extract_function_xml(content: &str) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    let mut remaining = content;
    let mut idx = 0u32;

    while let Some(start) = remaining.find("<function=") {
        let after_tag = &remaining[start + "<function=".len()..];
        // Find the closing '>' of the opening tag
        let Some(gt_pos) = after_tag.find('>') else { break };
        let fn_name = after_tag[..gt_pos].trim().to_string();
        let body_start = &after_tag[gt_pos + 1..];

        // Find </function>
        let Some(end_pos) = body_start.find("</function>") else { break };
        let json_body = body_start[..end_pos].trim();

        if let Ok(arguments) = serde_json::from_str::<serde_json::Value>(json_body) {
            calls.push(ToolCall {
                id: format!("recovery_tc_{}", idx),
                name: fn_name,
                arguments,
            });
            idx += 1;
        }

        remaining = &body_start[end_pos + "</function>".len()..];
    }

    calls
}

/// Extract `<tool_call>{"name":"...", "arguments":{...}}</tool_call>` patterns.
fn extract_tool_call_xml(content: &str) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    let mut remaining = content;
    let mut idx = 0u32;

    while let Some(start) = remaining.find("<tool_call>") {
        let body_start = &remaining[start + "<tool_call>".len()..];
        let Some(end_pos) = body_start.find("</tool_call>") else { break };
        let json_body = body_start[..end_pos].trim();

        if let Some(call) = parse_tool_call_json(json_body, idx) {
            calls.push(call);
            idx += 1;
        }

        remaining = &body_start[end_pos + "</tool_call>".len()..];
    }

    calls
}

/// Extract `<|tool_call|>JSON<|/tool_call|>` patterns (Qwen/DeepSeek format).
fn extract_special_token_calls(content: &str) -> Vec<ToolCall> {
    let mut calls = Vec::new();
    let mut remaining = content;
    let mut idx = 0u32;

    let open_tag = "<|tool_call|>";
    let close_tag = "<|/tool_call|>";

    while let Some(start) = remaining.find(open_tag) {
        let body_start = &remaining[start + open_tag.len()..];
        let Some(end_pos) = body_start.find(close_tag) else { break };
        let json_body = body_start[..end_pos].trim();

        if let Some(call) = parse_tool_call_json(json_body, idx) {
            calls.push(call);
            idx += 1;
        }

        remaining = &body_start[end_pos + close_tag.len()..];
    }

    calls
}

/// Extract inline JSON tool calls from content.
fn extract_inline_json(content: &str) -> Vec<ToolCall> {
    let trimmed = content.trim();

    // Try as JSON array of tool call objects
    if trimmed.starts_with('[') {
        if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(trimmed) {
            let calls: Vec<ToolCall> = arr
                .into_iter()
                .enumerate()
                .filter_map(|(i, v)| parse_value_as_tool_call(&v, i as u32))
                .collect();
            if !calls.is_empty() {
                return calls;
            }
        }
    }

    // Try as single JSON object
    if trimmed.starts_with('{') {
        if let Some(call) = parse_tool_call_json(trimmed, 0) {
            return vec![call];
        }
    }

    // Try to find embedded JSON in the text (brace matching)
    if let Some(start) = trimmed.find('{') {
        let rest = &trimmed[start..];
        if let Some(json_str) = extract_balanced_braces(rest) {
            if let Some(call) = parse_tool_call_json(json_str, 0) {
                return vec![call];
            }
        }
    }

    vec![]
}

/// Parse a JSON string as a tool call.
///
/// Accepts both `{"name":"...", "arguments":{...}}` and
/// `{"function":{"name":"...", "arguments":{...}}}` formats.
fn parse_tool_call_json(json_str: &str, idx: u32) -> Option<ToolCall> {
    let v: serde_json::Value = serde_json::from_str(json_str).ok()?;
    parse_value_as_tool_call(&v, idx)
}

/// Parse a serde_json::Value as a tool call.
fn parse_value_as_tool_call(v: &serde_json::Value, idx: u32) -> Option<ToolCall> {
    // Format 1: {"name":"...", "arguments":{...}}
    if let (Some(name), Some(args)) = (
        v.get("name").and_then(|n| n.as_str()),
        v.get("arguments"),
    ) {
        return Some(ToolCall {
            id: format!("recovery_tc_{}", idx),
            name: name.to_string(),
            arguments: args.clone(),
        });
    }

    // Format 2: {"function":{"name":"...", "arguments":{...}}}
    if let Some(func) = v.get("function") {
        if let (Some(name), Some(args)) = (
            func.get("name").and_then(|n| n.as_str()),
            func.get("arguments"),
        ) {
            return Some(ToolCall {
                id: format!("recovery_tc_{}", idx),
                name: name.to_string(),
                arguments: args.clone(),
            });
        }
    }

    // Format 3: {"tool":"...", "input":{...}} (some model variants)
    if let (Some(name), Some(args)) = (
        v.get("tool").and_then(|n| n.as_str()),
        v.get("input"),
    ) {
        return Some(ToolCall {
            id: format!("recovery_tc_{}", idx),
            name: name.to_string(),
            arguments: args.clone(),
        });
    }

    None
}

/// Extract a balanced brace-delimited JSON string from the start of `input`.
fn extract_balanced_braces(input: &str) -> Option<&str> {
    if !input.starts_with('{') {
        return None;
    }
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;

    for (i, ch) in input.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        match ch {
            '\\' if in_string => escape = true,
            '"' => in_string = !in_string,
            '{' if !in_string => depth += 1,
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(&input[..=i]);
                }
            }
            _ => {}
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_function_xml_recovery() {
        let content = r#"Some text <function=shell_exec>{"command": "ls -la"}</function> more text"#;
        let calls = recover_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell_exec");
        assert_eq!(calls[0].arguments["command"], "ls -la");
    }

    #[test]
    fn test_multiple_function_xml() {
        let content = r#"
<function=read_file>{"path": "a.txt"}</function>
<function=write_file>{"path": "b.txt", "content": "hello"}</function>
"#;
        let calls = recover_tool_calls(content);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[1].name, "write_file");
    }

    #[test]
    fn test_tool_call_xml_recovery() {
        let content = r#"<tool_call>{"name": "search", "arguments": {"query": "rust"}}</tool_call>"#;
        let calls = recover_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "search");
    }

    #[test]
    fn test_special_token_recovery() {
        let content = r#"<|tool_call|>{"name": "calc", "arguments": {"expr": "2+2"}}<|/tool_call|>"#;
        let calls = recover_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "calc");
    }

    #[test]
    fn test_inline_json_object() {
        let content = r#"{"name": "shell_exec", "arguments": {"command": "pwd"}}"#;
        let calls = recover_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell_exec");
    }

    #[test]
    fn test_inline_json_array() {
        let content = r#"[{"name": "a", "arguments": {}}, {"name": "b", "arguments": {"x": 1}}]"#;
        let calls = recover_tool_calls(content);
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn test_function_format_variant() {
        let content = r#"{"function": {"name": "tool_x", "arguments": {"key": "val"}}}"#;
        let calls = recover_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "tool_x");
    }

    #[test]
    fn test_tool_input_format() {
        let content = r#"{"tool": "my_tool", "input": {"data": 42}}"#;
        let calls = recover_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "my_tool");
        assert_eq!(calls[0].arguments["data"], 42);
    }

    #[test]
    fn test_embedded_json_in_text() {
        let content = r#"I'll use the tool: {"name": "search", "arguments": {"query": "test"}} to find results."#;
        let calls = recover_tool_calls(content);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "search");
    }

    #[test]
    fn test_no_tool_calls() {
        let content = "Just a regular text response with no tool calls.";
        let calls = recover_tool_calls(content);
        assert!(calls.is_empty());
    }

    #[test]
    fn test_empty_content() {
        let calls = recover_tool_calls("");
        assert!(calls.is_empty());
    }

    #[test]
    fn test_malformed_json_ignored() {
        let content = r#"<function=test>{not valid json}</function>"#;
        let calls = recover_tool_calls(content);
        assert!(calls.is_empty());
    }

    #[test]
    fn test_recovery_ids_are_sequential() {
        let content = r#"
<function=a>{"x":1}</function>
<function=b>{"y":2}</function>
"#;
        let calls = recover_tool_calls(content);
        assert_eq!(calls[0].id, "recovery_tc_0");
        assert_eq!(calls[1].id, "recovery_tc_1");
    }

    #[test]
    fn test_balanced_braces_with_nested() {
        let input = r#"{"a": {"b": {"c": 1}}, "d": 2} extra"#;
        let result = extract_balanced_braces(input);
        assert_eq!(result, Some(r#"{"a": {"b": {"c": 1}}, "d": 2}"#));
    }

    #[test]
    fn test_balanced_braces_with_strings() {
        let input = r#"{"key": "val with } brace"} rest"#;
        let result = extract_balanced_braces(input);
        assert_eq!(result, Some(r#"{"key": "val with } brace"}"#));
    }
}
