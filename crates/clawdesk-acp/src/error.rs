//! Compositional error algebra for the ACP crate.
//!
//! Errors carry causal chains, severity levels, and retryability metadata,
//! enabling both precise test assertions and production root-cause analysis.
//!
//! ## Error algebra
//!
//! Model errors as a free monad over a base error functor:
//!   `Error<E> = E | Caused(E, Box<Error<E>>)`
//!
//! The causal chain has depth `d` bounded by the pipeline length (≤ 6 for ACP:
//! discovery → decode → delegate → route → stream → notify).
//!
//! Retryability is a lattice:
//!   `Retryable ≤ RetryableWithBackoff ≤ NonRetryable`
//!
//! The composed retryability of a causal chain is the join (least upper bound)
//! of its components — monotonicity guarantees no unsafe retries.

use std::fmt;
use std::time::Duration;

/// Severity levels forming a total order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Informational — operation succeeded with caveats.
    Info,
    /// Warning — degraded behavior, should be investigated.
    Warning,
    /// Error — operation failed, may be retryable.
    Error,
    /// Critical — system integrity at risk, requires immediate attention.
    Critical,
}

/// Retryability lattice: `Retryable ≤ RetryableWithBackoff ≤ NonRetryable`.
///
/// The join (least upper bound) of a causal chain's retryability determines
/// the composed retryability. This is monotone: the most restrictive component
/// wins, preventing unsafe retries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Retryability {
    /// Can be retried immediately.
    Retryable,
    /// Can be retried with exponential backoff.
    RetryableWithBackoff {
        /// Suggested initial backoff.
        base_delay_ms: u64,
        /// Maximum number of retries.
        max_retries: u32,
    },
    /// Should not be retried — permanent failure.
    NonRetryable,
}

impl Retryability {
    /// Join (least upper bound) of two retryabilities.
    pub fn join(self, other: Self) -> Self {
        match (&self, &other) {
            (Self::NonRetryable, _) | (_, Self::NonRetryable) => Self::NonRetryable,
            (Self::RetryableWithBackoff { base_delay_ms: d1, max_retries: r1 },
             Self::RetryableWithBackoff { base_delay_ms: d2, max_retries: r2 }) => {
                Self::RetryableWithBackoff {
                    base_delay_ms: (*d1).max(*d2),
                    max_retries: (*r1).min(*r2),
                }
            }
            (Self::RetryableWithBackoff { .. }, _) => self,
            (_, Self::RetryableWithBackoff { .. }) => other,
            _ => Self::Retryable,
        }
    }

    /// Whether this error can be retried at all.
    pub fn is_retryable(&self) -> bool {
        !matches!(self, Self::NonRetryable)
    }
}

/// ACP-specific error variants covering the full protocol pipeline.
#[derive(Debug)]
pub enum AcpErrorKind {
    // ── Discovery phase ──
    /// Failed to fetch agent card from `/.well-known/agent.json`.
    DiscoveryFailed {
        url: String,
        detail: String,
    },
    /// Agent card JSON was malformed or missing required fields.
    InvalidAgentCard {
        agent_id: Option<String>,
        detail: String,
    },

    // ── Capability decode phase ──
    /// Capability bitset decoding error (e.g., unknown bit index).
    CapabilityDecodeError {
        bitset: u64,
        detail: String,
    },

    // ── Routing phase ──
    /// No agent matched the required capabilities.
    NoMatchingAgent {
        required_capabilities: Vec<String>,
    },
    /// Agent is registered but unhealthy.
    AgentUnhealthy {
        agent_id: String,
    },
    /// Agent is at capacity (max concurrent tasks reached).
    AgentAtCapacity {
        agent_id: String,
        current_tasks: u32,
        max_tasks: u32,
    },

    // ── Task delegation phase ──
    /// Task creation failed.
    TaskCreationFailed {
        detail: String,
    },
    /// Invalid state transition in the task FSM.
    InvalidTaskTransition {
        task_id: String,
        from_state: String,
        event: String,
    },

    // ── Streaming phase ──
    /// SSE connection failed or dropped.
    StreamConnectionFailed {
        endpoint: String,
        detail: String,
    },
    /// SSE stream timeout — consumer too slow or producer stalled.
    StreamTimeout {
        task_id: String,
        timeout: Duration,
    },
    /// Backpressure overflow — consumer cannot keep up.
    BackpressureOverflow {
        buffer_depth: usize,
        drop_policy: String,
    },

