//! Compositional error algebra for the Media crate.
//!
//! Mirrors the ACP error algebra with media-specific error variants,
//! causal chains, severity levels, and retryability metadata.
//!
//! Errors carry their full causal chain (source → intermediate → surface).
//! Retry logic inspects `error.is_retryable()` instead of pattern-matching.
//! Test assertions use semantic predicates (`error.is_timeout()`,
//! `error.is_format_error()`) instead of structural matching.

use std::fmt;
use std::time::Duration;

/// Severity levels (shared with ACP error algebra).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Info,
    Warning,
    Error,
    Critical,
}

/// Retryability lattice (shared with ACP error algebra).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Retryability {
    Retryable,
    RetryableWithBackoff { base_delay_ms: u64, max_retries: u32 },
    NonRetryable,
}

impl Retryability {
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

    pub fn is_retryable(&self) -> bool {
        !matches!(self, Self::NonRetryable)
    }
}

/// Media-specific error variants covering the full processing pipeline:
/// ingest → cache check → pipeline → process → select → output.
#[derive(Debug)]
pub enum MediaErrorKind {
    // ── Ingest phase ──
    /// Failed to read or fetch input media.
    IngestFailed {
        source_type: String,
        detail: String,
    },
    /// Input exceeds size limit.
    InputTooLarge {
        size_bytes: u64,
        max_bytes: u64,
    },
    /// Unsupported media format.
    UnsupportedFormat {
        mime_type: String,
    },

    // ── Cache phase ──
    /// Cache corruption detected.
    CacheCorruption {
        key: String,
        detail: String,
    },
    /// Cache I/O error.
    CacheIo {
        detail: String,
    },

    // ── Pipeline phase ──
    /// No processor available for media type.
    NoProcessorAvailable {
        media_type: String,
    },
    /// Pipeline concurrency limit reached.
    ConcurrencyLimitReached {
        media_type: String,
        limit: usize,
    },
    /// Pipeline DAG scheduling error.
    SchedulingError {
        detail: String,
    },

    // ── Processor phase ──
    /// External API (Whisper, Vision, etc.) returned an error.
    ProcessorApiFailed {
        processor: String,
        status: Option<u16>,
        detail: String,
    },
    /// Processor timeout.
    ProcessorTimeout {
        processor: String,
        timeout: Duration,
    },
    /// All processors failed (circuit breaker open).
    AllProcessorsFailed {
        media_type: String,
        attempts: usize,
    },

    // ── Selector phase ──
    /// Format selector found no matching processor chain.
    SelectorNoMatch {
        mime_type: String,
        detail: String,
    },

    // ── Output phase ──
    /// Output serialization or delivery failed.
    OutputFailed {
        detail: String,
    },

    // ── Voice pipeline ──
    /// Voice activity detection error.
    VadError {
        detail: String,
    },
    /// Speech-to-text or text-to-speech engine error.
    SpeechEngineError {
        engine: String,
        detail: String,
    },

    // ── General ──
    /// Network error during media fetch/upload.
    Network {
        detail: String,
    },
    /// Internal invariant violation.
    Internal {
        detail: String,
    },
}

impl fmt::Display for MediaErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::IngestFailed { source_type, detail } => write!(f, "ingest failed ({source_type}): {detail}"),
            Self::InputTooLarge { size_bytes, max_bytes } => write!(f, "input too large: {size_bytes} bytes (max {max_bytes})"),
            Self::UnsupportedFormat { mime_type } => write!(f, "unsupported format: {mime_type}"),
            Self::CacheCorruption { key, detail } => write!(f, "cache corruption (key={key}): {detail}"),
            Self::CacheIo { detail } => write!(f, "cache I/O: {detail}"),
            Self::NoProcessorAvailable { media_type } => write!(f, "no processor for {media_type}"),
            Self::ConcurrencyLimitReached { media_type, limit } => write!(f, "concurrency limit ({limit}) for {media_type}"),
            Self::SchedulingError { detail } => write!(f, "scheduling error: {detail}"),
            Self::ProcessorApiFailed { processor, status, detail } => {
                if let Some(s) = status {
                    write!(f, "{processor} API error (HTTP {s}): {detail}")
                } else {
                    write!(f, "{processor} API error: {detail}")
                }
            }
            Self::ProcessorTimeout { processor, timeout } => write!(f, "{processor} timeout after {timeout:?}"),
            Self::AllProcessorsFailed { media_type, attempts } => write!(f, "all processors failed for {media_type} ({attempts} attempts)"),
            Self::SelectorNoMatch { mime_type, detail } => write!(f, "no selector match for {mime_type}: {detail}"),
            Self::OutputFailed { detail } => write!(f, "output failed: {detail}"),
            Self::VadError { detail } => write!(f, "VAD error: {detail}"),
            Self::SpeechEngineError { engine, detail } => write!(f, "{engine} error: {detail}"),
            Self::Network { detail } => write!(f, "network: {detail}"),
            Self::Internal { detail } => write!(f, "internal: {detail}"),
        }
    }
}

