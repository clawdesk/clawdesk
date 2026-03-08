//! # Agent Message — Unified message type with extension via enum variants.
//!
//! Carries both LLM-visible content and agent-level metadata in the same
//! history. The [`convert_to_llm`] function provides a clean anti-corruption
//! layer: it filters to LLM-compatible messages and converts select custom
//! types (e.g., sub-agent results → user messages).
//!
//! ## Design
//!
//! Standard LLM messages and custom agent messages (status updates, sub-agent
//! results, pipeline events, UI notifications) flow through the same message
//! history. `convert_to_llm` is the single boundary between the "full agent
//! context" and the "LLM-visible context."
//!
//! ## Memory Layout
//!
//! The enum's largest variant is `Llm(ChatMessage)` (~200 bytes). Custom
//! variants add at most 16 bytes (fat pointer for boxed CustomPayload).
//! The discriminant tag is 1 byte — <10% overhead.

use clawdesk_providers::{ChatMessage, MessageRole};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ═══════════════════════════════════════════════════════════════════════════
// Agent message enum
// ═══════════════════════════════════════════════════════════════════════════

/// Unified message type for agent conversation history.
///
/// Extends the LLM interchange format with agent-level metadata without
/// polluting the LLM's context. The [`convert_to_llm`] function strips
/// non-LLM messages before sending to providers.
#[derive(Debug, Clone)]
pub enum AgentMessage {
    /// Standard LLM message (user, assistant, system, tool).
    Llm(ChatMessage),

    /// Result from a sub-agent execution.
    SubAgentResult {
        sub_agent_id: String,
        task: String,
        output: String,
        success: bool,
    },

    /// Pipeline event (step completion, routing decision, etc.).
    PipelineEvent {
        pipeline_id: String,
        step_name: String,
        event_type: PipelineEventType,
        detail: String,
    },

    /// Status update from a sub-agent or harness.
    StatusUpdate {
        agent_id: String,
        state: String,
        message: Option<String>,
    },

    /// System notification (safety, budget, degradation).
    SystemNotification {
        severity: NotificationSeverity,
        message: String,
    },

    /// Steering message injected mid-execution (from Rec 2).
    SteeringInjection {
        source: String,
        content: String,
    },

    /// Context from a parent agent handed off at spawn (from Rec 4).
    HandoffContext {
        parent_execution_id: String,
        summary: String,
    },

    /// Custom extension point for downstream consumers.
    Custom {
        message_type: String,
        payload: serde_json::Value,
    },
}

/// Pipeline event types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PipelineEventType {
    StepStarted,
    StepCompleted,
    StepFailed,
    RoutingDecision,
    GateApproved,
    GateDenied,
    MergeCompleted,
}

/// Severity levels for system notifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

// ═══════════════════════════════════════════════════════════════════════════
// Conversion — Anti-Corruption Layer
// ═══════════════════════════════════════════════════════════════════════════

/// Configuration for the `convert_to_llm` conversion.
#[derive(Debug, Clone, Default)]
pub struct ConvertConfig {
    /// Whether to include sub-agent results as user messages.
    pub include_sub_agent_results: bool,
    /// Whether to include handoff context as system-adjacent messages.
    pub include_handoff_context: bool,
    /// Whether to include steering injections as user messages.
    pub include_steering: bool,
    /// Maximum length for injected sub-agent result text.
    pub max_sub_agent_result_len: usize,
}

impl ConvertConfig {
    pub fn default_agent() -> Self {
        Self {
            include_sub_agent_results: true,
            include_handoff_context: true,
            include_steering: true,
            max_sub_agent_result_len: 2000,
        }
    }
}

/// Convert agent messages to LLM-compatible messages.
///
/// This is the anti-corruption layer: it prevents internal agent concerns
/// from leaking into the LLM's context. Non-LLM messages are either
/// filtered out or converted to appropriate LLM message formats.
///
/// Complexity: O(M) for M messages — one scan with per-message conversion.
pub fn convert_to_llm(messages: &[AgentMessage], config: &ConvertConfig) -> Vec<ChatMessage> {
    let mut llm_messages = Vec::with_capacity(messages.len());

    for msg in messages {
        match msg {
            AgentMessage::Llm(chat_msg) => {
                llm_messages.push(chat_msg.clone());
            }

            AgentMessage::SubAgentResult {
                sub_agent_id,
                task,
                output,
                success,
            } if config.include_sub_agent_results => {
                let status = if *success { "completed" } else { "failed" };
                let truncated = if output.len() > config.max_sub_agent_result_len {
                    &output[..config.max_sub_agent_result_len]
                } else {
                    output.as_str()
                };
                let content = format!(
                    "[Sub-agent '{}' {} task '{}']\n{}",
                    sub_agent_id, status, task, truncated
                );
                llm_messages.push(ChatMessage {
                    role: MessageRole::User,
                    content: Arc::from(content),
                    cached_tokens: None,
                });
            }

            AgentMessage::HandoffContext { summary, .. } if config.include_handoff_context => {
                let content = format!(
                    "## Relevant parent context\n\n{}",
                    summary
                );
                llm_messages.push(ChatMessage {
                    role: MessageRole::User,
                    content: Arc::from(content),
                    cached_tokens: None,
                });
            }

            AgentMessage::SteeringInjection { source, content }
                if config.include_steering =>
            {
                let msg_content = format!("[Steering from {}]: {}", source, content);
                llm_messages.push(ChatMessage {
                    role: MessageRole::User,
                    content: Arc::from(msg_content),
                    cached_tokens: None,
                });
            }

            // All other message types are filtered out (not visible to LLM)
            _ => {}
        }
    }

    llm_messages
}

