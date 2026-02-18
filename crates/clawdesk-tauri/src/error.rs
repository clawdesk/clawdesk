//! Shared error types for the Tauri shell layer.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("{0}")]
    Message(String),
}

pub type AppResult<T> = Result<T, AppError>;
