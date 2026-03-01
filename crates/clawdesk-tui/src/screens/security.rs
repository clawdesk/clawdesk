//! Security screen — sandbox status, vault, and policy management.

use super::{Screen, ScreenAction};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Row, Table, Wrap};
use ratatui::Frame;

#[derive(Debug, Clone)]
pub struct SandboxStatus {
    pub level: String,
    pub available_backends: Vec<String>,
    pub active_backend: String,
    pub resource_limits: String,
}

#[derive(Debug, Clone)]
pub struct VaultEntry {
    pub name: String,
    pub integration: String,
    pub stored_at: String,
    pub expires: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PolicyEntry {
    pub name: String,
    pub target: String,
    pub action: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SecurityPanel {
    Overview,
    Vault,
    Policies,
}

pub struct SecurityScreen {
    active_panel: SecurityPanel,
    sandbox: SandboxStatus,
    vault_entries: Vec<VaultEntry>,
    policies: Vec<PolicyEntry>,
    vault_state: ListState,
    policy_state: ratatui::widgets::TableState,
    vault_unlocked: bool,
}

impl SecurityScreen {
    pub fn new() -> Self {
        Self {
            active_panel: SecurityPanel::Overview,
            sandbox: SandboxStatus {
                level: "ProcessIsolation".to_string(),
                available_backends: vec![
                    "Workspace".to_string(),
                    "Subprocess".to_string(),
                ],
                active_backend: "Subprocess".to_string(),
                resource_limits: "cpu=30s, mem=512MB, fds=64".to_string(),
            },
            vault_entries: Vec::new(),
            policies: Vec::new(),
            vault_state: ListState::default(),
            policy_state: ratatui::widgets::TableState::default(),
            vault_unlocked: false,
        }
    }

    pub fn set_vault_entries(&mut self, entries: Vec<VaultEntry>) {
        self.vault_entries = entries;
        if !self.vault_entries.is_empty() {
            self.vault_state.select(Some(0));
        }
    }

    pub fn set_policies(&mut self, policies: Vec<PolicyEntry>) {
        self.policies = policies;
        if !self.policies.is_empty() {
            self.policy_state.select(Some(0));
        }
    }

    fn render_overview(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(8), // sandbox
                Constraint::Length(6), // quick stats
                Constraint::Min(3),   // help
            ])
            .split(area);

        // Sandbox status
        let sandbox_block = Block::default()
            .title(" Sandbox Status ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Green));

        let sandbox_text = vec![
            Line::from(vec![
                Span::styled("Level:    ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    &self.sandbox.level,
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::styled("Active:   ", Style::default().fg(Color::DarkGray)),
                Span::styled(&self.sandbox.active_backend, Style::default().fg(Color::Cyan)),
            ]),
            Line::from(vec![
                Span::styled("Backends: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    self.sandbox.available_backends.join(", "),
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(vec![
                Span::styled("Limits:   ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    &self.sandbox.resource_limits,
                    Style::default().fg(Color::Yellow),
                ),
            ]),
        ];

        frame.render_widget(
            Paragraph::new(sandbox_text)
                .block(sandbox_block)
                .wrap(Wrap { trim: false }),
            chunks[0],
        );

        // Quick stats
        let stats_block = Block::default()
            .title(" Security Summary ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));

        let vault_icon = if self.vault_unlocked { "🔓" } else { "🔒" };
        let stats_text = vec![
            Line::from(vec![
                Span::styled(
                    format!("  Vault: {} ", vault_icon),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    if self.vault_unlocked {
                        "unlocked"
                    } else {
                        "locked"
                    },
                    Style::default().fg(if self.vault_unlocked {
                        Color::Green
                    } else {
                        Color::Yellow
                    }),
                ),
                Span::styled(
                    format!("  ({} credentials)", self.vault_entries.len()),
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
            Line::from(vec![
                Span::styled(
                    format!("  Policies: {} active", self.policies.iter().filter(|p| p.enabled).count()),
                    Style::default().fg(Color::White),
                ),
                Span::styled(
                    format!(" / {} total", self.policies.len()),
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
        ];

        frame.render_widget(
            Paragraph::new(stats_text).block(stats_block),
            chunks[1],
        );

        // Help
        let help = Paragraph::new(vec![
            Line::from(vec![
                Span::styled(" [1] ", Style::default().fg(Color::Yellow)),
                Span::raw("Overview  "),
                Span::styled("[2] ", Style::default().fg(Color::Yellow)),
                Span::raw("Vault  "),
                Span::styled("[3] ", Style::default().fg(Color::Yellow)),
                Span::raw("Policies  "),
                Span::styled("[u] ", Style::default().fg(Color::Yellow)),
                Span::raw("Unlock vault"),
            ]),
        ])
        .block(
            Block::default()
                .title(" Navigation ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        frame.render_widget(help, chunks[2]);
    }

    fn render_vault(&self, frame: &mut Frame, area: Rect) {
        if !self.vault_unlocked {
            let block = Block::default()
                .title(" Credential Vault 🔒 ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow));

            let text = Paragraph::new(vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Vault is locked. Press [u] to unlock.",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )),
            ])
            .block(block);
            frame.render_widget(text, area);
            return;
        }

        let items: Vec<ListItem> = self
            .vault_entries
            .iter()
            .map(|entry| {
                let expires = entry
                    .expires
                    .as_deref()
                    .unwrap_or("never");

                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled(
                            format!("  {} ", entry.name),
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("[{}]", entry.integration),
                            Style::default().fg(Color::Cyan),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            format!("    Stored: {} │ Expires: {}", entry.stored_at, expires),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]),
                ])
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .title(format!(
                        " Credential Vault 🔓 ({}) ",
                        self.vault_entries.len()
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Green)),
            )
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            );

        frame.render_stateful_widget(list, area, &mut self.vault_state.clone());
    }

    fn render_policies(&self, frame: &mut Frame, area: Rect) {
        let rows: Vec<Row> = self
            .policies
            .iter()
            .map(|policy| {
                Row::new(vec![
                    if policy.enabled { "✓" } else { "✗" }.to_string(),
                    policy.name.clone(),
                    policy.target.clone(),
                    policy.action.clone(),
                ])
                .style(Style::default().fg(if policy.enabled {
                    Color::White
                } else {
                    Color::DarkGray
                }))
            })
            .collect();

        let header = Row::new(vec!["", "Policy", "Target", "Action"])
            .style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            );

        let table = Table::new(
            rows,
            [
                Constraint::Length(3),
                Constraint::Percentage(30),
                Constraint::Percentage(35),
                Constraint::Percentage(30),
            ],
        )
        .header(header)
        .block(
            Block::default()
                .title(format!(" Security Policies ({}) ", self.policies.len()))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .row_highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );

        frame.render_stateful_widget(table, area, &mut self.policy_state.clone());
    }
}

impl Screen for SecurityScreen {
    fn name(&self) -> &str {
        "Security"
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        match self.active_panel {
            SecurityPanel::Overview => self.render_overview(frame, area),
            SecurityPanel::Vault => self.render_vault(frame, area),
            SecurityPanel::Policies => self.render_policies(frame, area),
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> ScreenAction {
        match key.code {
            KeyCode::Char('1') => {
                self.active_panel = SecurityPanel::Overview;
                ScreenAction::None
            }
            KeyCode::Char('2') => {
                self.active_panel = SecurityPanel::Vault;
                ScreenAction::None
            }
            KeyCode::Char('3') => {
                self.active_panel = SecurityPanel::Policies;
                ScreenAction::None
            }
            KeyCode::Char('u') => ScreenAction::Command("vault:unlock".to_string()),
            KeyCode::Char('j') | KeyCode::Down => {
                match self.active_panel {
                    SecurityPanel::Vault => {
                        let count = self.vault_entries.len();
                        if count > 0 {
                            let current = self.vault_state.selected().unwrap_or(0);
                            self.vault_state.select(Some((current + 1).min(count - 1)));
                        }
                    }
                    SecurityPanel::Policies => {
                        let count = self.policies.len();
                        if count > 0 {
                            let current = self.policy_state.selected().unwrap_or(0);
                            self.policy_state.select(Some((current + 1).min(count - 1)));
                        }
                    }
                    _ => {}
                }
                ScreenAction::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                match self.active_panel {
                    SecurityPanel::Vault => {
                        let current = self.vault_state.selected().unwrap_or(0);
                        self.vault_state
                            .select(Some(current.saturating_sub(1)));
                    }
                    SecurityPanel::Policies => {
                        let current = self.policy_state.selected().unwrap_or(0);
                        self.policy_state
                            .select(Some(current.saturating_sub(1)));
                    }
                    _ => {}
                }
                ScreenAction::None
            }
            KeyCode::Char('q') => ScreenAction::Quit,
            _ => ScreenAction::None,
        }
    }
}
