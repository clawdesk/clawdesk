//! # clawdesk-browser
//!
//! Browser automation via Chrome DevTools Protocol (CDP).
//!
//! ## Architecture
//! - **CdpClient**: WebSocket-based CDP session management
//! - **BrowserAction**: High-level browser actions (navigate, click, type, screenshot)
//! - **PageContext**: DOM query, text extraction, element interaction
//! - **BrowserPool**: Connection pooling for concurrent browser sessions

pub mod action;
pub mod cdp;
pub mod page;

pub use action::{BrowserAction, ActionResult};
pub use cdp::CdpSession;
pub use page::PageContext;
