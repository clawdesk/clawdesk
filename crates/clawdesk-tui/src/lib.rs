//! # clawdesk-tui
//!
//! Terminal UI for ClawDesk — interactive chat, model picker, and system status.
//!
//! ## Architecture
//! - **ChatView**: Main chat interface with message history, input, and streaming
//! - **StatusBar**: Provider health, active model, token usage
//! - **ModelPicker**: Interactive model selection with capability display
//! - **Layout**: Responsive terminal layout engine
//! - **Theme**: Configurable color schemes

pub mod app;
pub mod chat;
pub mod layout;
pub mod status;
pub mod theme;

pub use app::App;
pub use chat::ChatView;
pub use layout::AppLayout;
pub use status::StatusBar;
pub use theme::Theme;
