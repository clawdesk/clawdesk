//! Turn capture — bridges `AgentRunner` output to `ChatReplayStore`.
//!
//! After each agent run, the caller constructs a `ChatTurn` from the
//! `AgentResponse` + original user message + timing data, then persists
//! it via `ChatReplayStore::store_turn()`.
//!
//! `TurnBuilder` provides a fluent API for constructing turns:
//!
//! ```rust,ignore
//! let turn = TurnBuilder::new("session-1", 0, "Hello!", &response)
//!     .model("claude-sonnet-4-20250514")
//!     .system_prompt("You are a helpful assistant.")
//!     .build();
//! store.store_turn(&turn).await?;
//! ```

use chrono::{DateTime, Utc};
use clawdesk_storage::replay_store::{ChatTurn, ToolExchange, TurnId};
use serde_json::Value;

/// Builder for constructing `ChatTurn` from agent run results.
pub struct TurnBuilder {
    session_id: String,
    sequence: u64,
    user_input: String,
    assistant_output: String,
    tool_exchanges: Vec<ToolExchange>,
    rounds: u32,
    finish_reason: Option<String>,
    model: Option<String>,
    input_tokens: u64,
    output_tokens: u64,
    started_at: DateTime<Utc>,
    completed_at: DateTime<Utc>,
    system_prompt: Option<String>,
    metadata: Value,
}

impl TurnBuilder {
    /// Start building a turn from the essential fields.
    pub fn new(
        session_id: impl Into<String>,
        sequence: u64,
        user_input: impl Into<String>,
        assistant_output: impl Into<String>,
    ) -> Self {
        let now = Utc::now();
        Self {
            session_id: session_id.into(),
            sequence,
            user_input: user_input.into(),
            assistant_output: assistant_output.into(),
            tool_exchanges: Vec::new(),
            rounds: 1,
            finish_reason: None,
            model: None,
            input_tokens: 0,
            output_tokens: 0,
            started_at: now,
            completed_at: now,
            system_prompt: None,
            metadata: Value::Null,
        }
    }

    pub fn model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn tokens(mut self, input: u64, output: u64) -> Self {
        self.input_tokens = input;
        self.output_tokens = output;
        self
    }

    pub fn rounds(mut self, rounds: u32) -> Self {
        self.rounds = rounds;
        self
    }

    pub fn finish_reason(mut self, reason: impl Into<String>) -> Self {
        self.finish_reason = Some(reason.into());
        self
    }

    pub fn timing(mut self, started_at: DateTime<Utc>, completed_at: DateTime<Utc>) -> Self {
        self.started_at = started_at;
        self.completed_at = completed_at;
        self
    }

    pub fn tool_exchange(mut self, exchange: ToolExchange) -> Self {
        self.tool_exchanges.push(exchange);
        self
    }

    pub fn tool_exchanges(mut self, exchanges: Vec<ToolExchange>) -> Self {
        self.tool_exchanges = exchanges;
        self
    }

    pub fn metadata(mut self, metadata: Value) -> Self {
        self.metadata = metadata;
        self
    }

    /// Build the `ChatTurn`.
    pub fn build(self) -> ChatTurn {
        let duration_ms = self
            .completed_at
            .signed_duration_since(self.started_at)
            .num_milliseconds()
            .unsigned_abs();

        ChatTurn {
            id: TurnId::new(&self.session_id, self.sequence),
            session_id: self.session_id,
            sequence: self.sequence,
            user_input: self.user_input,
            system_prompt: self.system_prompt,
            assistant_output: self.assistant_output,
            tool_exchanges: self.tool_exchanges,
            rounds: self.rounds,
            finish_reason: self.finish_reason,
            model: self.model,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            duration_ms,
            started_at: self.started_at,
            completed_at: self.completed_at,
            metadata: self.metadata,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn basic_turn_construction() {
        let turn = TurnBuilder::new("sess-1", 0, "Hello!", "Hi there!")
            .model("claude-sonnet-4-20250514")
            .tokens(50, 30)
            .rounds(1)
            .finish_reason("end_turn")
            .build();

        assert_eq!(turn.session_id, "sess-1");
        assert_eq!(turn.sequence, 0);
        assert_eq!(turn.user_input, "Hello!");
        assert_eq!(turn.assistant_output, "Hi there!");
        assert_eq!(turn.model, Some("claude-sonnet-4-20250514".into()));
        assert_eq!(turn.input_tokens, 50);
        assert_eq!(turn.output_tokens, 30);
        assert_eq!(turn.total_tokens(), 80);
        assert_eq!(turn.rounds, 1);
        assert!(!turn.has_tool_calls());
        assert_eq!(turn.id.as_str(), "sess-1:turn:0");
    }

    #[test]
    fn turn_with_tool_exchanges() {
        let tool = ToolExchange {
            call_id: "call_001".into(),
            tool_name: "web_search".into(),
            input: serde_json::json!({"query": "rust async"}),
            output: "Rust async/await is...".into(),
            duration_ms: 150,
        };

        let turn = TurnBuilder::new("sess-1", 1, "Search for Rust async", "Here's what I found...")
            .tool_exchange(tool)
            .rounds(2)
            .build();

        assert!(turn.has_tool_calls());
        assert_eq!(turn.tool_exchanges.len(), 1);
        assert_eq!(turn.tool_exchanges[0].tool_name, "web_search");
        assert_eq!(turn.rounds, 2);
    }

    #[test]
    fn turn_timing() {
        let start = Utc::now();
        let end = start + Duration::milliseconds(2500);

        let turn = TurnBuilder::new("sess-1", 0, "input", "output")
            .timing(start, end)
            .build();

        assert_eq!(turn.duration_ms, 2500);
        assert_eq!(turn.started_at, start);
        assert_eq!(turn.completed_at, end);
    }

    #[test]
    fn turn_serialization_roundtrip() {
        let turn = TurnBuilder::new("sess-1", 5, "What is 2+2?", "4")
            .model("haiku")
            .tokens(10, 5)
            .metadata(serde_json::json!({"source": "test"}))
            .build();

        let json = serde_json::to_string(&turn).unwrap();
        let restored: ChatTurn = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.session_id, "sess-1");
        assert_eq!(restored.sequence, 5);
        assert_eq!(restored.user_input, "What is 2+2?");
        assert_eq!(restored.assistant_output, "4");
        assert_eq!(restored.model, Some("haiku".into()));
    }

    #[test]
    fn sequential_turns() {
        let turns: Vec<ChatTurn> = (0..5)
            .map(|i| {
                TurnBuilder::new("sess-1", i, format!("msg {}", i), format!("reply {}", i))
                    .build()
            })
            .collect();

        assert_eq!(turns.len(), 5);
        for (i, turn) in turns.iter().enumerate() {
            assert_eq!(turn.sequence, i as u64);
            assert_eq!(turn.id.as_str(), format!("sess-1:turn:{}", i));
        }
    }
}
