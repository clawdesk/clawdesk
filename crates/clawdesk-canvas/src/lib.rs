//! # clawdesk-canvas
//!
//! Canvas host + A2UI (Agent-to-UI) protocol for ClawDesk.
//!
//! Provides:
//! - **Canvas Host**: HTTP server serving agent-generated web content
//! - **A2UI Protocol**: JSONL-based declarative UI pushed from agents to a WebView
//! - **Capability Tokens**: Time-limited auth tokens scoped to canvas URLs
//! - **Canvas Commands**: present, hide, navigate, eval, snapshot, a2ui_push, a2ui_reset
//!
//! Architecture:
//! ```text
//! ┌──────────────┐     ┌─────────────────┐     ┌──────────────┐
//! │  Agent Loop  │────▶│  Canvas Manager  │────▶│  Tauri       │
//! │  (tools)     │     │  (commands)      │     │  WebView     │
//! └──────────────┘     └─────────────────┘     └──────────────┘
//!                            │
//!                      ┌─────────────┐
//!                      │ Canvas Host │  HTTP :0 (dynamic port)
//!                      │ (static +   │  serves ~/.clawdesk/canvas/
//!                      │  A2UI SPA)  │  + /__clawdesk__/a2ui/
//!                      └─────────────┘
//! ```

pub mod a2ui;
pub mod capability;
pub mod commands;
pub mod device;
pub mod host;
pub mod node_commands;
pub mod node_coordinator;

// Re-exports
pub use a2ui::{A2UIComponent, A2UIMessage, ComponentTree, SurfaceUpdate};
pub use capability::{CanvasCapability, CapabilityStore};
pub use commands::{CanvasBackend, CanvasCommand, CanvasCommandResult, CanvasManager, CanvasPlacement, CanvasSnapshot, CanvasState};
pub use device::{CameraCapture, CameraInfo, DeviceCapabilities, DeviceInfo, DeviceManager, DeviceStatus, LocationData, LocationProvider, LocationSource, MediaProvider, ScreenRecording};
pub use host::CanvasHostServer;
pub use node_commands::{NodeCommandManager, NodeDeviceCommand, NodeCommandResult};
pub use node_coordinator::{NodeCoordinator, NodeRole, NodeCapability};