impl MediaErrorKind {
    pub fn severity(&self) -> Severity {
        match self {
            Self::CacheCorruption { .. } | Self::Internal { .. } => Severity::Critical,
            Self::AllProcessorsFailed { .. }
            | Self::ProcessorApiFailed { .. }
            | Self::ProcessorTimeout { .. }
            | Self::Network { .. }
            | Self::IngestFailed { .. } => Severity::Error,
            Self::ConcurrencyLimitReached { .. }
            | Self::NoProcessorAvailable { .. } => Severity::Warning,
            _ => Severity::Error,
        }
    }

    pub fn retryability(&self) -> Retryability {
        match self {
            Self::Network { .. }
            | Self::ProcessorTimeout { .. } => Retryability::RetryableWithBackoff {
                base_delay_ms: 1000,
                max_retries: 3,
            },
            Self::ConcurrencyLimitReached { .. } => Retryability::RetryableWithBackoff {
                base_delay_ms: 500,
                max_retries: 5,
            },
            Self::ProcessorApiFailed { status: Some(s), .. } if *s >= 500 => {
                Retryability::RetryableWithBackoff {
                    base_delay_ms: 2000,
                    max_retries: 3,
                }
            }
            Self::AllProcessorsFailed { .. } => Retryability::RetryableWithBackoff {
                base_delay_ms: 5000,
                max_retries: 2,
            },
            Self::CacheIo { .. } => Retryability::Retryable,
            Self::UnsupportedFormat { .. }
            | Self::InputTooLarge { .. }
            | Self::CacheCorruption { .. }
            | Self::Internal { .. } => Retryability::NonRetryable,
            _ => Retryability::NonRetryable,
        }
    }
}

/// Media error with causal chain.
pub struct MediaError {
    pub kind: MediaErrorKind,
    source: Option<Box<dyn std::error::Error + Send + Sync + 'static>>,
}

impl MediaError {
    pub fn new(kind: MediaErrorKind) -> Self {
        Self { kind, source: None }
    }

