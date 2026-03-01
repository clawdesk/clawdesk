//! Structured error types for the Tauri shell layer.
//!
//! Commands return `Result<T, String>` (Tauri convention). This module provides
//! [`CommandError`] — a serializable error envelope that gives the frontend
//! `{ code, message, retryable }` instead of raw strings.

use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    Message(String),
}

pub type AppResult<T> = Result<T, AppError>;

/// Structured error envelope returned to the frontend via IPC.
///
/// Serialized as JSON when converted to String via `Into<String>`.
/// The frontend can parse the JSON to obtain machine-readable error codes.
#[derive(Debug, Clone, Serialize)]
pub struct CommandError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl CommandError {
    pub fn new(code: impl Into<String>, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
        }
    }

    /// Convert from a `ClawDeskError` preserving taxonomy codes.
    pub fn from_clawdesk(err: &clawdesk_types::error::ClawDeskError) -> Self {
        Self {
            code: err.error_code().to_string(),
            message: err.to_string(),
            retryable: err.is_retryable(),
        }
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Serialize as JSON so the frontend can parse structured errors
        match serde_json::to_string(self) {
            Ok(json) => write!(f, "{}", json),
            Err(_) => write!(f, "{}", self.message),
        }
    }
}

impl From<CommandError> for String {
    fn from(err: CommandError) -> String {
        err.to_string()
    }
}

impl From<clawdesk_types::error::ClawDeskError> for CommandError {
    fn from(err: clawdesk_types::error::ClawDeskError) -> Self {
        Self::from_clawdesk(&err)
    }
}
