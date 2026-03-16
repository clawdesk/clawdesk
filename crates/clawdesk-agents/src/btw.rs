//! # BTW — Ephemeral Side Questions
//!
//! Allows users to ask a quick question mid-conversation without disrupting
//! the main task. The `/btw <question>` command:
//!
//! 1. Strips tool results from context (for speed).
//! 2. Forces thinking/reasoning OFF.
//! 3. Runs a lightweight LLM call with constrained system prompt.
//! 4. Returns the answer as an ephemeral overlay (not persisted in main history).
//!
//! Inspired by openclaw's `/btw` feature.

use serde::{Deserialize, Serialize};


// ───────────────────────────────────────────────────────────────────────────
// Command parsing
// ───────────────────────────────────────────────────────────────────────────

/// Parse a `/btw` command from user input.
///
/// Returns `Some(question)` if the input is a valid `/btw` command,
/// `None` otherwise.
pub fn parse_btw_command(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if !trimmed.starts_with("/btw") {
        return None;
    }
    let rest = &trimmed[4..];
    if rest.is_empty() {
        return Some(String::new()); // `/btw` with no question — show usage
    }
    if !rest.starts_with(char::is_whitespace) {
        return None; // e.g. `/btwhat` is not a btw command
    }
    let question = rest.trim().to_string();
    Some(question)
}

// ───────────────────────────────────────────────────────────────────────────
// BTW context
// ───────────────────────────────────────────────────────────────────────────

/// System prompt fragment for BTW side questions.
pub const BTW_SYSTEM_PROMPT: &str = "\
You are answering an ephemeral /btw side question. The user is asking a quick \
question about the ongoing conversation. Use the conversation only as background context.

Rules:
- Do NOT continue, resume, or complete any unfinished task.
- Do NOT emit tool calls, shell commands, or file writes unless explicitly asked.
- Answer briefly if the question allows it.
- If the question is ambiguous, ask for clarification in one sentence.
- Do NOT reference the /btw mechanism itself.";

/// Configuration for a BTW side question execution.
#[derive(Debug, Clone)]
pub struct BtwConfig {
    /// The user's question.
    pub question: String,
    /// Maximum tokens for the response.
    pub max_tokens: u32,
    /// Model override (if None, use the session's current model).
    pub model_override: Option<String>,
}

impl Default for BtwConfig {
    fn default() -> Self {
        Self {
            question: String::new(),
            max_tokens: 1024,
            model_override: None,
        }
    }
}

/// Result of a BTW side question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BtwResult {
    /// The original question.
    pub question: String,
    /// The AI's answer.
    pub answer: String,
    /// Whether an error occurred.
    pub is_error: bool,
    /// Model used.
    pub model: Option<String>,
    /// Tokens consumed.
    pub tokens: Option<u32>,
}

/// Event emitted for TUI rendering of BTW messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BtwEvent {
    /// Event kind — always "btw".
    pub kind: String,
    /// Associated run ID, if any.
    pub run_id: Option<String>,
    /// Session key.
    pub session_key: Option<String>,
    /// The question asked.
    pub question: String,
    /// The answer text (may stream incrementally).
    pub text: String,
    /// Whether this is an error response.
    pub is_error: bool,
}

impl BtwEvent {
    /// Create a new BTW answer event.
    pub fn answer(question: &str, text: &str) -> Self {
        Self {
            kind: "btw".to_string(),
            run_id: None,
            session_key: None,
            question: question.to_string(),
            text: text.to_string(),
            is_error: false,
        }
    }

    /// Create a new BTW error event.
    pub fn error(question: &str, error: &str) -> Self {
        Self {
            kind: "btw".to_string(),
            run_id: None,
            session_key: None,
            question: question.to_string(),
            text: error.to_string(),
            is_error: true,
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Context preparation
// ───────────────────────────────────────────────────────────────────────────

/// Strip tool results from chat messages to reduce context size for BTW.
///
/// Tool results are typically large and not relevant to ephemeral side
/// questions. This reduces the context window usage significantly.
pub fn strip_tool_results(
    messages: &[clawdesk_providers::ChatMessage],
) -> Vec<clawdesk_providers::ChatMessage> {
    messages
        .iter()
        .filter(|msg| !matches!(msg.role, clawdesk_providers::MessageRole::Tool))
        .cloned()
        .collect()
}

// ───────────────────────────────────────────────────────────────────────────
// Validation
// ───────────────────────────────────────────────────────────────────────────

/// Validate a BTW request before execution.
pub fn validate_btw_request(question: &str) -> Result<(), BtwError> {
    if question.is_empty() {
        return Err(BtwError::EmptyQuestion);
    }
    if question.len() > 2000 {
        return Err(BtwError::QuestionTooLong {
            length: question.len(),
        });
    }
    Ok(())
}

/// Errors from BTW operations.
#[derive(Debug, thiserror::Error)]
pub enum BtwError {
    #[error("usage: /btw <question>")]
    EmptyQuestion,
    #[error("question too long ({length} chars, max 2000)")]
    QuestionTooLong { length: usize },
    #[error("no active session — start a conversation first")]
    NoActiveSession,
    #[error("LLM error: {0}")]
    LlmError(String),
}

// ───────────────────────────────────────────────────────────────────────────
// Tests
// ───────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_btw_with_question() {
        assert_eq!(
            parse_btw_command("/btw what does this function do?"),
            Some("what does this function do?".to_string())
        );
    }

    #[test]
    fn parse_btw_empty_question() {
        assert_eq!(parse_btw_command("/btw"), Some(String::new()));
    }

    #[test]
    fn parse_btw_with_whitespace() {
        assert_eq!(
            parse_btw_command("/btw   spaces   "),
            Some("spaces".to_string())
        );
    }

    #[test]
    fn parse_non_btw_returns_none() {
        assert_eq!(parse_btw_command("hello world"), None);
        assert_eq!(parse_btw_command("/help"), None);
        assert_eq!(parse_btw_command("/btwhat"), None);
    }

    #[test]
    fn validate_empty_rejected() {
        assert!(validate_btw_request("").is_err());
    }

    #[test]
    fn validate_too_long_rejected() {
        let long = "a".repeat(2001);
        assert!(validate_btw_request(&long).is_err());
    }

    #[test]
    fn validate_normal_ok() {
        assert!(validate_btw_request("what is the meaning of life?").is_ok());
    }

    #[test]
    fn btw_event_answer() {
        let evt = BtwEvent::answer("question?", "answer!");
        assert_eq!(evt.kind, "btw");
        assert!(!evt.is_error);
        assert_eq!(evt.question, "question?");
        assert_eq!(evt.text, "answer!");
    }

    #[test]
    fn btw_event_error() {
        let evt = BtwEvent::error("question?", "something failed");
        assert!(evt.is_error);
    }
}
