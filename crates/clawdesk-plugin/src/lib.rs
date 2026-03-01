//! # clawdesk-plugin
//!
//! Plugin system for ClawDesk — lifecycle management, dependency resolution,
//! capability enforcement, and hot-reload.
//!
//! ## Architecture
//! - **PluginHost**: Manages plugin lifecycle (discover → load → resolve → activate)
//! - **DependencyResolver**: Topological sort for dependency ordering
//! - **CapabilityValidator**: Validates plugin capability requests against grants
//! - **PluginSandbox**: Resource limits and isolation enforcement
//!
//! Plugins communicate with the host via async trait objects. The host enforces
//! capability grants, resource limits, and lifecycle state transitions.

pub mod hooks;
pub mod host;
pub mod resolver;
pub mod sandbox;
pub mod registry;
pub mod sdk;

pub use host::{PluginFactory, PluginHandle, PluginHost, PluginInstance};
pub use resolver::DependencyResolver;
pub use sandbox::PluginSandbox;
pub use hooks::{Hook, HookContext, HookManager, HookOverrides, HookResult, Phase, ProactiveMemoryHook};
pub use registry::PluginRegistry;
pub use sdk::{ClawDeskPlugin, PluginContext, PluginEvent, PluginManifest, PluginResponse, PluginSdkError};
