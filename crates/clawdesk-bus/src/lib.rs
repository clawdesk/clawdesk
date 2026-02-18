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
//! - **Crash recovery**: Events persisted to SochDB append-only log
//! - **Consumer cursors**: u64 offsets per subscriber — O(1) publish/consume

pub mod dispatch;
pub mod event;
pub mod priority;
pub mod subscription;
pub mod topic;
