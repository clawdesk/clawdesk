//! # clawdesk-autoreply
//!
//! Auto-reply engine — the full message processing pipeline from inbound
//! message to delivered reply.
//!
//! ## Pipeline Stages
//! ```text
//! Inbound → Classify → Route → Enrich → Execute → Format → Deliver
//! ```
//!
//! - **Classify**: Determine trigger type (mention, DM, command, scheduled)
//! - **Route**: Check allowlists, select agent, apply send policy
//! - **Enrich**: Add context (memories, system prompt, channel metadata)
//! - **Execute**: Run agent pipeline (LLM + tools)
//! - **Format**: Adapt response for target channel constraints
//! - **Deliver**: Send via channel, track delivery status

pub mod block_stream;
pub mod chunking;
pub mod classifier;
pub mod command_auth;
pub mod command_registry;
pub mod commands;
pub mod debounce;
pub mod directive;
pub mod echo;
pub mod formatter;
pub mod media_directive;
pub mod pipeline;
pub mod router;

pub use block_stream::{Block, BlockCoalescer, CoalescedDelivery, TypingHeartbeat};
pub use classifier::TriggerClassifier;
pub use command_registry::{CommandRegistry, CommandDef, ParsedCommand, CommandResult, Command, CommandContext};
pub use directive::{Directives, ThinkLevel, parse_directives, merge_directives};
pub use echo::{EchoSuppressor, EchoSuppressionConfig, SuppressionReason};
pub use formatter::ResponseFormatter;
pub use media_directive::{MediaSplit, parse_media_directives, media_urls_to_attachments};
pub use pipeline::ReplyPipeline;
pub use router::MessageRouter;