    pub fn caused_by<E: std::error::Error + Send + Sync + 'static>(mut self, source: E) -> Self {
        self.source = Some(Box::new(source));
        self
    }

    pub fn with_source(mut self, source: Box<dyn std::error::Error + Send + Sync + 'static>) -> Self {
        self.source = Some(source);
        self
    }

    pub fn severity(&self) -> Severity {
        let mut sev = self.kind.severity();
        let mut current: Option<&(dyn std::error::Error + 'static)> = self.source.as_deref().map(|e| e as &(dyn std::error::Error + 'static));
        while let Some(cause) = current {
            if let Some(me) = cause.downcast_ref::<MediaError>() {
                sev = sev.max(me.kind.severity());
            }
            current = cause.source();
        }
        sev
    }

    pub fn retryability(&self) -> Retryability {
        let mut retry = self.kind.retryability();
        let mut current: Option<&(dyn std::error::Error + 'static)> = self.source.as_deref().map(|e| e as &(dyn std::error::Error + 'static));
        while let Some(cause) = current {
            if let Some(me) = cause.downcast_ref::<MediaError>() {
                retry = retry.join(me.kind.retryability());
            }
            current = cause.source();
        }
        retry
    }

    pub fn is_retryable(&self) -> bool {
        self.retryability().is_retryable()
    }

    pub fn is_timeout(&self) -> bool {
        matches!(self.kind, MediaErrorKind::ProcessorTimeout { .. })
            || self.caused_by_pred(|e| matches!(e.kind, MediaErrorKind::ProcessorTimeout { .. }))
    }

    pub fn is_format_error(&self) -> bool {
        matches!(self.kind, MediaErrorKind::UnsupportedFormat { .. })
            || self.caused_by_pred(|e| matches!(e.kind, MediaErrorKind::UnsupportedFormat { .. }))
    }

    pub fn is_cache_error(&self) -> bool {
        matches!(self.kind, MediaErrorKind::CacheCorruption { .. } | MediaErrorKind::CacheIo { .. })
            || self.caused_by_pred(|e| matches!(e.kind, MediaErrorKind::CacheCorruption { .. } | MediaErrorKind::CacheIo { .. }))
    }

    pub fn caused_by_pred(&self, pred: impl Fn(&MediaError) -> bool) -> bool {
        let mut current: Option<&(dyn std::error::Error + 'static)> = self.source.as_deref().map(|e| e as &(dyn std::error::Error + 'static));
        while let Some(cause) = current {
            if let Some(me) = cause.downcast_ref::<MediaError>() {
                if pred(me) {
                    return true;
                }
            }
            current = cause.source();
        }
        false
    }

    pub fn causal_chain(&self) -> Vec<&MediaErrorKind> {
        let mut chain = vec![&self.kind];
        let mut current: Option<&(dyn std::error::Error + 'static)> = self.source.as_deref().map(|e| e as &(dyn std::error::Error + 'static));
        while let Some(cause) = current {
            if let Some(me) = cause.downcast_ref::<MediaError>() {
                chain.push(&me.kind);
            }
            current = cause.source();
        }
        chain
    }

    pub fn error_code(&self) -> &'static str {
        match &self.kind {
            MediaErrorKind::IngestFailed { .. } => "MEDIA_INGEST_FAILED",
            MediaErrorKind::InputTooLarge { .. } => "MEDIA_TOO_LARGE",
            MediaErrorKind::UnsupportedFormat { .. } => "MEDIA_UNSUPPORTED_FORMAT",
            MediaErrorKind::CacheCorruption { .. } => "MEDIA_CACHE_CORRUPTION",
            MediaErrorKind::CacheIo { .. } => "MEDIA_CACHE_IO",
            MediaErrorKind::NoProcessorAvailable { .. } => "MEDIA_NO_PROCESSOR",
            MediaErrorKind::ConcurrencyLimitReached { .. } => "MEDIA_CONCURRENCY_LIMIT",
            MediaErrorKind::SchedulingError { .. } => "MEDIA_SCHEDULING",
            MediaErrorKind::ProcessorApiFailed { .. } => "MEDIA_PROCESSOR_API",
            MediaErrorKind::ProcessorTimeout { .. } => "MEDIA_PROCESSOR_TIMEOUT",
            MediaErrorKind::AllProcessorsFailed { .. } => "MEDIA_ALL_PROCESSORS_FAILED",
            MediaErrorKind::SelectorNoMatch { .. } => "MEDIA_SELECTOR_NO_MATCH",
            MediaErrorKind::OutputFailed { .. } => "MEDIA_OUTPUT_FAILED",
            MediaErrorKind::VadError { .. } => "MEDIA_VAD_ERROR",
            MediaErrorKind::SpeechEngineError { .. } => "MEDIA_SPEECH_ENGINE",
            MediaErrorKind::Network { .. } => "MEDIA_NETWORK",
            MediaErrorKind::Internal { .. } => "MEDIA_INTERNAL",
        }
    }
}

impl fmt::Debug for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MediaError")
            .field("kind", &self.kind)
            .field("severity", &self.severity())
            .field("retryable", &self.is_retryable())
            .field("source", &self.source.as_ref().map(|s| s.to_string()))
            .finish()
    }
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)?;
        if let Some(ref source) = self.source {
            write!(f, ": caused by: {}", source)?;
        }
        Ok(())
    }
}

impl std::error::Error for MediaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source.as_deref().map(|e| e as &(dyn std::error::Error + 'static))
    }
}

impl From<String> for MediaError {
    fn from(s: String) -> Self {
        Self::new(MediaErrorKind::Internal { detail: s })
    }
}

impl From<std::io::Error> for MediaError {
    fn from(e: std::io::Error) -> Self {
        Self::new(MediaErrorKind::CacheIo {
            detail: e.to_string(),
        })
    }
}

pub type MediaResult<T> = std::result::Result<T, MediaError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn causal_chain_propagation() {
        let root = MediaError::new(MediaErrorKind::Network {
            detail: "DNS resolution failed".into(),
        });
        let surface = MediaError::new(MediaErrorKind::ProcessorApiFailed {
            processor: "whisper".into(),
            status: Some(503), // 5xx triggers RetryableWithBackoff
            detail: "request failed".into(),
        })
        .caused_by(root);

        assert_eq!(surface.causal_chain().len(), 2);
        assert!(surface.is_retryable()); // both 503 and network are retryable
    }

    #[test]
    fn non_retryable_dominates_chain() {
        let root = MediaError::new(MediaErrorKind::UnsupportedFormat {
            mime_type: "video/x-custom".into(),
        });
        let surface = MediaError::new(MediaErrorKind::Network {
            detail: "timeout".into(),
        })
        .caused_by(root);

        // UnsupportedFormat is NonRetryable → dominates chain.
        assert!(!surface.is_retryable());
    }

    #[test]
    fn semantic_predicates() {
        let err = MediaError::new(MediaErrorKind::CacheCorruption {
            key: "abc123".into(),
            detail: "checksum mismatch".into(),
        });
        assert!(err.is_cache_error());
        assert!(!err.is_timeout());
        assert_eq!(err.severity(), Severity::Critical);
    }
}
