//! Chat screen — interactive conversation view with streaming support.

use super::{Screen, ScreenAction};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

/// A single message in the conversation.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: MessageRole,
    pub content: String,
    pub timestamp: String,
    pub streaming: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
    Tool,
}

/// Input mode for the chat.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputMode {
    Normal,
    Insert,
    Command,
}

pub struct ChatScreen {
    messages: Vec<ChatMessage>,
    input_buffer: String,
    cursor_pos: usize,
    input_mode: InputMode,
    scroll_offset: usize,
    session_id: Option<String>,
    model_name: String,
    is_streaming: bool,
    input_history: Vec<String>,
    history_idx: Option<usize>,
}

impl ChatScreen {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            input_buffer: String::new(),
            cursor_pos: 0,
            input_mode: InputMode::Normal,
            scroll_offset: 0,
            session_id: None,
            model_name: "default".to_string(),
            is_streaming: false,
            input_history: Vec::new(),
            history_idx: None,
        }
    }

    pub fn set_session(&mut self, session_id: String) {
        self.session_id = Some(session_id);
        self.messages.clear();
        self.scroll_offset = 0;
    }

    pub fn push_message(&mut self, msg: ChatMessage) {
        self.is_streaming = msg.streaming;
        self.messages.push(msg);
        // Auto-scroll to bottom
        self.scroll_to_bottom();
    }

    pub fn append_stream_token(&mut self, token: &str) {
        if let Some(last) = self.messages.last_mut() {
            if last.streaming {
                last.content.push_str(token);
            }
        }
    }

    pub fn finish_stream(&mut self) {
        if let Some(last) = self.messages.last_mut() {
            last.streaming = false;
        }
        self.is_streaming = false;
    }

    fn scroll_to_bottom(&mut self) {
        self.scroll_offset = self.messages.len().saturating_sub(1);
    }

    fn submit_input(&mut self) -> ScreenAction {
        let input = self.input_buffer.trim().to_string();
        if input.is_empty() {
            return ScreenAction::None;
        }

        self.input_history.push(input.clone());
        self.history_idx = None;
        self.input_buffer.clear();
        self.cursor_pos = 0;

        // Push user message locally
        self.push_message(ChatMessage {
            role: MessageRole::User,
            content: input.clone(),
            timestamp: String::new(),
            streaming: false,
        });

        ScreenAction::Command(format!("chat:send:{}", input))
    }

    fn render_messages(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .title(format!(
                " Chat {} ",
                self.session_id.as_deref().unwrap_or("(no session)")
            ))
            .borders(Borders::ALL)
            .border_style(if self.input_mode == InputMode::Normal {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            });

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let items: Vec<ListItem> = self
            .messages
            .iter()
            .skip(self.scroll_offset.saturating_sub(inner.height as usize))
            .map(|msg| {
                let (prefix, style) = match msg.role {
                    MessageRole::User => (
                        "▶ You",
                        Style::default().fg(Color::Green).add_modifier(Modifier::BOLD),
                    ),
                    MessageRole::Assistant => (
                        "◀ AI",
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                    ),
                    MessageRole::System => (
                        "⚙ Sys",
                        Style::default().fg(Color::Yellow),
                    ),
                    MessageRole::Tool => (
                        "🔧 Tool",
                        Style::default().fg(Color::Magenta),
                    ),
                };

                let cursor = if msg.streaming { "▌" } else { "" };
                let lines: Vec<Line> = std::iter::once(Line::from(vec![
                    Span::styled(format!("{}: ", prefix), style),
                ]))
                .chain(msg.content.lines().map(|l| {
                    Line::from(Span::raw(l.to_string()))
                }))
                .chain(if msg.streaming {
                    vec![Line::from(Span::styled(cursor, Style::default().fg(Color::White).add_modifier(Modifier::SLOW_BLINK)))]
                } else {
                    vec![]
                })
                .collect();

                ListItem::new(Text::from(lines))
            })
            .collect();

        let list = List::new(items);
        frame.render_widget(list, inner);
    }

    fn render_input(&self, frame: &mut Frame, area: Rect) {
        let mode_indicator = match self.input_mode {
            InputMode::Normal => ("NORMAL", Color::Blue),
            InputMode::Insert => ("INSERT", Color::Green),
            InputMode::Command => ("CMD", Color::Yellow),
        };

        let block = Block::default()
            .title(format!(" {} ", mode_indicator.0))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(mode_indicator.1));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let prompt = if self.is_streaming {
            "⏳ streaming..."
        } else {
            ">"
        };

        let input_text = Line::from(vec![
            Span::styled(
                format!("{} ", prompt),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(&self.input_buffer),
            Span::styled("▎", Style::default().fg(Color::White).add_modifier(Modifier::SLOW_BLINK)),
        ]);

        let input_para = Paragraph::new(input_text).wrap(Wrap { trim: false });
        frame.render_widget(input_para, inner);
    }

    fn render_status_bar(&self, frame: &mut Frame, area: Rect) {
        let status = Line::from(vec![
            Span::styled(
                format!(" {} ", self.model_name),
                Style::default().fg(Color::Black).bg(Color::Cyan),
            ),
            Span::styled(
                format!(" {} msgs ", self.messages.len()),
                Style::default().fg(Color::DarkGray),
            ),
            if self.is_streaming {
                Span::styled(" ● streaming ", Style::default().fg(Color::Green))
            } else {
                Span::styled(" ○ idle ", Style::default().fg(Color::DarkGray))
            },
        ]);

        frame.render_widget(Paragraph::new(status), area);
    }
}

