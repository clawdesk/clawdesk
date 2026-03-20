//! Screen trait and Tab system for the TUI framework.
//!
//! Each screen implements `Screen` and is routable via the `Tab` enum.
//! The router dispatches render and event handling to the active screen.

pub mod agents;
pub mod channels;
pub mod chat;
pub mod dashboard;
pub mod health;
pub mod logs;
pub mod memory;
pub mod resources;
pub mod security;
pub mod sessions;
pub mod settings;
pub mod skills;

use crate::event::AppEvent;
use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;
use std::fmt;

/// Application-level phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Boot splash / initialization
    Boot,
    /// Main application (tab navigation active)
    Main,
}

/// Tabs in the main phase — addressable screens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Dashboard,
    Chat,
    Sessions,
    Agents,
    Channels,
    Memory,
    Skills,
    Settings,
    Logs,
    Security,
    Health,
    Resources,
}

impl Tab {
    /// All tabs in display order.
    pub const ALL: &'static [Tab] = &[
        Tab::Dashboard,
        Tab::Chat,
        Tab::Sessions,
        Tab::Agents,
        Tab::Channels,
        Tab::Memory,
        Tab::Skills,
        Tab::Settings,
        Tab::Logs,
        Tab::Security,
        Tab::Health,
        Tab::Resources,
    ];

    /// One-char shortcut for each tab.
    pub fn shortcut(&self) -> char {
        match self {
            Tab::Dashboard => 'd',
            Tab::Chat => 'c',
            Tab::Sessions => 's',
            Tab::Agents => 'a',
            Tab::Channels => 'h',
            Tab::Memory => 'm',
            Tab::Skills => 'k',
            Tab::Settings => ',',
            Tab::Logs => 'l',
            Tab::Security => 'x',
            Tab::Health => 'z',
            Tab::Resources => 'r',
        }
    }

    /// Tab from shortcut key.
    pub fn from_shortcut(ch: char) -> Option<Tab> {
        Tab::ALL.iter().find(|t| t.shortcut() == ch).copied()
    }

    /// Next tab (wrapping).
    pub fn next(self) -> Tab {
        let idx = Tab::ALL.iter().position(|t| *t == self).unwrap_or(0);
        Tab::ALL[(idx + 1) % Tab::ALL.len()]
    }

    /// Previous tab (wrapping).
    pub fn prev(self) -> Tab {
        let idx = Tab::ALL.iter().position(|t| *t == self).unwrap_or(0);
        if idx == 0 {
            Tab::ALL[Tab::ALL.len() - 1]
        } else {
            Tab::ALL[idx - 1]
        }
    }
}

impl fmt::Display for Tab {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tab::Dashboard => write!(f, "Dashboard"),
            Tab::Chat => write!(f, "Chat"),
            Tab::Sessions => write!(f, "Sessions"),
            Tab::Agents => write!(f, "Agents"),
            Tab::Channels => write!(f, "Channels"),
            Tab::Memory => write!(f, "Memory"),
            Tab::Skills => write!(f, "Skills"),
            Tab::Settings => write!(f, "Settings"),
            Tab::Logs => write!(f, "Logs"),
            Tab::Security => write!(f, "Security"),
            Tab::Health => write!(f, "Health"),
            Tab::Resources => write!(f, "Resources"),
        }
    }
}

/// Action returned by a screen after handling an event.
#[derive(Debug, Clone)]
pub enum ScreenAction {
    /// No action needed.
    None,
    /// Switch to another tab.
    SwitchTab(Tab),
    /// Request application quit.
    Quit,
    /// Show a notification popup.
    Notify { title: String, body: String },
    /// Dispatch a command string (e.g., for the command palette).
    Command(String),
}

/// The Screen trait — all TUI screens implement this.
pub trait Screen {
    /// Human-readable screen name.
    fn name(&self) -> &str;

    /// Render the screen into the given area.
    fn render(&self, frame: &mut Frame, area: Rect);

    /// Handle a key event. Return an action to propagate.
    fn handle_key(&mut self, key: KeyEvent) -> ScreenAction;

    /// Handle a tick event (animations, polling updates).
    fn on_tick(&mut self) {}

    /// Handle a backend event.
    fn handle_backend_event(&mut self, _event: &crate::event::BackendEvent) {}

    /// Called when this screen becomes active.
    fn on_enter(&mut self) {}

    /// Called when this screen becomes inactive.
    fn on_leave(&mut self) {}
}

/// Screen router — dispatches to the active screen.
pub struct Router {
    pub phase: Phase,
    pub active_tab: Tab,
    pub dashboard: dashboard::DashboardScreen,
    pub chat: chat::ChatScreen,
    pub sessions: sessions::SessionsScreen,
    pub agents: agents::AgentsScreen,
    pub channels: channels::ChannelsScreen,
    pub memory: memory::MemoryScreen,
    pub skills: skills::SkillsScreen,
    pub settings: settings::SettingsScreen,
    pub logs: logs::LogsScreen,
    pub security: security::SecurityScreen,
    pub health: health::HealthDashboardScreen,
    pub resources: resources::ResourceMonitorScreen,
}

impl Router {
    pub fn new() -> Self {
        Self {
            phase: Phase::Boot,
            active_tab: Tab::Dashboard,
            dashboard: dashboard::DashboardScreen::new(),
            chat: chat::ChatScreen::new(),
            sessions: sessions::SessionsScreen::new(),
            agents: agents::AgentsScreen::new(),
            channels: channels::ChannelsScreen::new(),
            memory: memory::MemoryScreen::new(),
            skills: skills::SkillsScreen::new(),
            settings: settings::SettingsScreen::new(),
            logs: logs::LogsScreen::new(),
            security: security::SecurityScreen::new(),
            health: health::HealthDashboardScreen::new(),
            resources: resources::ResourceMonitorScreen::new(),
        }
    }

    /// Get a mutable reference to the active screen.
    pub fn active_screen_mut(&mut self) -> &mut dyn Screen {
        match self.active_tab {
            Tab::Dashboard => &mut self.dashboard,
            Tab::Chat => &mut self.chat,
            Tab::Sessions => &mut self.sessions,
            Tab::Agents => &mut self.agents,
            Tab::Channels => &mut self.channels,
            Tab::Memory => &mut self.memory,
            Tab::Skills => &mut self.skills,
            Tab::Settings => &mut self.settings,
            Tab::Logs => &mut self.logs,
            Tab::Security => &mut self.security,
            Tab::Health => &mut self.health,
            Tab::Resources => &mut self.resources,
        }
    }

    /// Get an immutable reference to the active screen.
    pub fn active_screen(&self) -> &dyn Screen {
        match self.active_tab {
            Tab::Dashboard => &self.dashboard,
            Tab::Chat => &self.chat,
            Tab::Sessions => &self.sessions,
            Tab::Agents => &self.agents,
            Tab::Channels => &self.channels,
            Tab::Memory => &self.memory,
            Tab::Skills => &self.skills,
            Tab::Settings => &self.settings,
            Tab::Logs => &self.logs,
            Tab::Security => &self.security,
            Tab::Health => &self.health,
            Tab::Resources => &self.resources,
        }
    }

    /// Switch to a new tab, calling on_leave / on_enter hooks.
    pub fn switch_to(&mut self, tab: Tab) {
        if self.active_tab != tab {
            self.active_screen_mut().on_leave();
            self.active_tab = tab;
            self.active_screen_mut().on_enter();
        }
    }
}
