//! Session transcript indexer — automatically stores completed conversations
//! as searchable memory chunks.
//!
//! When a conversation session ends (or reaches a configurable turn
//! count), the session indexer flattens the transcript, chunks it, and stores
//! each chunk in the memory system with source=Conversation metadata.
//!
//! ## Design
//!
//! ```text
//! Session ends → flatten_session() → chunk_text() → remember_batch()
//!                                                         │
//!                                                    MemoryManager
//! ```
//!
//! Each chunk includes metadata:
//! - `session_id`: unique session identifier
//! - `source`: "Conversation"
//! - `turn_count`: number of messages in the session
//! - `chunk_index` / `total_chunks`: for reconstructing context
//! - `participants`: list of roles in the conversation
//! - `timestamp`: when the session was indexed

use crate::chunker::{chunk_text, flatten_session, sha256_hex, ChunkerConfig};
use crate::manager::{MemoryConfig, MemoryManager, MemorySource};
use clawdesk_storage::memory_backend::MemoryBackend;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// Configuration for session indexing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionIndexConfig {
    /// Minimum number of turns before a session is worth indexing.
    pub min_turns: usize,
    /// Maximum number of characters in the flattened transcript to index.
    /// Longer sessions are truncated from the beginning (keeping recent turns).
    pub max_chars: usize,
    /// Whether session indexing is enabled.
    pub enabled: bool,
    /// Chunker configuration for splitting sessions.
    pub chunker: ChunkerConfig,
}

impl Default for SessionIndexConfig {
    fn default() -> Self {
        Self {
            min_turns: 4,
            max_chars: 50_000,
            enabled: true,
            chunker: ChunkerConfig {
                max_chars: 2048,
                overlap_chars: 200,
                min_chars: 100,
            },
        }
    }
}

/// A message in a session transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: String,
    pub content: String,
}

/// Index a completed session into the memory system.
///
/// Returns the number of chunks stored, or an error string.
pub async fn index_session<S: MemoryBackend>(
    manager: &MemoryManager<S>,
    session_id: &str,
    messages: &[SessionMessage],
    config: &SessionIndexConfig,
) -> Result<usize, String> {
    if !config.enabled {
        debug!("Session indexing disabled, skipping");
        return Ok(0);
    }

    if messages.len() < config.min_turns {
        debug!(
            session_id,
            turns = messages.len(),
            min = config.min_turns,
            "Session too short to index"
        );
        return Ok(0);
    }

    // Flatten messages into a single document.
    let pairs: Vec<(String, String)> = messages
        .iter()
        .map(|m| (m.role.clone(), m.content.clone()))
        .collect();
    let mut document = flatten_session(&pairs);

    // Truncate from the beginning if too long (keep recent context).
    if document.len() > config.max_chars {
        let start = document.len() - config.max_chars;
        // Find the next newline after the cut point to avoid splitting mid-message.
        let adjusted_start = document[start..]
            .find('\n')
            .map(|pos| start + pos + 1)
            .unwrap_or(start);
        document = document[adjusted_start..].to_string();
    }

    // Chunk the document.
    let chunks = chunk_text(&document, &config.chunker);
    if chunks.is_empty() {
        return Ok(0);
    }

    let now = chrono::Utc::now().to_rfc3339();
    let session_hash = sha256_hex(session_id);

    // Build batch items.
    let items: Vec<(String, MemorySource, serde_json::Value)> = chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| {
            let metadata = serde_json::json!({
                "session_id": session_id,
                "session_hash": &session_hash[..16],
                "turn_count": messages.len(),
                "chunk_index": i,
                "total_chunks": chunks.len(),
                "timestamp": &now,
                "content_hash": sha256_hex(&chunk.text),
            });
            (chunk.text.clone(), MemorySource::Conversation, metadata)
        })
        .collect();

    let chunk_count = items.len();
    manager.remember_batch(items).await?;

    info!(
        session_id,
        chunks = chunk_count,
        turns = messages.len(),
        "Session transcript indexed"
    );

    Ok(chunk_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_and_chunk_integration() {
        let messages = vec![
            SessionMessage { role: "user".into(), content: "Hello, how are you?".into() },
            SessionMessage { role: "assistant".into(), content: "I'm doing well! How can I help?".into() },
            SessionMessage { role: "user".into(), content: "Tell me about Rust.".into() },
            SessionMessage {
                role: "assistant".into(),
                content: "Rust is a systems programming language focused on safety, speed, \
                          and concurrency. It prevents memory errors at compile time through \
                          its ownership system.".into(),
            },
        ];

        let pairs: Vec<(String, String)> = messages
            .iter()
            .map(|m| (m.role.clone(), m.content.clone()))
            .collect();
        let doc = flatten_session(&pairs);

        assert!(doc.contains("user: Hello"));
        assert!(doc.contains("assistant: Rust is"));

        let config = ChunkerConfig {
            max_chars: 2048,
            overlap_chars: 200,
            min_chars: 100,
        };
        let chunks = chunk_text(&doc, &config);
        assert!(!chunks.is_empty());
        // Small conversation should be 1 chunk.
        assert_eq!(chunks.len(), 1);
    }

    #[test]
    fn long_session_truncation() {
        // Build a session longer than max_chars.
        let message = "This is a test message with some content that repeats. ";
        let long_content: String = message.repeat(100);
        let messages = vec![
            SessionMessage { role: "user".into(), content: long_content.clone() },
            SessionMessage { role: "assistant".into(), content: long_content.clone() },
            SessionMessage { role: "user".into(), content: "Short question.".into() },
            SessionMessage { role: "assistant".into(), content: "Short answer.".into() },
        ];

        let pairs: Vec<(String, String)> = messages
            .iter()
            .map(|m| (m.role.clone(), m.content.clone()))
            .collect();
        let doc = flatten_session(&pairs);

        let config = SessionIndexConfig {
            max_chars: 500,
            ..Default::default()
        };

        // Truncate by keeping the end.
        let mut truncated = doc.clone();
        if truncated.len() > config.max_chars {
            let start = truncated.len() - config.max_chars;
            let adjusted = truncated[start..]
                .find('\n')
                .map(|pos| start + pos + 1)
                .unwrap_or(start);
            truncated = truncated[adjusted..].to_string();
        }

        assert!(truncated.len() <= config.max_chars + 200); // allow some newline adjustment
        // Should keep the recent messages.
        assert!(truncated.contains("Short"));
    }
}
