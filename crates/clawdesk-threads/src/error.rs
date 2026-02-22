//! Error types for the thread store.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ThreadStoreError {
    #[error("failed to open database: {detail}")]
    OpenFailed { detail: String },

    #[error("serialization failed: {detail}")]
    Serialization { detail: String },

    #[error("thread not found: {id}")]
    ThreadNotFound { id: String },

    #[error("message not found: {id}")]
    MessageNotFound { id: String },

    #[error("database I/O error: {detail}")]
    Io { detail: String },

    #[error("data corruption: {detail}")]
    Corruption { detail: String },
}

/// Convenience Result alias.
pub type Result<T> = std::result::Result<T, ThreadStoreError>;
