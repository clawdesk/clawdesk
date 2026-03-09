//! Extended error taxonomy — runtime and MCP errors.
//!
//! Complements the core `ClawDeskError` hierarchy with domain-specific error
//! types for the runtime (workflow execution, checkpointing) and MCP protocol
//! layers that don't fit into the existing variants.

use thiserror::Error;

/// Runtime/workflow execution errors.
#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("checkpoint write failed for run {run_id}: {detail}")]
    CheckpointFailed { run_id: String, detail: String },

    #[error("journal replay failed at seq {sequence}: {detail}")]
    JournalReplayFailed { sequence: u64, detail: String },

    #[error("lease expired for run {run_id} (held by {holder})")]
    LeaseExpired { run_id: String, holder: String },

    #[error("lease contention for run {run_id}: {detail}")]
    LeaseContention { run_id: String, detail: String },

    #[error("dead letter: run {run_id} failed after {attempts} attempts")]
    DeadLettered { run_id: String, attempts: u32 },

    #[error("DAG cycle detected: {cycle:?}")]
    DagCycle { cycle: Vec<String> },

    #[error("workflow cancelled: {reason}")]
    Cancelled { reason: String },

    #[error("shutdown in progress")]
    ShuttingDown,

    #[error("backpressure: request shed (queue depth {depth})")]
    BackpressureShed { depth: u64 },

    #[error("sandbox execution failed: {detail}")]
    SandboxFailed { detail: String },
}

/// MCP protocol errors.
#[derive(Debug, Error)]
pub enum McpProtocolError {
    #[error("JSON-RPC parse error: {detail}")]
    ParseError { detail: String },

    #[error("invalid method: {method}")]
    InvalidMethod { method: String },

    #[error("missing required parameter: {param}")]
    MissingParam { param: String },

    #[error("tool {tool} not found on server {server}")]
    ToolNotFound { server: String, tool: String },

    #[error("server {server} connection failed: {detail}")]
    ConnectionFailed { server: String, detail: String },

    #[error("server {server} timed out after {timeout_ms}ms")]
    Timeout { server: String, timeout_ms: u64 },

    #[error("auth failed for server {server}: {detail}")]
    AuthFailed { server: String, detail: String },

    #[error("namespace collision: {namespace}")]
    NamespaceCollision { namespace: String },
}

/// Error severity levels for observability and alerting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ErrorSeverity {
    /// Informational — can be safely ignored.
    Info,
    /// Warning — degraded but functional.
    Warning,
    /// Error — operation failed.
    Error,
    /// Critical — systemic failure, paging required.
    Critical,
}

/// Trait for errors that can report their severity and HTTP status.
pub trait ClassifiableError {
    /// Severity level for alerting.
    fn severity(&self) -> ErrorSeverity;
    /// Suggested HTTP status code for API responses.
    fn http_status(&self) -> u16;
    /// Whether the error is transient and the operation should be retried.
    fn is_transient(&self) -> bool;
}

impl ClassifiableError for RuntimeError {
    fn severity(&self) -> ErrorSeverity {
        match self {
            Self::ShuttingDown | Self::Cancelled { .. } => ErrorSeverity::Info,
            Self::BackpressureShed { .. } | Self::LeaseContention { .. } => ErrorSeverity::Warning,
            Self::CheckpointFailed { .. }
            | Self::JournalReplayFailed { .. }
            | Self::SandboxFailed { .. } => ErrorSeverity::Error,
            Self::DeadLettered { .. } | Self::DagCycle { .. } => ErrorSeverity::Critical,
            Self::LeaseExpired { .. } => ErrorSeverity::Warning,
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::ShuttingDown => 503,
            Self::BackpressureShed { .. } => 429,
            Self::Cancelled { .. } => 499,
            Self::LeaseContention { .. } | Self::LeaseExpired { .. } => 409,
            _ => 500,
        }
    }

    fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::BackpressureShed { .. }
                | Self::LeaseContention { .. }
                | Self::ShuttingDown
        )
    }
}

impl ClassifiableError for McpProtocolError {
    fn severity(&self) -> ErrorSeverity {
        match self {
            Self::ParseError { .. } | Self::MissingParam { .. } => ErrorSeverity::Warning,
            Self::ToolNotFound { .. } | Self::InvalidMethod { .. } => ErrorSeverity::Warning,
            Self::ConnectionFailed { .. } | Self::Timeout { .. } => ErrorSeverity::Error,
            Self::AuthFailed { .. } => ErrorSeverity::Error,
            Self::NamespaceCollision { .. } => ErrorSeverity::Warning,
        }
    }

    fn http_status(&self) -> u16 {
        match self {
            Self::ParseError { .. } | Self::MissingParam { .. } => 400,
            Self::InvalidMethod { .. } | Self::ToolNotFound { .. } => 404,
            Self::AuthFailed { .. } => 401,
            Self::Timeout { .. } => 504,
            Self::ConnectionFailed { .. } => 502,
            Self::NamespaceCollision { .. } => 409,
        }
    }

    fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::ConnectionFailed { .. } | Self::Timeout { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_error_classification() {
        let err = RuntimeError::BackpressureShed { depth: 100 };
        assert_eq!(err.severity(), ErrorSeverity::Warning);
        assert_eq!(err.http_status(), 429);
        assert!(err.is_transient());
    }

    #[test]
    fn mcp_error_classification() {
        let err = McpProtocolError::Timeout {
            server: "test".into(),
            timeout_ms: 5000,
        };
        assert_eq!(err.severity(), ErrorSeverity::Error);
        assert_eq!(err.http_status(), 504);
        assert!(err.is_transient());
    }

    #[test]
    fn critical_errors() {
        let err = RuntimeError::DeadLettered {
            run_id: "run-1".into(),
            attempts: 5,
        };
        assert_eq!(err.severity(), ErrorSeverity::Critical);
        assert!(!err.is_transient());
    }

    #[test]
    fn severity_ordering() {
        assert!(ErrorSeverity::Critical > ErrorSeverity::Error);
        assert!(ErrorSeverity::Error > ErrorSeverity::Warning);
        assert!(ErrorSeverity::Warning > ErrorSeverity::Info);
    }
}
