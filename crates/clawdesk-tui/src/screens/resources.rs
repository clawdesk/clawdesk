//! Resource Monitor TUI screen — real-time memory, CPU, cost tracking.

use super::{Screen, ScreenAction};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table, Wrap};
use ratatui::Frame;

/// Provider cost entry for the TUI.
#[derive(Debug, Clone)]
pub struct ProviderCost {
    pub name: String,
    pub tokens: u64,
    pub cost_usd: f64,
    pub requests: u64,
}

pub struct ResourceMonitorScreen {
    pub rss_mb: f64,
    pub cpu_percent: f32,
    pub uptime_secs: u64,
    pub active_agents: usize,
    pub memory_ratio: f64,
    pub baseline_display: String,
    pub providers: Vec<ProviderCost>,
    pub total_cost: f64,
    pub savings_display: String,
}

impl ResourceMonitorScreen {
    pub fn new() -> Self {
        Self {
            rss_mb: 0.0,
            cpu_percent: 0.0,
            uptime_secs: 0,
            active_agents: 0,
            memory_ratio: 0.0,
            baseline_display: String::new(),
            providers: Vec::new(),
            total_cost: 0.0,
            savings_display: String::new(),
        }
    }

    pub fn update_metrics(
        &mut self,
        rss_mb: f64,
        cpu_percent: f32,
        uptime_secs: u64,
        active_agents: usize,
        memory_ratio: f64,
        baseline_display: String,
        total_cost: f64,
        savings_display: String,
    ) {
        self.rss_mb = rss_mb;
        self.cpu_percent = cpu_percent;
        self.uptime_secs = uptime_secs;
        self.active_agents = active_agents;
        self.memory_ratio = memory_ratio;
        self.baseline_display = baseline_display;
        self.total_cost = total_cost;
        self.savings_display = savings_display;
    }

    fn format_uptime(secs: u64) -> String {
        let hours = secs / 3600;
        let minutes = (secs % 3600) / 60;
        if hours > 0 {
            format!("{}h {}m", hours, minutes)
        } else {
            format!("{}m", minutes)
        }
    }
}

impl Screen for ResourceMonitorScreen {
    fn name(&self) -> &str {
        "Resources"
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(8),  // System metrics
                Constraint::Min(6),    // Provider costs
                Constraint::Length(3), // Cost savings
            ])
            .split(area);

        // System metrics panel
        let metrics_text = vec![
            Line::from(vec![
                Span::styled("  RSS Memory:   ", Style::default().fg(Color::Cyan)),
                Span::raw(format!("{:.1} MB", self.rss_mb)),
            ]),
            Line::from(vec![
                Span::styled("  CPU Usage:    ", Style::default().fg(Color::Cyan)),
                Span::raw(format!("{:.1}%", self.cpu_percent)),
            ]),
            Line::from(vec![
                Span::styled("  Uptime:       ", Style::default().fg(Color::Cyan)),
                Span::raw(Self::format_uptime(self.uptime_secs)),
            ]),
            Line::from(vec![
                Span::styled("  Agents:       ", Style::default().fg(Color::Cyan)),
                Span::raw(format!("{}", self.active_agents)),
            ]),
            Line::from(vec![
                Span::styled("  Comparison:   ", Style::default().fg(Color::Cyan)),
                Span::raw(self.baseline_display.clone()),
            ]),
        ];
        let metrics = Paragraph::new(metrics_text)
            .block(Block::default().title(" System Resources ").borders(Borders::ALL));
        frame.render_widget(metrics, chunks[0]);

        // Provider costs table
        let header = Row::new(vec!["Provider", "Tokens", "Cost", "Requests"])
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));
        let rows: Vec<Row> = self.providers.iter().map(|p| {
            Row::new(vec![
                p.name.clone(),
                format!("{}", p.tokens),
                format!("${:.4}", p.cost_usd),
                format!("{}", p.requests),
            ])
        }).collect();
        let table = Table::new(
            rows,
            [
                Constraint::Percentage(30),
                Constraint::Percentage(25),
                Constraint::Percentage(25),
                Constraint::Percentage(20),
            ],
        )
        .header(header)
        .block(Block::default().title(" Provider Costs ").borders(Borders::ALL));
        frame.render_widget(table, chunks[1]);

        // Cost savings
        let savings = Paragraph::new(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(&self.savings_display, Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
        ]))
        .block(Block::default().title(" Cost Router ").borders(Borders::ALL));
        frame.render_widget(savings, chunks[2]);
    }

    fn handle_key(&mut self, _key: KeyEvent) -> ScreenAction {
        ScreenAction::None
    }
}
