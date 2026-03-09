//! Error types for agent configuration.

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AgentConfigError {
    #[error("Agent config directory not found: {0}")]
    DirectoryNotFound(PathBuf),

    #[error("I/O error: {0}")]
    IoError(String),

    #[error("Parse error in {path}: {detail}")]
    ParseError { path: PathBuf, detail: String },

    #[error("Validation error: {0}")]
    ValidationError(String),

    #[error("Agent config already exists: {0}")]
    AlreadyExists(PathBuf),
}
