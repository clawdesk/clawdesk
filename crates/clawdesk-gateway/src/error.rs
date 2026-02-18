//! Typed error handling for the Axum gateway API.
//!
//! `ApiError` replaces ad-hoc `(StatusCode, String)` tuples with a structured
//! error type that:
//! 1. Implements `IntoResponse` for Axum (structured JSON error bodies)
//! 2. Maps `ClawDeskError` variants to appropriate HTTP status codes
//! 3. Returns machine-readable error codes alongside human messages
//!
//! ## Wire format
//!
//! ```json
//! {
//!   "error": {
//!     "code": "RATE_LIMITED",
//!     "message": "rate limited by anthropic (retry after 30s)",
//!     "retryable": true
//!   }
//! }
//! ```

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use clawdesk_types::error::{
    AgentError, ClawDeskError, GatewayError, ProviderError, SecurityError, StorageError,
};
use serde::Serialize;

/// Structured API error for the gateway.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// Errors from the core domain hierarchy.
    #[error("{0}")]
    Domain(#[from] ClawDeskError),

    /// Entity not found.
    #[error("{entity} '{id}' not found")]
    NotFound { entity: &'static str, id: String },

    /// Thread ownership conflict.
    #[error("thread '{thread_id}' busy, retry in {retry_after_ms}ms")]
    ThreadBusy {
        thread_id: String,
        owner_id: String,
        retry_after_ms: u64,
    },

    /// Internal server error (escape hatch for migration).
    #[error("{0}")]
    Internal(String),
}

/// JSON error body returned in API responses.
#[derive(Debug, Serialize)]
struct ErrorBody {
    error: ErrorDetail,
}

#[derive(Debug, Serialize)]
struct ErrorDetail {
    code: &'static str,
    message: String,
    retryable: bool,
}

impl ApiError {
    /// Map to HTTP status code.
    fn status_code(&self) -> StatusCode {
        match self {
            ApiError::Domain(e) => match e {
                ClawDeskError::Provider(pe) => match pe {
                    ProviderError::RateLimit { .. } => StatusCode::TOO_MANY_REQUESTS,
                    ProviderError::AuthFailure { .. } => StatusCode::UNAUTHORIZED,
                    ProviderError::Timeout { .. } => StatusCode::GATEWAY_TIMEOUT,
                    ProviderError::ModelNotFound { .. } => StatusCode::NOT_FOUND,
                    ProviderError::ContextLengthExceeded { .. } => StatusCode::PAYLOAD_TOO_LARGE,
                    _ => StatusCode::BAD_GATEWAY,
                },
                ClawDeskError::Storage(StorageError::NotFound { .. }) => StatusCode::NOT_FOUND,
                ClawDeskError::Storage(_) => StatusCode::INTERNAL_SERVER_ERROR,
                ClawDeskError::Agent(AgentError::Cancelled) => StatusCode::from_u16(499).unwrap_or(StatusCode::BAD_REQUEST),
                ClawDeskError::Agent(AgentError::AllProvidersExhausted) => StatusCode::SERVICE_UNAVAILABLE,
                ClawDeskError::Agent(_) => StatusCode::INTERNAL_SERVER_ERROR,
                ClawDeskError::Security(_) => StatusCode::FORBIDDEN,
                ClawDeskError::Gateway(GatewayError::AuthRequired | GatewayError::InvalidToken) => StatusCode::UNAUTHORIZED,
                ClawDeskError::Gateway(_) => StatusCode::INTERNAL_SERVER_ERROR,
                ClawDeskError::Config(_) => StatusCode::BAD_REQUEST,
                ClawDeskError::Channel { .. } => StatusCode::BAD_GATEWAY,
            },
            ApiError::NotFound { .. } => StatusCode::NOT_FOUND,
            ApiError::ThreadBusy { .. } => StatusCode::CONFLICT,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    /// Machine-readable error code.
    fn error_code(&self) -> &'static str {
        match self {
            ApiError::Domain(e) => e.error_code(),
            ApiError::NotFound { .. } => "NOT_FOUND",
            ApiError::ThreadBusy { .. } => "THREAD_BUSY",
            ApiError::Internal(_) => "INTERNAL_ERROR",
        }
    }

    /// Is this error retryable?
    fn is_retryable(&self) -> bool {
        match self {
            ApiError::Domain(e) => e.is_retryable(),
            ApiError::ThreadBusy { .. } => true,
            _ => false,
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status_code();
        let body = ErrorBody {
            error: ErrorDetail {
                code: self.error_code(),
                message: self.to_string(),
                retryable: self.is_retryable(),
            },
        };
        (status, Json(body)).into_response()
    }
}

// Convenience conversions for common error sources

impl From<String> for ApiError {
    fn from(s: String) -> Self {
        ApiError::Internal(s)
    }
}

impl<T> From<std::sync::PoisonError<T>> for ApiError {
    fn from(e: std::sync::PoisonError<T>) -> Self {
        ApiError::Internal(format!("lock poisoned: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_maps_to_429() {
        let err = ApiError::Domain(ClawDeskError::Provider(ProviderError::RateLimit {
            provider: "anthropic".into(),
            retry_after: Some(std::time::Duration::from_secs(30)),
        }));
        assert_eq!(err.status_code(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(err.error_code(), "RATE_LIMITED");
        assert!(err.is_retryable());
    }

    #[test]
    fn not_found_maps_to_404() {
        let err = ApiError::NotFound {
            entity: "session",
            id: "abc-123".into(),
        };
        assert_eq!(err.status_code(), StatusCode::NOT_FOUND);
        assert!(!err.is_retryable());
    }

    #[test]
    fn thread_busy_maps_to_409() {
        let err = ApiError::ThreadBusy {
            thread_id: "t1".into(),
            owner_id: "req-1".into(),
            retry_after_ms: 500,
        };
        assert_eq!(err.status_code(), StatusCode::CONFLICT);
        assert!(err.is_retryable());
    }
}
