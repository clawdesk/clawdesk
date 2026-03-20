//! # clawdesk-bus
//!
//! Reactive dataflow bus — event-sourced pipeline substrate.
//!
//! Provides a push-based event bus where producers emit typed events and
//! pipelines subscribe to event patterns. Implements weighted fair queuing
//! (WFQ) across priority classes with per-topic ring buffers.
//!
//! ## Architecture
//!
//! - **Topics**: Named event streams with configurable capacity
//! - **Priority classes**: Urgent (w=8), Standard (w=4), Batch (w=1)
//! - **Backpressure**: Bounded mpsc channels; full channels yield cooperatively
//! - **Persistence**: Not yet implemented — all events are in-memory only.
//!   A future version may persist events to SochDB for crash recovery.
//! - **Consumer cursors**: u64 offsets per subscriber — O(1) publish/consume

pub mod adaptive_priority;
pub mod backpressure;
pub mod bridge;
pub mod config_events;
pub mod dispatch;
pub mod event;
pub mod inbound;
pub mod orchestrator_bridge;
pub mod priority;
pub mod skill_events;
pub mod subscription;
pub mod surface;
pub mod topic;
pub mod typed_event;
pub mod workflow;
