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

pub mod classifier;
pub mod echo;
pub mod formatter;
pub mod pipeline;
pub mod router;

pub use classifier::TriggerClassifier;
pub use echo::{EchoSuppressor, EchoSuppressionConfig, SuppressionReason};
pub use formatter::ResponseFormatter;
pub use pipeline::ReplyPipeline;
pub use router::MessageRouter;