// ═══════════════════════════════════════════════════════════════════════════
// Constructors
// ═══════════════════════════════════════════════════════════════════════════

impl AgentMessage {
    /// Create an LLM user message.
    pub fn user(content: impl Into<Arc<str>>) -> Self {
        Self::Llm(ChatMessage {
            role: MessageRole::User,
            content: content.into(),
            cached_tokens: None,
        })
    }

    /// Create an LLM assistant message.
    pub fn assistant(content: impl Into<Arc<str>>) -> Self {
        Self::Llm(ChatMessage {
            role: MessageRole::Assistant,
            content: content.into(),
            cached_tokens: None,
        })
    }

    /// Create an LLM system message.
    pub fn system(content: impl Into<Arc<str>>) -> Self {
        Self::Llm(ChatMessage {
            role: MessageRole::System,
            content: content.into(),
            cached_tokens: None,
        })
    }

    /// Create a sub-agent result message.
    pub fn sub_agent_result(
        sub_agent_id: impl Into<String>,
        task: impl Into<String>,
        output: impl Into<String>,
        success: bool,
    ) -> Self {
        Self::SubAgentResult {
            sub_agent_id: sub_agent_id.into(),
            task: task.into(),
            output: output.into(),
            success,
        }
    }

    /// Create a system notification.
    pub fn notification(severity: NotificationSeverity, message: impl Into<String>) -> Self {
        Self::SystemNotification {
            severity,
            message: message.into(),
        }
    }

    /// Whether this message is visible to the LLM after conversion.
    pub fn is_llm_visible(&self, config: &ConvertConfig) -> bool {
        match self {
            Self::Llm(_) => true,
            Self::SubAgentResult { .. } => config.include_sub_agent_results,
            Self::HandoffContext { .. } => config.include_handoff_context,
            Self::SteeringInjection { .. } => config.include_steering,
            _ => false,
        }
    }

    /// Get the underlying ChatMessage if this is an LLM message.
    pub fn as_llm(&self) -> Option<&ChatMessage> {
        match self {
            Self::Llm(msg) => Some(msg),
            _ => None,
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// From implementations
// ═══════════════════════════════════════════════════════════════════════════

impl From<ChatMessage> for AgentMessage {
    fn from(msg: ChatMessage) -> Self {
        Self::Llm(msg)
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_filters_non_llm() {
        let messages = vec![
            AgentMessage::user("Hello"),
            AgentMessage::notification(NotificationSeverity::Info, "budget low"),
            AgentMessage::assistant("Hi there"),
            AgentMessage::PipelineEvent {
                pipeline_id: "p1".into(),
                step_name: "step1".into(),
                event_type: PipelineEventType::StepCompleted,
                detail: "done".into(),
            },
        ];

        let config = ConvertConfig::default();
        let llm = convert_to_llm(&messages, &config);
        assert_eq!(llm.len(), 2);
        assert_eq!(llm[0].role, MessageRole::User);
        assert_eq!(llm[1].role, MessageRole::Assistant);
    }

    #[test]
    fn test_convert_includes_sub_agent_results() {
        let messages = vec![
            AgentMessage::user("analyze this"),
            AgentMessage::sub_agent_result("agent-1", "research", "findings here", true),
        ];

        let config = ConvertConfig::default_agent();
        let llm = convert_to_llm(&messages, &config);
        assert_eq!(llm.len(), 2);
        assert!(llm[1].content.contains("Sub-agent"));
        assert!(llm[1].content.contains("completed"));
    }

    #[test]
    fn test_convert_excludes_sub_agent_when_disabled() {
        let messages = vec![
            AgentMessage::sub_agent_result("a1", "task", "output", true),
        ];

        let config = ConvertConfig::default(); // include_sub_agent_results = false
        let llm = convert_to_llm(&messages, &config);
        assert!(llm.is_empty());
    }

    #[test]
    fn test_convert_truncates_long_results() {
        let long_output = "x".repeat(5000);
        let messages = vec![
            AgentMessage::sub_agent_result("a1", "task", long_output, true),
        ];

        let config = ConvertConfig {
            include_sub_agent_results: true,
            max_sub_agent_result_len: 100,
            ..Default::default()
        };
        let llm = convert_to_llm(&messages, &config);
        assert_eq!(llm.len(), 1);
        // The formatted message includes prefix + truncated content
        assert!(llm[0].content.len() < 200);
    }

    #[test]
    fn test_handoff_context_conversion() {
        let messages = vec![
            AgentMessage::HandoffContext {
                parent_execution_id: "exec-1".into(),
                summary: "Parent found that X is relevant".into(),
            },
        ];

        let config = ConvertConfig::default_agent();
        let llm = convert_to_llm(&messages, &config);
        assert_eq!(llm.len(), 1);
        assert!(llm[0].content.contains("Relevant parent context"));
    }

    #[test]
    fn test_from_chat_message() {
        let chat = ChatMessage {
            role: MessageRole::User,
            content: Arc::from("hello"),
            cached_tokens: None,
        };
        let agent: AgentMessage = chat.into();
        assert!(agent.as_llm().is_some());
    }

    #[test]
    fn test_is_llm_visible() {
        let config = ConvertConfig::default_agent();
        assert!(AgentMessage::user("hi").is_llm_visible(&config));
        assert!(!AgentMessage::notification(NotificationSeverity::Info, "x").is_llm_visible(&config));
        assert!(AgentMessage::sub_agent_result("a", "t", "o", true).is_llm_visible(&config));
    }
}
