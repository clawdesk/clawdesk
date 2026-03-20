//! Health Dashboard TUI screen — security score and check results.

use super::{Screen, ScreenAction};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;

/// Health check result for TUI display.
#[derive(Debug, Clone)]
pub struct HealthCheck {
    pub name: String,
    pub passed: bool,
    pub weight: u32,
    pub message: String,
    pub remediation: Option<String>,
}

pub struct HealthDashboardScreen {
    pub score: u32,
    pub grade: String,
    pub checks: Vec<HealthCheck>,
    pub selected: usize,
    pub show_details: bool,
}

impl HealthDashboardScreen {
    pub fn new() -> Self {
        Self {
            score: 0,
            grade: "?".to_string(),
            checks: Vec::new(),
            selected: 0,
            show_details: false,
        }
    }

    pub fn update_report(&mut self, score: u32, grade: String, checks: Vec<HealthCheck>) {
        self.score = score;
        self.grade = grade;
        self.checks = checks;
    }
}

impl Screen for HealthDashboardScreen {
    fn name(&self) -> &str {
        "Health"
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5), // Score header
                Constraint::Min(10),   // Check list
            ])
            .split(area);

        // Score gauge
        let gauge_color = match self.score {
            90..=100 => Color::Green,
            70..=89 => Color::Yellow,
            50..=69 => Color::Rgb(255, 165, 0),
            _ => Color::Red,
        };
        let gauge = Gauge::default()
            .block(Block::default()
                .title(format!(" Security Score: {}/100 ({}) ", self.score, self.grade))
                .borders(Borders::ALL))
            .gauge_style(Style::default().fg(gauge_color))
            .percent(self.score as u16);
        frame.render_widget(gauge, chunks[0]);

        // Check list
        let items: Vec<ListItem> = self.checks.iter().enumerate().map(|(i, check)| {
            let icon = if check.passed { "✓" } else { "✗" };
            let color = if check.passed { Color::Green } else { Color::Red };
            let style = if i == self.selected {
                Style::default().fg(color).add_modifier(Modifier::BOLD | Modifier::REVERSED)
            } else {
                Style::default().fg(color)
            };
            let line = Line::from(vec![
                Span::styled(format!(" {} ", icon), style),
                Span::styled(
                    format!("{:<25} [w={}] {}", check.name, check.weight, check.message),
                    style,
                ),
            ]);
            ListItem::new(line)
        }).collect();

        let list = List::new(items)
            .block(Block::default()
                .title(" Security Checks ")
                .borders(Borders::ALL));
        frame.render_widget(list, chunks[1]);
    }

    fn handle_key(&mut self, key: KeyEvent) -> ScreenAction {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected + 1 < self.checks.len() {
                    self.selected += 1;
                }
            }
            KeyCode::Enter => {
                self.show_details = !self.show_details;
            }
            _ => {}
        }
        ScreenAction::None
    }
}
