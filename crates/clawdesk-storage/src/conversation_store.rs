//! Conversation history storage with vector search.

use async_trait::async_trait;
use clawdesk_types::{
    error::StorageError,
    session::{AgentMessage, SessionKey},
};
use serde::{Deserialize, Serialize};

/// A search hit from vector similarity search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub id: String,
    pub content: String,
    pub score: f32,
    pub metadata: serde_json::Value,
}

/// Parameters for context assembly.
#[derive(Debug, Clone)]
pub struct ContextParams {
    pub session_key: SessionKey,
    pub token_budget: usize,
    pub system_prompt: String,
    pub query_embedding: Option<Vec<f32>>,
    pub history_limit: usize,
}

/// Assembled context payload ready for LLM consumption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextPayload {
    pub text: String,
    pub tokens_used: usize,
    pub tokens_budget: usize,
    pub sections_included: Vec<String>,
}

/// Port: conversation history storage.
#[async_trait]
pub trait ConversationStore: Send + Sync + 'static {
    /// Append a message to a session's conversation history.
    async fn append_message(
        &self,
        key: &SessionKey,
        msg: &AgentMessage,
    ) -> Result<(), StorageError>;

    /// Batch-append multiple messages in a single write pass.
    ///
    /// Default implementation calls `append_message` in a loop. Backends
    /// should override this to use batch/transaction primitives for
    /// amortised I/O (e.g. during agent tool-call bursts where 5-10
    /// messages arrive within milliseconds).
    async fn append_messages(
        &self,
        key: &SessionKey,
        msgs: &[AgentMessage],
    ) -> Result<(), StorageError> {
        for msg in msgs {
            self.append_message(key, msg).await?;
        }
        Ok(())
    }

    /// Load conversation history for a session (most recent first).
    async fn load_history(
        &self,
        key: &SessionKey,
        limit: usize,
    ) -> Result<Vec<AgentMessage>, StorageError>;

    /// Semantic search across conversations using vector similarity.
    async fn search_similar(
        &self,
        query_embedding: &[f32],
        k: usize,
    ) -> Result<Vec<SearchHit>, StorageError>;

    /// Build context for LLM consumption with token budgeting.
    async fn build_context(
        &self,
        params: ContextParams,
    ) -> Result<ContextPayload, StorageError>;
}
