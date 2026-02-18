//! TUI application — event loop, rendering, and keybindings.
//!
//! Uses `ratatui` + `crossterm` for terminal rendering with double-buffering.
//! Supports Vim-like keybindings in normal mode, free-form input in insert mode.
//!
//! ## Architecture
//! - `App` holds all state: chat view, layout, status bar, active session
//! - `InputMode`: Normal (vim keys) vs Insert (typing)
//! - `run()` enters the main event loop at 30fps with non-blocking input polling
//! - Rendering uses ratatui's `Widget` system for composable UI elements

use crate::chat::{ChatMessage, ChatView, Role};
use crate::layout::LayoutMode;
use crate::status::StatusBar;
use crate::theme::Theme;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use std::io::{self, stdout};
use std::time::Duration;

/// Input mode determines how keystrokes are interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    /// Vim-like navigation: j/k scroll, / search, i insert, q quit.
    Normal,
    /// Free-form text input for composing messages.
    Insert,
}

/// Session tab — minimal multiplexing state.
pub struct Session {
    pub id: usize,
    pub name: String,
    pub chat: ChatView,
}

impl Session {
    pub fn new(id: usize, name: impl Into<String>) -> Self {
        Self {
            id,
            name: name.into(),
            chat: ChatView::new(),
        }
    }
}

/// Main TUI application state.
pub struct App {
    /// All sessions (Ctrl+1..9 to switch).
    pub sessions: Vec<Session>,
    /// Index of the active session.
    pub active_session: usize,
    /// Current input mode.
    pub mode: InputMode,
    /// Status bar data.
    pub status: StatusBar,
    /// Active theme.
    pub theme: Theme,
    /// Whether the app should quit.
    pub should_quit: bool,
    /// Current layout mode.
    pub layout_mode: LayoutMode,
}

impl App {
    /// Create a new App with one default session.
    pub fn new() -> Self {
        let mut sessions = Vec::new();
        sessions.push(Session::new(0, "General"));

        Self {
            sessions,
            active_session: 0,
            mode: InputMode::Normal,
            status: StatusBar::new(),
            theme: Theme::dark(),
            should_quit: false,
            layout_mode: LayoutMode::Chat,
        }
    }

    /// Get the active session mutably.
    pub fn active_chat(&mut self) -> &mut ChatView {
        &mut self.sessions[self.active_session].chat
    }

    /// Add a new session and switch to it.
    pub fn new_session(&mut self, name: impl Into<String>) {
        let id = self.sessions.len();
        self.sessions.push(Session::new(id, name));
        self.active_session = id;
    }

    /// Switch to session by index (0-based).
    pub fn switch_session(&mut self, index: usize) {
        if index < self.sessions.len() {
            self.active_session = index;
        }
    }

    /// Handle a key event.
    pub fn handle_key(&mut self, key: KeyEvent) {
        match self.mode {
            InputMode::Normal => self.handle_normal_key(key),
            InputMode::Insert => self.handle_insert_key(key),
        }
    }