impl Screen for ChatScreen {
    fn name(&self) -> &str {
        "Chat"
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(5),    // messages
                Constraint::Length(3), // input
                Constraint::Length(1), // status bar
            ])
            .split(area);

        self.render_messages(frame, chunks[0]);
        self.render_input(frame, chunks[1]);
        self.render_status_bar(frame, chunks[2]);
    }

    fn handle_key(&mut self, key: KeyEvent) -> ScreenAction {
        match self.input_mode {
            InputMode::Normal => match key.code {
                KeyCode::Char('i') => {
                    self.input_mode = InputMode::Insert;
                    ScreenAction::None
                }
                KeyCode::Char('a') => {
                    self.input_mode = InputMode::Insert;
                    self.cursor_pos = self.input_buffer.len();
                    ScreenAction::None
                }
                KeyCode::Char(':') => {
                    self.input_mode = InputMode::Command;
                    ScreenAction::None
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    self.scroll_offset = self.scroll_offset.saturating_add(1).min(self.messages.len());
                    ScreenAction::None
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.scroll_offset = self.scroll_offset.saturating_sub(1);
                    ScreenAction::None
                }
                KeyCode::Char('G') => {
                    self.scroll_to_bottom();
                    ScreenAction::None
                }
                KeyCode::Char('g') => {
                    self.scroll_offset = 0;
                    ScreenAction::None
                }
                KeyCode::Char('q') => ScreenAction::Quit,
                _ => ScreenAction::None,
            },
            InputMode::Insert => match key.code {
                KeyCode::Esc => {
                    self.input_mode = InputMode::Normal;
                    ScreenAction::None
                }
                KeyCode::Enter => self.submit_input(),
                KeyCode::Backspace => {
                    if self.cursor_pos > 0 {
                        self.cursor_pos -= 1;
                        self.input_buffer.remove(self.cursor_pos);
                    }
                    ScreenAction::None
                }
                KeyCode::Delete => {
                    if self.cursor_pos < self.input_buffer.len() {
                        self.input_buffer.remove(self.cursor_pos);
                    }
                    ScreenAction::None
                }
                KeyCode::Left => {
                    self.cursor_pos = self.cursor_pos.saturating_sub(1);
                    ScreenAction::None
                }
                KeyCode::Right => {
                    self.cursor_pos = (self.cursor_pos + 1).min(self.input_buffer.len());
                    ScreenAction::None
                }
                KeyCode::Home => {
                    self.cursor_pos = 0;
                    ScreenAction::None
                }
                KeyCode::End => {
                    self.cursor_pos = self.input_buffer.len();
                    ScreenAction::None
                }
                KeyCode::Up => {
                    // History navigation
                    if !self.input_history.is_empty() {
                        let idx = match self.history_idx {
                            Some(i) => i.saturating_sub(1),
                            None => self.input_history.len() - 1,
                        };
                        self.history_idx = Some(idx);
                        self.input_buffer = self.input_history[idx].clone();
                        self.cursor_pos = self.input_buffer.len();
                    }
                    ScreenAction::None
                }
                KeyCode::Down => {
                    if let Some(idx) = self.history_idx {
                        if idx + 1 < self.input_history.len() {
                            self.history_idx = Some(idx + 1);
                            self.input_buffer = self.input_history[idx + 1].clone();
                        } else {
                            self.history_idx = None;
                            self.input_buffer.clear();
                        }
                        self.cursor_pos = self.input_buffer.len();
                    }
                    ScreenAction::None
                }
                KeyCode::Char(ch) => {
                    self.input_buffer.insert(self.cursor_pos, ch);
                    self.cursor_pos += 1;
                    ScreenAction::None
                }
                _ => ScreenAction::None,
            },
            InputMode::Command => match key.code {
                KeyCode::Esc => {
                    self.input_mode = InputMode::Normal;
                    self.input_buffer.clear();
                    self.cursor_pos = 0;
                    ScreenAction::None
                }
                KeyCode::Enter => {
                    let cmd = self.input_buffer.trim().to_string();
                    self.input_buffer.clear();
                    self.cursor_pos = 0;
                    self.input_mode = InputMode::Normal;
                    ScreenAction::Command(cmd)
                }
                KeyCode::Char(ch) => {
                    self.input_buffer.insert(self.cursor_pos, ch);
                    self.cursor_pos += 1;
                    ScreenAction::None
                }
                KeyCode::Backspace => {
                    if self.cursor_pos > 0 {
                        self.cursor_pos -= 1;
                        self.input_buffer.remove(self.cursor_pos);
                    }
                    ScreenAction::None
                }
                _ => ScreenAction::None,
            },
        }
    }

    fn handle_backend_event(&mut self, event: &crate::event::BackendEvent) {
        match event {
            crate::event::BackendEvent::StreamToken { token, .. } => {
                self.append_stream_token(token);
            }
            _ => {}
        }
    }

    fn on_enter(&mut self) {
        self.input_mode = InputMode::Insert;
    }
}
