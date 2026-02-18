//! A2A Message types — typed communication between agents.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::task::TaskId;

/// A message exchanged between agents within a task context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct A2AMessage {
    /// Unique message ID.
    pub id: String,
    /// Task this message belongs to.
    pub task_id: TaskId,
    /// Sender agent ID.
    pub from: String,
    /// Recipient agent ID.
    pub to: String,
    /// Message kind (typed payload).
    pub kind: A2AMessageKind,
    /// Timestamp.
    pub timestamp: DateTime<Utc>,
    /// Optional metadata.
    #[serde(default)]
    pub metadata: serde_json::Value,
}

/// Typed message payload variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum A2AMessageKind {
    /// Create a new task.
    TaskSend {
        skill_id: Option<String>,
        input: serde_json::Value,
    },
    /// Status update on a task.
    TaskStatus {
        state: crate::task::TaskState,
        progress: Option<f64>,
        message: Option<String>,
    },
    /// Request additional input for a task.
    InputRequest {
        prompt: String,
        schema: Option<serde_json::Value>,
    },
    /// Provide input for a task.
    InputResponse {
        input: serde_json::Value,
    },
    /// Task artifact delivery.
    ArtifactDelivery {
        artifacts: Vec<Artifact>,
    },
    /// Cancel a task.
    TaskCancel {
        reason: Option<String>,
    },
    /// Streaming text chunk (for real-time output).
    StreamChunk {
        delta: String,
        done: bool,
    },
    /// Error notification.
    Error {
        code: String,
        message: String,
        detail: Option<serde_json::Value>,
    },
    /// Ping/health check.
    Ping { nonce: u64 },
    /// Pong (reply to ping).
    Pong { nonce: u64 },
}

/// An artifact produced during task execution.
///
/// Artifacts are the tangible outputs of a task — generated files, code,
/// data, etc. They're delivered as part of the task result or streamed
/// incrementally during execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    /// Unique artifact ID.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// MIME type (e.g., "text/plain", "application/json", "image/png").
    pub mime_type: String,
    /// Artifact data — inline for small artifacts.
    pub data: ArtifactData,
    /// Size in bytes.
    pub size_bytes: Option<u64>,
    /// Whether this is the final version or a partial update.
    pub is_final: bool,
    /// Index for ordering multiple artifacts.
    pub index: u32,
}

/// How artifact data is delivered.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactData {
    /// Inline text content.
    Text(String),
    /// Base64-encoded binary data.
    Base64(String),
    /// URL to fetch the artifact.
    Url(String),
    /// Artifact is being streamed (follow the task stream).
    Streaming,
}

impl A2AMessage {
    /// Create a task-send message.
    pub fn task_send(
        task_id: TaskId,
        from: impl Into<String>,
        to: impl Into<String>,
        skill_id: Option<String>,
        input: serde_json::Value,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            task_id,
            from: from.into(),
            to: to.into(),
            kind: A2AMessageKind::TaskSend { skill_id, input },
            timestamp: Utc::now(),
            metadata: serde_json::Value::Null,
        }
    }

    /// Create a status update message.
    pub fn status_update(
        task_id: TaskId,
        from: impl Into<String>,
        to: impl Into<String>,
        state: crate::task::TaskState,
    ) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            task_id,
            from: from.into(),
            to: to.into(),
            kind: A2AMessageKind::TaskStatus {
                state,
                progress: None,
                message: None,
            },
            timestamp: Utc::now(),
            metadata: serde_json::Value::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_serialization_roundtrip() {
        let msg = A2AMessage::task_send(
            TaskId::new(),
            "agent-a",
            "agent-b",
            Some("code-review".into()),
            serde_json::json!({"file": "main.rs"}),
        );

        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: A2AMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(msg.id, deserialized.id);
        assert_eq!(msg.from, deserialized.from);
    }

    #[test]
    fn artifact_inline_text() {
        let artifact = Artifact {
            id: "a1".into(),
            name: "output.txt".into(),
            mime_type: "text/plain".into(),
            data: ArtifactData::Text("Hello, world!".into()),
            size_bytes: Some(13),
            is_final: true,
            index: 0,
        };

        let json = serde_json::to_string(&artifact).unwrap();
        assert!(json.contains("text/plain"));
    }
}
