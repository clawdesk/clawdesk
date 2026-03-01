//! Agents screen — view, configure, and monitor agents.

use super::{Screen, ScreenAction};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

#[derive(Debug, Clone)]
pub struct AgentEntry {
    pub id: String,
    pub name: String,
    pub model: String,
    pub status: AgentStatus,
    pub tools_count: usize,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AgentStatus {
    Idle,
    Running,
    Error,
    Disabled,
}

impl AgentStatus {
    fn label(&self) -> &str {
        match self {
            AgentStatus::Idle => "● idle",
            AgentStatus::Running => "▶ running",
            AgentStatus::Error => "✖ error",
            AgentStatus::Disabled => "○ disabled",
        }
    }

    fn color(&self) -> Color {
        match self {
            AgentStatus::Idle => Color::DarkGray,
            AgentStatus::Running => Color::Green,
            AgentStatus::Error => Color::Red,
            AgentStatus::Disabled => Color::DarkGray,
        }
    }
}

pub struct AgentsScreen {
    agents: Vec<AgentEntry>,
    list_state: ListState,
    show_detail: bool,
}

impl AgentsScreen {
    pub fn new() -> Self {
        Self {
            agents: Vec::new(),
            list_state: ListState::default(),
            show_detail: false,
        }
    }

    pub fn set_agents(&mut self, agents: Vec<AgentEntry>) {
        self.agents = agents;
        if !self.agents.is_empty() {
            self.list_state.select(Some(0));
        }
    }

    fn selected_agent(&self) -> Option<&AgentEntry> {
        let idx = self.list_state.selected()?;
        self.agents.get(idx)
    }

    fn move_selection(&mut self, delta: i32) {
        if self.agents.is_empty() {
            return;
        }
        let current = self.list_state.selected().unwrap_or(0) as i32;
        let new = (current + delta).clamp(0, self.agents.len() as i32 - 1) as usize;
        self.list_state.select(Some(new));
    }

    fn render_agent_list(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .agents
            .iter()
            .map(|agent| {
                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled(
                            format!("  {} ", agent.name),
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            agent.status.label(),
                            Style::default().fg(agent.status.color()),
                        ),
                    ]),
                    Line::from(vec![Span::styled(
                        format!("    {} • {} tools", agent.model, agent.tools_count),
                        Style::default().fg(Color::DarkGray),
                    )]),
                ])
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .title(format!(" Agents ({}) ", self.agents.len()))
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

        frame.render_stateful_widget(list, area, &mut self.list_state.clone());
    }

    fn render_detail(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .title(" Agent Details ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));

        if let Some(agent) = self.selected_agent() {
            let text = vec![
                Line::from(vec![
                    Span::styled("Name:   ", Style::default().fg(Color::DarkGray)),
                    Span::styled(&agent.name, Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
                ]),
                Line::from(vec![
                    Span::styled("ID:     ", Style::default().fg(Color::DarkGray)),
                    Span::styled(&agent.id, Style::default().fg(Color::DarkGray)),
                ]),
                Line::from(vec![
                    Span::styled("Model:  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(&agent.model, Style::default().fg(Color::Cyan)),
                ]),
                Line::from(vec![
                    Span::styled("Status: ", Style::default().fg(Color::DarkGray)),
                    Span::styled(agent.status.label(), Style::default().fg(agent.status.color())),
                ]),
                Line::from(vec![
                    Span::styled("Tools:  ", Style::default().fg(Color::DarkGray)),
                    Span::styled(format!("{}", agent.tools_count), Style::default().fg(Color::White)),
                ]),
                Line::from(""),
                Line::from(Span::styled(&agent.description, Style::default().fg(Color::White))),
            ];

            frame.render_widget(Paragraph::new(text).block(block).wrap(Wrap { trim: false }), area);
        } else {
            frame.render_widget(
                Paragraph::new(" Select an agent to view details").block(block),
                area,
            );
        }
    }
}

impl Screen for AgentsScreen {
    fn name(&self) -> &str {
        "Agents"
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        if self.show_detail {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
                .split(area);
            self.render_agent_list(frame, chunks[0]);
            self.render_detail(frame, chunks[1]);
        } else {
            self.render_agent_list(frame, area);
        }
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
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                self.show_detail = !self.show_detail;
                ScreenAction::None
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.show_detail = false;
                ScreenAction::None
            }
            KeyCode::Char('n') => ScreenAction::Command("agent:new".to_string()),
            KeyCode::Char('e') => {
                if let Some(agent) = self.selected_agent() {
                    ScreenAction::Command(format!("agent:edit:{}", agent.id))
                } else {
                    ScreenAction::None
                }
            }
            KeyCode::Char('r') => {
                if let Some(agent) = self.selected_agent() {
                    ScreenAction::Command(format!("agent:restart:{}", agent.id))
                } else {
                    ScreenAction::None
                }
            }
            KeyCode::Char('q') => ScreenAction::Quit,
            _ => ScreenAction::None,
        }
    }

    fn handle_backend_event(&mut self, event: &crate::event::BackendEvent) {
        if let crate::event::BackendEvent::AgentStateChanged { agent_id, state } = event {
            if let Some(agent) = self.agents.iter_mut().find(|a| a.id == *agent_id) {
                agent.status = match state.as_str() {
                    "running" => AgentStatus::Running,
                    "error" => AgentStatus::Error,
                    "disabled" => AgentStatus::Disabled,
                    _ => AgentStatus::Idle,
                };
            }
        }
    }
}