    // ── Notification phase ──
    /// Push notification delivery failed.
    NotificationFailed {
        target: String,
        detail: String,
    },

    // ── General ──
    /// Network-level error (connection reset, DNS failure, etc.).
    Network {
        detail: String,
    },
    /// Authentication or authorization failure.
    Auth {
        detail: String,
    },
    /// Serialization or protocol format error.
    Protocol {
        detail: String,
    },
    /// Internal error — invariant violation.
    Internal {
        detail: String,
    },
}

impl fmt::Display for AcpErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DiscoveryFailed { url, detail } => write!(f, "discovery failed for {url}: {detail}"),
            Self::InvalidAgentCard { agent_id, detail } => {
                if let Some(id) = agent_id {
                    write!(f, "invalid agent card for {id}: {detail}")
                } else {
                    write!(f, "invalid agent card: {detail}")
                }
            }
            Self::CapabilityDecodeError { bitset, detail } => write!(f, "capability decode error (bitset={bitset:#x}): {detail}"),
            Self::NoMatchingAgent { required_capabilities } => write!(f, "no agent matches capabilities: {:?}", required_capabilities),
            Self::AgentUnhealthy { agent_id } => write!(f, "agent {agent_id} is unhealthy"),
            Self::AgentAtCapacity { agent_id, current_tasks, max_tasks } => write!(f, "agent {agent_id} at capacity ({current_tasks}/{max_tasks})"),
            Self::TaskCreationFailed { detail } => write!(f, "task creation failed: {detail}"),
            Self::InvalidTaskTransition { task_id, from_state, event } => write!(f, "invalid transition on task {task_id}: {from_state} + {event}"),
            Self::StreamConnectionFailed { endpoint, detail } => write!(f, "SSE connection to {endpoint} failed: {detail}"),
            Self::StreamTimeout { task_id, timeout } => write!(f, "stream timeout on task {task_id} after {timeout:?}"),
            Self::BackpressureOverflow { buffer_depth, drop_policy } => write!(f, "backpressure overflow (buffer={buffer_depth}, policy={drop_policy})"),
            Self::NotificationFailed { target, detail } => write!(f, "notification to {target} failed: {detail}"),
            Self::Network { detail } => write!(f, "network error: {detail}"),
            Self::Auth { detail } => write!(f, "auth error: {detail}"),
            Self::Protocol { detail } => write!(f, "protocol error: {detail}"),
            Self::Internal { detail } => write!(f, "internal error: {detail}"),
        }
    }
}

impl AcpErrorKind {
    /// Inherent severity of this error kind.
    pub fn severity(&self) -> Severity {
        match self {
            Self::BackpressureOverflow { .. } | Self::Internal { .. } => Severity::Critical,
            Self::DiscoveryFailed { .. }
            | Self::StreamConnectionFailed { .. }
            | Self::StreamTimeout { .. }
            | Self::Network { .. }
            | Self::TaskCreationFailed { .. } => Severity::Error,
            Self::AgentUnhealthy { .. }
            | Self::AgentAtCapacity { .. }
            | Self::NotificationFailed { .. } => Severity::Warning,
            _ => Severity::Error,
        }
    }

    /// Inherent retryability of this error kind.
    pub fn retryability(&self) -> Retryability {
        match self {
            Self::Network { .. }
            | Self::StreamConnectionFailed { .. }
            | Self::AgentUnhealthy { .. } => Retryability::RetryableWithBackoff {
                base_delay_ms: 1000,
                max_retries: 3,
            },
            Self::StreamTimeout { .. }
            | Self::BackpressureOverflow { .. }
            | Self::AgentAtCapacity { .. } => Retryability::RetryableWithBackoff {
                base_delay_ms: 2000,
                max_retries: 5,
            },
            Self::NotificationFailed { .. } => Retryability::Retryable,
            Self::Auth { .. }
            | Self::InvalidAgentCard { .. }
            | Self::CapabilityDecodeError { .. }
            | Self::InvalidTaskTransition { .. }
            | Self::Protocol { .. }
            | Self::Internal { .. } => Retryability::NonRetryable,
            _ => Retryability::NonRetryable,
        }
    }
}