    fn handle_normal_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Char('i') => self.mode = InputMode::Insert,
            KeyCode::Char('j') => self.active_chat().scroll_down(1),
            KeyCode::Char('k') => self.active_chat().scroll_up(1),
            KeyCode::Char('G') => {
                self.active_chat().scroll_to_bottom();
            }
            KeyCode::Char('g') => {
                // Scroll to top (oldest messages)
                let chat = self.active_chat();
                let count = chat.message_count();
                chat.scroll_up(count);
            }
            KeyCode::Char(c @ '1'..='9') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let idx = (c as usize) - ('1' as usize);
                self.switch_session(idx);
            }
            KeyCode::Tab => {
                let next = (self.active_session + 1) % self.sessions.len();
                self.switch_session(next);
            }
            _ => {}
        }
    }

    fn handle_insert_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.mode = InputMode::Normal,
            KeyCode::Enter => {
                let chat = self.active_chat();
                let text = chat.take_input();
                if !text.is_empty() {
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let hours = (now / 3600) % 24;
                    let mins = (now / 60) % 60;
                    let ts = format!("{:02}:{:02}", hours, mins);
                    chat.push_message(ChatMessage {
                        role: Role::User,
                        content: text,
                        timestamp: ts,
                        model: None,
                        tokens: None,
                        streaming: false,
                    });
                    // In a real app, this would send to the gateway and start streaming
                }
            }
            KeyCode::Backspace => self.active_chat().backspace(),
            KeyCode::Left => self.active_chat().cursor_left(),
            KeyCode::Right => self.active_chat().cursor_right(),
            KeyCode::Char(c) => self.active_chat().insert_char(c),
            _ => {}
        }
    }

    /// Run the TUI event loop. This takes ownership of the terminal.
    pub fn run(&mut self) -> io::Result<()> {
        // Enter raw mode and alternate screen
        terminal::enable_raw_mode()?;
        stdout().execute(EnterAlternateScreen)?;
        let mut terminal = Terminal::new(CrosstermBackend::new(stdout()))?;

        let tick_rate = Duration::from_millis(33); // ~30fps

        while !self.should_quit {
            // Render
            terminal.draw(|frame| self.render(frame))?;

            // Poll for input
            if event::poll(tick_rate)? {
                if let Event::Key(key) = event::read()? {
                    // Ctrl+C always quits
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        self.should_quit = true;
                    } else {
                        self.handle_key(key);
                    }
                }
            }
        }

        // Restore terminal
        terminal::disable_raw_mode()?;
        stdout().execute(LeaveAlternateScreen)?;
        Ok(())
    }

    /// Render the entire UI for one frame.
    fn render(&self, frame: &mut Frame) {
        let area = frame.area();

        // Main layout: chat area + status bar (1 line)
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .split(area);

        let main_area = chunks[0];
        let status_area = chunks[1];

        // Determine if we show sidebar
        let show_sidebar = area.width >= 80;

        if show_sidebar && self.sessions.len() > 1 {
            let h_chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(20), Constraint::Min(40)])
                .split(main_area);

            self.render_session_list(frame, h_chunks[0]);
            self.render_chat(frame, h_chunks[1]);
        } else {
            self.render_chat(frame, main_area);
        }

        self.render_status_bar(frame, status_area);
    }

    fn render_session_list(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let items: Vec<ListItem> = self
            .sessions
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let marker = if i == self.active_session { "▸ " } else { "  " };
                ListItem::new(format!("{}{}", marker, s.name))
            })
            .collect();

        let list = List::new(items)
            .block(Block::default().borders(Borders::RIGHT).title("Sessions"));
        frame.render_widget(list, area);
    }

    fn render_chat(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let session = &self.sessions[self.active_session];
        let chat = &session.chat;

        // Split: messages area + input area
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(3)])
            .split(area);

        // Build message display from chat messages
        let msg_height = chunks[0].height.saturating_sub(2) as usize; // borders
        let messages: Vec<ListItem> = chat
            .visible_messages(msg_height)
            .map(|msg| {
                let prefix = format!("[{}] {}: ", msg.timestamp, msg.role.label());
                let suffix = if msg.streaming { " ▌" } else { "" };
                ListItem::new(format!("{}{}{}", prefix, msg.content, suffix))
            })
            .collect();

        let msg_list = List::new(messages)
            .block(Block::default().borders(Borders::ALL).title(format!(
                " {} ",
                session.name
            )));
        frame.render_widget(msg_list, chunks[0]);

        // Input box
        let mode_indicator = match self.mode {
            InputMode::Normal => "[NORMAL]",
            InputMode::Insert => "[INSERT]",
        };
        let input_text = chat.input();
        let input = Paragraph::new(format!("{} {}", mode_indicator, input_text))
            .block(Block::default().borders(Borders::ALL).title(" Input "))
            .wrap(Wrap { trim: false });
        frame.render_widget(input, chunks[1]);

        // Set cursor position in insert mode
        if self.mode == InputMode::Insert {
            let cursor_x = chunks[1].x + 1 + mode_indicator.len() as u16 + 1 + chat.cursor_position() as u16;
            let cursor_y = chunks[1].y + 1;
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }

    fn render_status_bar(&self, frame: &mut Frame, area: ratatui::layout::Rect) {
        let status_text = self.status.format_line(area.width as usize);
        let mode_text = match self.mode {
            InputMode::Normal => " NORMAL ",
            InputMode::Insert => " INSERT ",
        };
        let session_info = format!(
            " [{}/{}] ",
            self.active_session + 1,
            self.sessions.len()
        );

        let text = format!("{}{}{}", mode_text, status_text, session_info);
        let status = Paragraph::new(text)
            .style(Style::default().bg(Color::DarkGray).fg(Color::White));
        frame.render_widget(status, area);
    }
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_starts_in_normal_mode() {
        let app = App::new();
        assert_eq!(app.mode, InputMode::Normal);
        assert_eq!(app.sessions.len(), 1);
        assert!(!app.should_quit);
    }

    #[test]
    fn switch_to_insert_mode() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        assert_eq!(app.mode, InputMode::Insert);
    }

    #[test]
    fn quit_on_q() {
        let mut app = App::new();
        app.handle_key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE));
        assert!(app.should_quit);
    }

    #[test]
    fn typing_in_insert_mode() {
        let mut app = App::new();
        app.mode = InputMode::Insert;
        app.handle_key(KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE));
        app.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        assert_eq!(app.active_chat().input(), "hi");
    }

    #[test]
    fn escape_returns_to_normal() {
        let mut app = App::new();
        app.mode = InputMode::Insert;
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.mode, InputMode::Normal);
    }

    #[test]
    fn new_session_and_switch() {
        let mut app = App::new();
        app.new_session("Test");
        assert_eq!(app.sessions.len(), 2);
        assert_eq!(app.active_session, 1);
        app.switch_session(0);
        assert_eq!(app.active_session, 0);
    }

    #[test]
    fn tab_cycles_sessions() {
        let mut app = App::new();
        app.new_session("Second");
        app.switch_session(0);
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.active_session, 1);
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.active_session, 0);
    }
}
