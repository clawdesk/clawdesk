//! Integration test harness for ClawDesk.
//!
//! Provides mock adapters, test fixtures, and utilities for end-to-end
//! testing of the ClawDesk pipeline: inbound message → agent processing →
//! outbound response.

pub mod fixtures;
pub mod mock_channel;
pub mod mock_provider;
pub mod helpers;
