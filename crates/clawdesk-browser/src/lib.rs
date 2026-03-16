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
//! - **Profile**: Persistent browser profiles for session continuity
//! - **Tabs**: Multi-tab management via CDP Target domain
//! - **Snapshot**: Enhanced ARIA/AI snapshots with ref-based targeting
//! - **FileOps**: File upload/download, console capture

pub mod action;
pub mod cdp;
pub mod dom_intel;
pub mod extension_relay;
pub mod file_ops;
pub mod headless;
pub mod manager;
pub mod page;
pub mod profile;
pub mod route_dispatcher;
pub mod safety;
pub mod session_registry;
pub mod snapshot;
pub mod ssrf;
pub mod tabs;
pub mod tool_registry;

pub use action::{BrowserAction, ActionResult, execute_action, execute_tool_call};
pub use action::{action_tool_definitions, parse_tool_call, ActionData, ElementTarget, ToolDef};
pub use cdp::CdpSession;
pub use dom_intel::DomSnapshot;
pub use file_ops::{upload_file, enable_downloads, drain_console_buffer, inject_console_shim};
pub use manager::{BrowserManager, ConsoleEntry};
pub use page::PageContext;
pub use profile::{BrowserProfile, ProfileManager};
pub use snapshot::{EnhancedSnapshot, SnapshotConfig, SnapshotMode, aria_snapshot, ai_snapshot};
pub use tabs::{TabInfo, list_tabs, open_tab, focus_tab, close_tab, format_tabs_for_llm};
pub use tool_registry::{BrowserToolId, resolve_alias, is_deprecated_alias};
pub use extension_relay::{ExtensionRelay, RelayConfig, RelayState, RelaySession};
pub use session_registry::{SessionTabRegistry, ProfileId, TargetId, TabState, TabEntry};
pub use route_dispatcher::{AgentRoute, RouteRequest, RouteResponse, NavigationGuard};
pub use headless::{RemoteCdpConfig, PageAction, ActionBatch, ActionOutcome, PageState, ActivityTracker, DownloadCapture, rewrite_cdp_url};
