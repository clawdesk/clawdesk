//! Shared utilities for OpenAI-compatible API message formatting.
//!
//! Both the OpenAI and Azure OpenAI providers need to convert the internal
//! `ChatMessage` representation (which uses a flat `{role, content}` format)
//! into the OpenAI API format where:
//! - Assistant messages that triggered tool calls carry a `tool_calls` array
//! - Tool result messages carry a top-level `tool_call_id` field
//!
//! The agent runner stores tool call metadata inside the content JSON of tool
//! messages.  This module reconstructs the proper API format by parsing that
//! metadata.
//!
//! ## Problem
//!
//! The runner pushes tool exchange messages like this during multi-round tool
//! execution:
//!
//! ```text
//! ChatMessage { role: Assistant, content: "I'll look that up…" }
//! ChatMessage { role: Tool,      content: r#"{"tool_call_id":"call_abc","name":"web_search","content":"...","is_error":false}"# }
//! ```
//!
//! The OpenAI/Azure API expects:
//!
//! ```json
//! {"role":"assistant","content":"I'll look that up…","tool_calls":[{"id":"call_abc","type":"function","function":{"name":"web_search","arguments":"{}"}}]}
//! {"role":"tool","tool_call_id":"call_abc","content":"..."}
//! ```
//!
//! Without this conversion, Azure rejects with HTTP 400:
//! *"messages with role 'tool' must be a response to a preceding message with 'tool_calls'"*

use crate::{ChatMessage, MessageRole};

/// Metadata parsed from a tool result message's content JSON.
struct ToolResultMeta {
    tool_call_id: String,
    name: String,
    content: String,
}

/// Try to parse tool result metadata from a ChatMessage with role=Tool.
///
/// The runner stores tool results as:
/// ```json
/// {"tool_call_id": "call_xxx", "name": "tool_name", "content": "...", "is_error": false}
/// ```
fn parse_tool_result(msg: &ChatMessage) -> Option<ToolResultMeta> {
    let parsed: serde_json::Value = serde_json::from_str(&msg.content).ok()?;
    Some(ToolResultMeta {
        tool_call_id: parsed.get("tool_call_id")?.as_str()?.to_string(),
        name: parsed
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string(),
        content: parsed
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
    })
}

