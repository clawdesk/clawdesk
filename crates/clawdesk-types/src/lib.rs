//! # clawdesk-types
//!
//! Zero-dependency core types for the ClawDesk multi-channel AI agent gateway.
//!
//! This crate defines the foundational type algebra covering all 14 categories:
//! - **Message types**: Sum-type envelope (`InboundMessage`) with per-channel variants
//! - **Normalized message**: Common form for downstream processing
//! - **Config types**: Algebraic product type for configuration
//! - **Error types**: Closed union error hierarchy with exhaustive matching
//! - **Session types**: Session key, state, and lifecycle
//! - **Channel types**: Channel identifiers and metadata
//! - **Plugin types**: Plugin manifest, lifecycle, capabilities
//! - **Media types**: Audio, video, image understanding
//! - **Cron types**: Scheduled task definitions and run logs
//! - **Security types**: Audit entries, scan results, ACLs
//! - **Protocol types**: Canonical gateway message format
//! - **Auto-reply types**: Trigger classification, send policy

pub mod artifact;
pub mod autoreply;
pub mod channel;
pub mod dirs;
pub mod config;
pub mod cron;
pub mod error;
pub mod failover;
pub mod isolation;
pub mod media;
pub mod message;
pub mod ordered_lock;
pub mod plugin;
pub mod protocol;
pub mod reactions;
pub mod ring;
pub mod security;
pub mod session;
pub mod taint;
pub mod token_usage;
pub mod tokenizer;
pub mod error_ext;

// Re-export key types at crate root
pub use error_ext::{RuntimeError, McpProtocolError, ErrorSeverity, ClassifiableError};
pub use channel::{ChannelId, ChannelMeta};
pub use config::{ClawDeskConfig, ValidatedConfig};
pub use error::{ClawDeskError, ProviderError, ProviderErrorKind};
pub use ring::DropOldest;
pub use tokenizer::estimate_tokens;
pub use tokenizer::truncate_to_char_boundary;
pub use token_usage::TokenUsage;
pub use isolation::IsolationLevel;
pub use message::{
    InboundMessage, MediaAttachment, MessageOrigin, NormalizedMessage, OutboundMessage,
    ReplyContext, SenderIdentity,
};
pub use reactions::{Reaction, ReactionEvent, RichContent, CardContent, PollContent, QuickReplySet, ButtonContent};
pub use session::{Session, SessionConfig, SessionKey, SessionSummary};
pub use artifact::{ArtifactRef, ArtifactData, ArtifactId, ArtifactIndex};
