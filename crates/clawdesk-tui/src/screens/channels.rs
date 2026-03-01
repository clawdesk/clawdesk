//! Channels screen — view and manage communication channels.

use super::{Screen, ScreenAction};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, ListState, Paragraph, Row, Table};
use ratatui::Frame;

#[derive(Debug, Clone)]
pub struct ChannelEntry {
    pub id: String,
    pub name: String,
    pub channel_type: String,
    pub healthy: bool,
    pub connected: bool,
    pub message_count: u64,
    pub last_message: String,
}

pub struct ChannelsScreen {
    channels: Vec<ChannelEntry>,
    table_state: ratatui::widgets::TableState,
}

impl ChannelsScreen {
    pub fn new() -> Self {
        Self {
            channels: Vec::new(),
            table_state: ratatui::widgets::TableState::default(),
        }
    }

    pub fn set_channels(&mut self, channels: Vec<ChannelEntry>) {
        self.channels = channels;
        if !self.channels.is_empty() {
            self.table_state.select(Some(0));
        }
    }

    fn selected_channel(&self) -> Option<&ChannelEntry> {
        let idx = self.table_state.selected()?;
        self.channels.get(idx)
    }

    fn move_selection(&mut self, delta: i32) {
        if self.channels.is_empty() {
            return;
        }
        let current = self.table_state.selected().unwrap_or(0) as i32;
        let new = (current + delta).clamp(0, self.channels.len() as i32 - 1) as usize;
        self.table_state.select(Some(new));
    }
}

impl Screen for ChannelsScreen {
    fn name(&self) -> &str {
        "Channels"
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(5)])
            .split(area);

        // Channel table
        let header = Row::new(vec!["", "Channel", "Type", "Status", "Messages", "Last Active"])
            .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD));

        let rows: Vec<Row> = self
            .channels
            .iter()
            .map(|ch| {
                let status_icon = if !ch.connected {
                    "○"
                } else if ch.healthy {
                    "●"
                } else {
                    "▲"
                };
                let status_color = if !ch.connected {
                    Color::DarkGray
                } else if ch.healthy {
                    Color::Green
                } else {
                    Color::Yellow
                };

                Row::new(vec![
                    status_icon.to_string(),
                    ch.name.clone(),
                    ch.channel_type.clone(),
                    if ch.connected {
                        "connected".to_string()
                    } else {
                        "disconnected".to_string()
                    },
                    format!("{}", ch.message_count),
                    ch.last_message.clone(),
                ])
                .style(Style::default().fg(if ch.connected {
                    Color::White
                } else {
                    Color::DarkGray
                }))
            })
            .collect();

        let table = Table::new(
            rows,
            [
                Constraint::Length(2),
                Constraint::Percentage(20),
                Constraint::Percentage(15),
                Constraint::Percentage(15),
                Constraint::Percentage(15),
                Constraint::Percentage(30),
            ],
        )
        .header(header)
        .block(
            Block::default()
                .title(format!(" Channels ({}) ", self.channels.len()))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .row_highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );

        frame.render_stateful_widget(table, chunks[0], &mut self.table_state.clone());

        // Help bar
        let help = Block::default()
            .title(" Actions ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        let help_text = Line::from(vec![
            Span::styled(" [Enter] ", Style::default().fg(Color::Yellow)),
            Span::raw("Configure  "),
            Span::styled("[c] ", Style::default().fg(Color::Yellow)),
            Span::raw("Connect  "),
            Span::styled("[d] ", Style::default().fg(Color::Yellow)),
            Span::raw("Disconnect  "),
            Span::styled("[t] ", Style::default().fg(Color::Yellow)),
            Span::raw("Test  "),
            Span::styled("[n] ", Style::default().fg(Color::Yellow)),
            Span::raw("New Channel"),
        ]);

        frame.render_widget(Paragraph::new(help_text).block(help), chunks[1]);
    }

    fn handle_key(&mut self, key: KeyEvent) -> ScreenAction {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.move_selection(1);
                ScreenAction::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.move_selection(-1);
                ScreenAction::None
            }
            KeyCode::Enter => {
                if let Some(ch) = self.selected_channel() {
                    ScreenAction::Command(format!("channel:configure:{}", ch.id))
                } else {
                    ScreenAction::None
                }
            }
            KeyCode::Char('c') => {
                if let Some(ch) = self.selected_channel() {
                    ScreenAction::Command(format!("channel:connect:{}", ch.id))
                } else {
                    ScreenAction::None
                }
            }
            KeyCode::Char('d') => {
                if let Some(ch) = self.selected_channel() {
                    ScreenAction::Command(format!("channel:disconnect:{}", ch.id))
                } else {
                    ScreenAction::None
                }
            }
            KeyCode::Char('t') => {
                if let Some(ch) = self.selected_channel() {
                    ScreenAction::Command(format!("channel:test:{}", ch.id))
                } else {
                    ScreenAction::None
                }
            }
            KeyCode::Char('n') => ScreenAction::Command("channel:new".to_string()),
            KeyCode::Char('q') => ScreenAction::Quit,
            _ => ScreenAction::None,
        }
    }

    fn handle_backend_event(&mut self, event: &crate::event::BackendEvent) {
        if let crate::event::BackendEvent::ChannelHealth {
            channel_id,
            healthy,
        } = event
        {
            if let Some(ch) = self.channels.iter_mut().find(|c| c.id == *channel_id) {
                ch.healthy = *healthy;
            }
        }
    }
}