/// Convert internal `ChatMessage` list to OpenAI API message format.
///
/// Handles:
/// - **Assistant → Tool** sequences: reconstructs the `tool_calls` array on the
///   assistant message so the API accepts the subsequent tool results.
/// - **Tool messages**: extracts `tool_call_id` and content from the embedded
///   JSON, placing `tool_call_id` at the top level as the API requires.
/// - **System/User messages**: passed through unchanged.
///
/// The `system_prompt` is prepended as the first message when present.
pub fn build_openai_api_messages(
    system_prompt: Option<&str>,
    messages: &[ChatMessage],
) -> Vec<serde_json::Value> {
    let mut api_msgs = Vec::with_capacity(messages.len() + 1);

    if let Some(system) = system_prompt {
        api_msgs.push(serde_json::json!({
            "role": "system",
            "content": system,
        }));
    }

    let len = messages.len();
    for i in 0..len {
        let msg = &messages[i];
        match msg.role {
            MessageRole::Assistant => {
                // Look ahead: if the next message(s) are Tool results, this
                // assistant message must have originally had tool_calls.
                // Reconstruct the tool_calls array from the following tool
                // result metadata.
                let mut tool_calls_json = Vec::new();
                let mut j = i + 1;
                while j < len && messages[j].role == MessageRole::Tool {
                    if let Some(meta) = parse_tool_result(&messages[j]) {
                        tool_calls_json.push(serde_json::json!({
                            "id": meta.tool_call_id,
                            "type": "function",
                            "function": {
                                "name": meta.name,
                                // Original arguments aren't stored in the tool result;
                                // use empty object as a placeholder.  The API validates
                                // structural pairing (matching IDs), not argument replay.
                                "arguments": "{}"
                            }
                        }));
                    }
                    j += 1;
                }

                if tool_calls_json.is_empty() {
                    // Normal assistant message — no tool calls followed.
                    api_msgs.push(serde_json::json!({
                        "role": "assistant",
                        "content": &*msg.content,
                    }));
                } else {
                    // Assistant message that triggered tool calls.
                    let mut asst = serde_json::json!({
                        "role": "assistant",
                        "tool_calls": tool_calls_json,
                    });
                    // Include content if non-empty (some models emit text + tool_calls).
                    if !msg.content.trim().is_empty() {
                        asst["content"] = serde_json::json!(&*msg.content);
                    }
                    api_msgs.push(asst);
                }
            }
            MessageRole::Tool => {
                // Extract top-level tool_call_id and actual content text
                // from the runner's embedded JSON format.
                if let Some(meta) = parse_tool_result(msg) {
                    api_msgs.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": meta.tool_call_id,
                        "content": meta.content,
                    }));
                } else {
                    // Fallback: content isn't parseable runner JSON — send as-is.
                    // This will likely fail at the API but preserves debug visibility.
                    tracing::warn!(
                        content_preview = %msg.content.chars().take(100).collect::<String>(),
                        "Tool message content is not parseable runner JSON — sending raw"
                    );
                    api_msgs.push(serde_json::json!({
                        "role": "tool",
                        "tool_call_id": "unknown",
                        "content": &*msg.content,
                    }));
                }
            }
            _ => {
                // System, User — pass through.
                api_msgs.push(serde_json::json!({
                    "role": msg.role.as_str(),
                    "content": &*msg.content,
                }));
            }
        }
    }

    api_msgs
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn make_msg(role: MessageRole, content: &str) -> ChatMessage {
        ChatMessage {
            role,
            content: Arc::from(content),
            cached_tokens: None,
        }
    }

    #[test]
    fn plain_user_assistant_roundtrip() {
        let msgs = vec![
            make_msg(MessageRole::User, "hello"),
            make_msg(MessageRole::Assistant, "hi there"),
        ];
        let api = build_openai_api_messages(Some("system prompt"), &msgs);
        assert_eq!(api.len(), 3); // system + user + assistant
        assert_eq!(api[0]["role"], "system");
        assert_eq!(api[1]["role"], "user");
        assert_eq!(api[2]["role"], "assistant");
        assert_eq!(api[2]["content"], "hi there");
        assert!(api[2].get("tool_calls").is_none());
    }

    #[test]
    fn tool_round_reconstructs_tool_calls() {
        let msgs = vec![
            make_msg(MessageRole::User, "search for cats"),
            make_msg(MessageRole::Assistant, "Let me search for that."),
            make_msg(
                MessageRole::Tool,
                r#"{"tool_call_id":"call_001","name":"web_search","content":"Cats are great pets.","is_error":false}"#,
            ),
            make_msg(MessageRole::Assistant, "Cats are great pets!"),
        ];
        let api = build_openai_api_messages(None, &msgs);
        assert_eq!(api.len(), 4);

        // The assistant message before the tool result should have tool_calls
        let asst = &api[1];
        assert_eq!(asst["role"], "assistant");
        let tc = asst["tool_calls"].as_array().expect("should have tool_calls");
        assert_eq!(tc.len(), 1);
        assert_eq!(tc[0]["id"], "call_001");
        assert_eq!(tc[0]["function"]["name"], "web_search");

        // Tool message should have tool_call_id at top level
        let tool = &api[2];
        assert_eq!(tool["role"], "tool");
        assert_eq!(tool["tool_call_id"], "call_001");
        assert_eq!(tool["content"], "Cats are great pets.");

        // Final assistant message — no tool_calls
        assert_eq!(api[3]["role"], "assistant");
        assert!(api[3].get("tool_calls").is_none());
    }

    #[test]
    fn multiple_tool_calls_in_one_round() {
        let msgs = vec![
            make_msg(MessageRole::User, "get weather and news"),
            make_msg(MessageRole::Assistant, ""),
            make_msg(
                MessageRole::Tool,
                r#"{"tool_call_id":"call_A","name":"get_weather","content":"Sunny 72F","is_error":false}"#,
            ),
            make_msg(
                MessageRole::Tool,
                r#"{"tool_call_id":"call_B","name":"get_news","content":"Headlines today","is_error":false}"#,
            ),
            make_msg(MessageRole::Assistant, "It's sunny and here are the headlines."),
        ];
        let api = build_openai_api_messages(None, &msgs);
        assert_eq!(api.len(), 5);

        let asst = &api[1];
        let tc = asst["tool_calls"].as_array().unwrap();
        assert_eq!(tc.len(), 2);
        assert_eq!(tc[0]["id"], "call_A");
        assert_eq!(tc[1]["id"], "call_B");

        // Empty assistant content should NOT have content field
        assert!(asst.get("content").is_none());
    }
}
