//! Closed error type hierarchy with exhaustive matching.
//!
//! Every possible error is a variant in `ClawDeskError`. The compiler guarantees
//! exhaustive handling — no unhandled error variant is possible. Error classification
//! is a pure function over the closed union, not regex-based string matching.

use crate::channel::ChannelId;
use crate::session::SessionKey;
use std::time::Duration;
use thiserror::Error;

/// The top-level error type for the entire ClawDesk system.
#[derive(Debug, Error)]
pub enum ClawDeskError {
    #[error("storage: {0}")]
    Storage(#[from] StorageError),

    #[error("provider: {0}")]
    Provider(#[from] ProviderError),

    #[error("channel {channel}: {kind}")]
    Channel {
        channel: ChannelId,
        kind: ChannelErrorKind,
    },

    #[error("agent: {0}")]
    Agent(#[from] AgentError),

    #[error("config: {0}")]
    Config(#[from] ConfigError),

    #[error("gateway: {0}")]
    Gateway(#[from] GatewayError),

    #[error("security: {0}")]
    Security(#[from] SecurityError),
}

/// Storage layer errors.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("database open failed: {detail}")]
    OpenFailed { detail: String },

    #[error("transaction conflict on key {key}")]
    TransactionConflict { key: String },

    #[error("serialization failed: {detail}")]
    SerializationFailed { detail: String },

    #[error("key not found: {key}")]
    NotFound { key: String },

    #[error("WAL corruption detected: {detail}")]
    WalCorruption { detail: String },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Provider/LLM errors — a closed union replacing regex-classified strings.
#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("rate limited by {provider} (retry after {retry_after:?})")]
    RateLimit {
        provider: String,
        retry_after: Option<Duration>,
    },

    #[error("auth failed for {provider} profile {profile_id}")]
    AuthFailure {
        provider: String,
        profile_id: String,
    },

    #[error("timeout after {after:?} calling {provider}/{model}")]
    Timeout {
        provider: String,
        model: String,
        after: Duration,
    },

    #[error("billing issue with {provider}")]
    Billing { provider: String },

    #[error("format error from {provider}: {detail}")]
    FormatError { provider: String, detail: String },

    #[error("{provider} server error (HTTP {status})")]
    ServerError { provider: String, status: u16 },

    #[error("network error calling {provider}: {detail}")]
    NetworkError { provider: String, detail: String },

    #[error("model {model} not found on {provider}")]
    ModelNotFound { provider: String, model: String },

    #[error("context length exceeded for {model}: {detail}")]
    ContextLengthExceeded { model: String, detail: String },
}

impl ProviderError {
    /// Is this error retryable for fallback purposes?
    /// Pure function over the closed union — no regex needed. O(1) pattern match.
    pub fn is_retryable(&self) -> bool {
        match self {
            ProviderError::RateLimit { .. } | ProviderError::Timeout { .. } => true,
            ProviderError::ServerError { status, .. } => *status >= 500,
            _ => false,
        }
    }

    /// Provider name for this error.
    pub fn provider(&self) -> &str {
        match self {
            Self::RateLimit { provider, .. } => provider,
            Self::AuthFailure { provider, .. } => provider,
            Self::Timeout { provider, .. } => provider,
            Self::Billing { provider } => provider,
            Self::FormatError { provider, .. } => provider,
            Self::ServerError { provider, .. } => provider,
            Self::NetworkError { provider, .. } => provider,
            Self::ModelNotFound { provider, .. } => provider,
            Self::ContextLengthExceeded { .. } => "unknown",
        }
    }
}

/// Channel-specific error kinds.
#[derive(Debug, Error)]
pub enum ChannelErrorKind {
    #[error("connection failed: {detail}")]
    ConnectionFailed { detail: String },

    #[error("authentication failed: {detail}")]
    AuthFailed { detail: String },

    #[error("message delivery failed: {detail}")]
    DeliveryFailed { detail: String },

    #[error("rate limited")]
    RateLimited,

    #[error("channel not configured")]
    NotConfigured,

    #[error("pairing required")]
    PairingRequired,
}

/// Agent execution errors.
#[derive(Debug, Error)]
pub enum AgentError {
    #[error("cancelled")]
    Cancelled,

    #[error("tool execution failed: {tool}: {detail}")]
    ToolFailed { tool: String, detail: String },

    #[error("concurrency conflict on session {key}")]
    ConcurrencyConflict { key: SessionKey },

    #[error("all providers exhausted")]
    AllProvidersExhausted,

    #[error("context assembly failed: {detail}")]
    ContextAssemblyFailed { detail: String },

    #[error("max iterations reached: {limit}")]
    MaxIterations { limit: u32 },
}

/// Configuration errors.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("validation failed: {errors:?}")]
    ValidationFailed { errors: Vec<String> },

    #[error("parse error: {detail}")]
    ParseError { detail: String },

    #[error("unknown provider: {name}")]
    UnknownProvider { name: String },

    #[error("missing required field: {field}")]
    MissingField { field: String },
}

/// Gateway server errors.
#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("bind failed on {addr}: {detail}")]
    BindFailed { addr: String, detail: String },

    #[error("websocket error: {detail}")]
    WebSocket { detail: String },

    #[error("authentication required")]
    AuthRequired,

    #[error("invalid token")]
    InvalidToken,
}

/// Security boundary errors.
#[derive(Debug, Error)]
pub enum SecurityError {
    #[error("command denied: {command}")]
    CommandDenied { command: String },

    #[error("capability not granted: {capability}")]
    CapabilityDenied { capability: String },

    #[error("sandbox violation: {detail}")]
    SandboxViolation { detail: String },

    #[error("content blocked: {reason}")]
    ContentBlocked { reason: String },

    #[error("audit log write failed: {detail}")]
    AuditFailed { detail: String },

    #[error("skill scan failed for {skill}: {detail}")]
    SkillScanFailed { skill: String, detail: String },
}

/// Plugin system errors.
#[derive(Debug, Error)]
pub enum PluginError {
    #[error("plugin {name} not found")]
    NotFound { name: String },

    #[error("plugin {name} load failed: {detail}")]
    LoadFailed { name: String, detail: String },

    #[error("plugin {name} activation failed: {detail}")]
    ActivationFailed { name: String, detail: String },

    #[error("circular dependency detected: {cycle:?}")]
    CircularDependency { cycle: Vec<String> },

    #[error("plugin {name} incompatible: requires SDK {required}, have {actual}")]
    IncompatibleSdk {
        name: String,
        required: String,
        actual: String,
    },

    #[error("plugin {name} timed out after {timeout_secs}s")]
    Timeout { name: String, timeout_secs: u64 },
}

/// Media processing errors.
#[derive(Debug, Error)]
pub enum MediaError {
    #[error("transcription failed for {media_type}: {detail}")]
    TranscriptionFailed { media_type: String, detail: String },

    #[error("unsupported media type: {mime_type}")]
    UnsupportedType { mime_type: String },

    #[error("media too large: {size_bytes} bytes (max {max_bytes})")]
    TooLarge { size_bytes: u64, max_bytes: u64 },

    #[error("provider {provider} unavailable: {detail}")]
    ProviderUnavailable { provider: String, detail: String },
}

/// Cron/scheduling errors.
#[derive(Debug, Error)]
pub enum CronError {
    #[error("invalid cron expression: {expr}")]
    InvalidExpression { expr: String },

    #[error("cron task {id} timed out after {timeout_secs}s")]
    TaskTimeout { id: String, timeout_secs: u64 },

    #[error("cron task {id} overlapping with running instance")]
    Overlapping { id: String },

    #[error("delivery failed for task {id}: {detail}")]
    DeliveryFailed { id: String, detail: String },
}

/// Memory/embedding errors.
#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("embedding generation failed: {detail}")]
    EmbeddingFailed { detail: String },

    #[error("vector store error: {detail}")]
    VectorStoreError { detail: String },

    #[error("sync pipeline error: {detail}")]
    SyncError { detail: String },

    #[error("reindex in progress")]
    ReindexInProgress,
}

impl ClawDeskError {
    /// Classify whether this error is retryable.
    pub fn is_retryable(&self) -> bool {
        match self {
            ClawDeskError::Provider(e) => e.is_retryable(),
            ClawDeskError::Storage(StorageError::TransactionConflict { .. }) => true,
            ClawDeskError::Gateway(GatewayError::WebSocket { .. }) => true,
            _ => false,
        }
    }

    /// Get a structured error code for API responses.
    pub fn error_code(&self) -> &'static str {
        match self {
            ClawDeskError::Storage(_) => "STORAGE_ERROR",
            ClawDeskError::Provider(e) => match e {
                ProviderError::RateLimit { .. } => "RATE_LIMITED",
                ProviderError::AuthFailure { .. } => "AUTH_FAILED",
                ProviderError::ContextLengthExceeded { .. } => "CONTEXT_OVERFLOW",
                ProviderError::Billing { .. } => "BILLING_ERROR",
                _ => "PROVIDER_ERROR",
            },
            ClawDeskError::Channel { .. } => "CHANNEL_ERROR",
            ClawDeskError::Agent(e) => match e {
                AgentError::Cancelled => "CANCELLED",
                AgentError::AllProvidersExhausted => "PROVIDERS_EXHAUSTED",
                AgentError::MaxIterations { .. } => "MAX_ITERATIONS",
                _ => "AGENT_ERROR",
            },
            ClawDeskError::Config(_) => "CONFIG_ERROR",
            ClawDeskError::Gateway(_) => "GATEWAY_ERROR",
            ClawDeskError::Security(_) => "SECURITY_ERROR",
        }
    }
}

/// Convenience Result type alias.
pub type Result<T> = std::result::Result<T, ClawDeskError>;
