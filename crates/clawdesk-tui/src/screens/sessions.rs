//! Sessions screen — list, search, and manage conversation sessions.

use super::{Screen, ScreenAction};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub id: String,
    pub title: String,
    pub model: String,
    pub message_count: u32,
    pub last_active: String,
    pub channel: String,
}

pub struct SessionsScreen {
    sessions: Vec<SessionEntry>,
    list_state: ListState,
    search_query: String,
    searching: bool,
    filtered_indices: Vec<usize>,
}

impl SessionsScreen {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            list_state: ListState::default(),
            search_query: String::new(),
            searching: false,
            filtered_indices: Vec::new(),
        }
    }

    pub fn set_sessions(&mut self, sessions: Vec<SessionEntry>) {
        self.sessions = sessions;
        self.apply_filter();
        if !self.filtered_indices.is_empty() {
            self.list_state.select(Some(0));
        }
    }

    fn apply_filter(&mut self) {
        let query = self.search_query.to_lowercase();
        self.filtered_indices = self
            .sessions
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                query.is_empty()
                    || s.title.to_lowercase().contains(&query)
                    || s.model.to_lowercase().contains(&query)
                    || s.channel.to_lowercase().contains(&query)
            })
            .map(|(i, _)| i)
            .collect();
    }

    fn selected_session(&self) -> Option<&SessionEntry> {
        let idx = self.list_state.selected()?;
        let real_idx = *self.filtered_indices.get(idx)?;
        self.sessions.get(real_idx)
    }

    fn move_selection(&mut self, delta: i32) {
        if self.filtered_indices.is_empty() {
            return;
        }
        let current = self.list_state.selected().unwrap_or(0) as i32;
        let new = (current + delta).clamp(0, self.filtered_indices.len() as i32 - 1) as usize;
        self.list_state.select(Some(new));
    }
}

impl Screen for SessionsScreen {
    fn name(&self) -> &str {
        "Sessions"
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3), // search bar
                Constraint::Min(5),   // list
                Constraint::Length(3), // detail
            ])
            .split(area);

        // Search bar
        let search_block = Block::default()
            .title(" Search Sessions ")
            .borders(Borders::ALL)
            .border_style(if self.searching {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            });

        let search_text = if self.search_query.is_empty() && !self.searching {
            Span::styled("  / to search...", Style::default().fg(Color::DarkGray))
        } else {
            Span::raw(format!("  🔍 {}", self.search_query))
        };

        frame.render_widget(
            Paragraph::new(Line::from(search_text)).block(search_block),
            chunks[0],
        );

        // Session list
        let items: Vec<ListItem> = self
            .filtered_indices
            .iter()
            .map(|&idx| {
                let s = &self.sessions[idx];
                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled(
                            format!("  {} ", s.title),
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("[{}]", s.channel),
                            Style::default().fg(Color::Cyan),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            format!("    {} • {} msgs • {}", s.model, s.message_count, s.last_active),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]),
                ])
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .title(format!(" Sessions ({}) ", self.filtered_indices.len()))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");

        frame.render_stateful_widget(list, chunks[1], &mut self.list_state.clone());

        // Detail preview
        let detail = if let Some(session) = self.selected_session() {
            format!(
                " ID: {} │ Model: {} │ Channel: {} │ Messages: {}",
                &session.id[..8.min(session.id.len())],
                session.model,
                session.channel,
                session.message_count
            )
        } else {
            " No session selected".to_string()
        };

        let detail_block = Block::default()
            .title(" Details ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        frame.render_widget(
            Paragraph::new(detail).block(detail_block),
            chunks[2],
        );
    }

    fn handle_key(&mut self, key: KeyEvent) -> ScreenAction {
        if self.searching {
            match key.code {
                KeyCode::Esc => {
                    self.searching = false;
                    ScreenAction::None
                }
                KeyCode::Enter => {
                    self.searching = false;
                    ScreenAction::None
                }
                KeyCode::Backspace => {
                    self.search_query.pop();
                    self.apply_filter();
                    ScreenAction::None
                }
                KeyCode::Char(ch) => {
                    self.search_query.push(ch);
                    self.apply_filter();
                    ScreenAction::None
                }
                _ => ScreenAction::None,
            }
        } else {
            match key.code {
                KeyCode::Char('/') => {
                    self.searching = true;
                    ScreenAction::None
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    self.move_selection(1);
                    ScreenAction::None
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.move_selection(-1);
                    ScreenAction::None
                }
                KeyCode::Enter => {
                    if let Some(session) = self.selected_session() {
                        ScreenAction::Command(format!("session:open:{}", session.id))
                    } else {
                        ScreenAction::None
                    }
                }
                KeyCode::Char('d') => {
                    if let Some(session) = self.selected_session() {
                        ScreenAction::Command(format!("session:delete:{}", session.id))
                    } else {
                        ScreenAction::None
                    }
                }
                KeyCode::Char('n') => ScreenAction::Command("session:new".to_string()),
                KeyCode::Char('q') => ScreenAction::Quit,
                _ => ScreenAction::None,
            }
        }
    }
}
