//! # clawdesk-channel-plugins
//!
//! Dynamic channel plugin architecture with self-registration, per-channel
//! capability matrices, hierarchical allowlists, and action dispatch.
//!
//! ## Capability Matrix
//! Each channel declares its capabilities as a bitvector for O(1) intersection.

pub mod capability;
pub mod plugin;
pub mod allowlist;

pub use capability::{ChannelCapability, CapabilitySet};
pub use plugin::{ChannelPlugin, PluginManifest, PluginRegistry};
pub use allowlist::{AllowlistLevel, HierarchicalAllowlist, AllowDecision};