/// ACP error with causal chain provenance.
///
/// The causal chain is a linked list: `Error<E> = E | Caused(E, Box<Error<E>>)`.
/// Depth is bounded by the pipeline length (≤ 6 for ACP).
/// Cause lookup by type is `O(d)` via downcast — effectively `O(1)` for `d ≤ 6`.
pub struct AcpError {
    /// The error that occurred.
    pub kind: AcpErrorKind,
    /// Causal chain — the source error that led to this one.
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

impl AcpError {
    /// Create a new ACP error.
    pub fn new(kind: AcpErrorKind) -> Self {
        Self { kind, source: None }
    }

    /// Wrap a source error as the cause.
    pub fn caused_by<E: std::error::Error + Send + Sync + 'static>(mut self, source: E) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    /// Wrap a boxed source error.
    pub fn with_source(mut self, source: Box<dyn std::error::Error + Send + Sync + 'static>) -> Self {
        self.source = Some(source);
        self
    }

    /// Severity of this error (max of self and cause chain).
    pub fn severity(&self) -> Severity {
        let mut sev = self.kind.severity();
        let mut current: Option<&(dyn std::error::Error + 'static)> = self.source.as_deref().map(|e| e as &(dyn std::error::Error + 'static));
        while let Some(cause) = current {
            if let Some(acp) = cause.downcast_ref::<AcpError>() {
                sev = sev.max(acp.kind.severity());
            }
            current = cause.source();
        }
        sev
    }

    /// Retryability of this error (join of self and cause chain).
    pub fn retryability(&self) -> Retryability {
        let mut retry = self.kind.retryability();
        let mut current: Option<&(dyn std::error::Error + 'static)> = self.source.as_deref().map(|e| e as &(dyn std::error::Error + 'static));
        while let Some(cause) = current {
            if let Some(acp) = cause.downcast_ref::<AcpError>() {
                retry = retry.join(acp.kind.retryability());
            }
            current = cause.source();
        }
        retry
    }

    /// Whether this error is retryable.
    pub fn is_retryable(&self) -> bool {
        self.retryability().is_retryable()
    }

    /// Check if any error in the causal chain is of the specified kind.
    pub fn is_timeout(&self) -> bool {
        matches!(self.kind, AcpErrorKind::StreamTimeout { .. })
            || self.caused_by_pred(|e| matches!(e.kind, AcpErrorKind::StreamTimeout { .. }))
    }

    /// Check if the causal chain contains a network error.
    pub fn is_network_error(&self) -> bool {
        matches!(self.kind, AcpErrorKind::Network { .. } | AcpErrorKind::StreamConnectionFailed { .. })
            || self.caused_by_pred(|e| matches!(e.kind, AcpErrorKind::Network { .. } | AcpErrorKind::StreamConnectionFailed { .. }))
    }

    /// Check if any cause in the chain satisfies a predicate.
    pub fn caused_by_pred(&self, pred: impl Fn(&AcpError) -> bool) -> bool {
        let mut current: Option<&(dyn std::error::Error + 'static)> = self.source.as_deref().map(|e| e as &(dyn std::error::Error + 'static));
        while let Some(cause) = current {
            if let Some(acp) = cause.downcast_ref::<AcpError>() {
                if pred(acp) {
                    return true;
                }
            }
            current = cause.source();
        }
        false
    }

    /// Walk the causal chain and collect all ACP errors.
    pub fn causal_chain(&self) -> Vec<&AcpErrorKind> {
        let mut chain = vec![&self.kind];
        let mut current: Option<&(dyn std::error::Error + 'static)> = self.source.as_deref().map(|e| e as &(dyn std::error::Error + 'static));
        while let Some(cause) = current {
            if let Some(acp) = cause.downcast_ref::<AcpError>() {
                chain.push(&acp.kind);
            }
            current = cause.source();
        }
        chain
    }

    /// Error code for structured logging / API responses.
    pub fn error_code(&self) -> &'static str {
        match &self.kind {
            AcpErrorKind::DiscoveryFailed { .. } => "ACP_DISCOVERY_FAILED",
            AcpErrorKind::InvalidAgentCard { .. } => "ACP_INVALID_AGENT_CARD",
            AcpErrorKind::CapabilityDecodeError { .. } => "ACP_CAPABILITY_DECODE",
            AcpErrorKind::NoMatchingAgent { .. } => "ACP_NO_MATCH",
            AcpErrorKind::AgentUnhealthy { .. } => "ACP_AGENT_UNHEALTHY",
            AcpErrorKind::AgentAtCapacity { .. } => "ACP_AGENT_CAPACITY",
            AcpErrorKind::TaskCreationFailed { .. } => "ACP_TASK_CREATION",
            AcpErrorKind::InvalidTaskTransition { .. } => "ACP_INVALID_TRANSITION",
            AcpErrorKind::StreamConnectionFailed { .. } => "ACP_STREAM_CONNECT",
            AcpErrorKind::StreamTimeout { .. } => "ACP_STREAM_TIMEOUT",
            AcpErrorKind::BackpressureOverflow { .. } => "ACP_BACKPRESSURE",
            AcpErrorKind::NotificationFailed { .. } => "ACP_NOTIFICATION",
            AcpErrorKind::Network { .. } => "ACP_NETWORK",
            AcpErrorKind::Auth { .. } => "ACP_AUTH",
            AcpErrorKind::Protocol { .. } => "ACP_PROTOCOL",
            AcpErrorKind::Internal { .. } => "ACP_INTERNAL",
        }
    }
}

