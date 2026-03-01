//! # clawdesk-browser
//!
//! Browser automation via Chrome DevTools Protocol (CDP)
//! with DOM Intelligence for LLM-native interaction.
//!
//! ## Architecture
//! - **CdpSession**: WebSocket-based CDP session management
//! - **DomIntelligence**: Element indexing, DOM distillation, a11y tree
//! - **BrowserManager**: Per-agent session pool with idle reaper
//! - **SSRF**: URL validation against server-side request forgery
//! - **Safety**: Content wrapping, purchase detection
//! - **BrowserAction**: High-level browser actions (navigate, click, type, screenshot)
//! - **PageContext**: DOM query, text extraction, element interaction

pub mod action;
pub mod cdp;
pub mod dom_intel;
pub mod manager;
pub mod page;
pub mod safety;
pub mod ssrf;

pub use action::{BrowserAction, ActionResult, execute_action, execute_tool_call};
pub use action::{action_tool_definitions, parse_tool_call, ActionData, ElementTarget, ToolDef};
pub use cdp::CdpSession;
pub use dom_intel::DomSnapshot;
pub use manager::BrowserManager;
pub use page::PageContext;
