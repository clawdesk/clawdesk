//! # clawdesk-tui
//!
//! Terminal UI for ClawDesk — interactive chat, model picker, and system status.
//!
//! ## Architecture
//! - **Screens**: Tab-routable Screen trait with 10 built-in screens
//! - **Event**: Multiplexed event system (terminal + backend + tick)
//! - **ChatView**: Main chat interface with message history, input, and streaming
//! - **StatusBar**: Provider health, active model, token usage
//! - **ModelPicker**: Interactive model selection with capability display
//! - **Layout**: Responsive terminal layout engine
//! - **Theme**: Configurable color schemes with 4 presets

pub mod app;
pub mod btw_overlay;
pub mod chat;
pub mod event;
pub mod layout;
pub mod screens;
pub mod status;
pub mod theme;

pub use app::App;
pub use btw_overlay::BtwInlineMessage;
pub use chat::ChatView;
pub use event::{AppEvent, BackendEvent, EventHandler};
pub use layout::AppLayout;
pub use screens::{Phase, Router, Screen, ScreenAction, Tab};
pub use status::StatusBar;
pub use theme::Theme;