impl fmt::Debug for AcpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AcpError")
            .field("kind", &self.kind)
            .field("severity", &self.severity())
            .field("retryable", &self.is_retryable())
            .field("source", &self.source.as_ref().map(|s| s.to_string()))
            .finish()
    }
}

impl fmt::Display for AcpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)?;
        if let Some(ref source) = self.source {
            write!(f, ": caused by: {}", source)?;
        }
        Ok(())
    }
}

impl std::error::Error for AcpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|e| e as &(dyn std::error::Error + 'static))
    }
}

impl From<String> for AcpError {
    fn from(s: String) -> Self {
        Self::new(AcpErrorKind::Internal { detail: s })
    }
}

impl From<&str> for AcpError {
    fn from(s: &str) -> Self {
        Self::new(AcpErrorKind::Internal { detail: s.to_string() })
    }
}

/// Convenience type alias.
pub type AcpResult<T> = std::result::Result<T, AcpError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn causal_chain_propagation() {
        let root = AcpError::new(AcpErrorKind::Network {
            detail: "connection reset".into(),
        });
        let mid = AcpError::new(AcpErrorKind::StreamConnectionFailed {
            endpoint: "http://agent.local".into(),
            detail: "SSE reconnect failed".into(),
        })
        .caused_by(root);
        let surface = AcpError::new(AcpErrorKind::TaskCreationFailed {
            detail: "delegation aborted".into(),
        })
        .caused_by(mid);

        assert_eq!(surface.causal_chain().len(), 3);
        assert!(surface.is_network_error());
    }

    #[test]
    fn retryability_lattice_join() {
        let retryable = Retryability::Retryable;
        let backoff = Retryability::RetryableWithBackoff {
            base_delay_ms: 1000,
            max_retries: 3,
        };
        let non_retryable = Retryability::NonRetryable;

        // Join is monotone: non-retryable dominates.
        assert_eq!(retryable.join(non_retryable), Retryability::NonRetryable);
        assert!(backoff.join(retryable).is_retryable());
        assert!(!non_retryable.join(retryable).is_retryable());
    }

    #[test]
    fn severity_max_in_chain() {
        let root = AcpError::new(AcpErrorKind::Internal {
            detail: "invariant violated".into(),
        });
        let surface = AcpError::new(AcpErrorKind::AgentUnhealthy {
            agent_id: "test".into(),
        })
        .caused_by(root);

        // Internal is Critical, AgentUnhealthy is Warning → max = Critical.
        assert_eq!(surface.severity(), Severity::Critical);
    }

    #[test]
    fn semantic_predicates_for_test_assertions() {
        let err = AcpError::new(AcpErrorKind::StreamTimeout {
            task_id: "task-1".into(),
            timeout: Duration::from_secs(30),
        });
        assert!(err.is_timeout());
        assert!(err.is_retryable());
        assert_eq!(err.error_code(), "ACP_STREAM_TIMEOUT");
    }

    #[test]
    fn from_string_conversion() {
        let err: AcpError = "something went wrong".into();
        assert_eq!(err.error_code(), "ACP_INTERNAL");
        assert!(!err.is_retryable());
    }
}
