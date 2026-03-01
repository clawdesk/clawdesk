//! Dashboard screen — system overview, metrics, and quick actions.

use super::{Screen, ScreenAction};
use clawdesk_types::DropOldest;
use crossterm::event::KeyEvent;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph};
use ratatui::Frame;

/// System metric snapshot.
#[derive(Debug, Clone, Default)]
pub struct SystemMetrics {
    pub active_agents: u32,
    pub total_sessions: u32,
    pub active_channels: u32,
    pub memory_entries: u64,
    pub cpu_usage: f64,
    pub memory_mb: f64,
    pub uptime_secs: u64,
    pub tools_available: u32,
    pub pending_tasks: u32,
}

/// Recent activity entry for the feed.
#[derive(Debug, Clone)]
pub struct ActivityEntry {
    pub timestamp: String,
    pub icon: &'static str,
    pub message: String,
}

pub struct DashboardScreen {
    metrics: SystemMetrics,
    activity_feed: DropOldest<ActivityEntry>,
    selected_panel: usize,
}

impl DashboardScreen {
    pub fn new() -> Self {
        Self {
            metrics: SystemMetrics::default(),
            activity_feed: DropOldest::new(100),
            selected_panel: 0,
        }
    }

    pub fn update_metrics(&mut self, metrics: SystemMetrics) {
        self.metrics = metrics;
    }

    pub fn push_activity(&mut self, entry: ActivityEntry) {
        self.activity_feed.push(entry);
    }

    fn render_metrics_panel(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .title(" System Metrics ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan));

        let inner = block.inner(area);
        frame.render_widget(block, area);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(2),
                Constraint::Length(2),
                Constraint::Min(0),
            ])
            .split(inner);

        let metric_style = Style::default().fg(Color::White);
        let value_style = Style::default().fg(Color::Green).add_modifier(Modifier::BOLD);

        let metrics = [
            ("Agents", format!("{}", self.metrics.active_agents)),
            ("Sessions", format!("{}", self.metrics.total_sessions)),
            ("Channels", format!("{}", self.metrics.active_channels)),
            ("Memory", format!("{} entries", self.metrics.memory_entries)),
        ];

        for (i, (label, value)) in metrics.iter().enumerate() {
            if i < chunks.len() {
                let line = Line::from(vec![
                    Span::styled(format!("  {:<12}", label), metric_style),
                    Span::styled(value.as_str(), value_style),
                ]);
                frame.render_widget(Paragraph::new(line), chunks[i]);
            }
        }

        // CPU gauge
        if chunks.len() > 4 {
            let cpu_gauge = Gauge::default()
                .block(Block::default().title("CPU"))
                .gauge_style(Style::default().fg(Color::Yellow))
                .percent((self.metrics.cpu_usage * 100.0) as u16);
            frame.render_widget(cpu_gauge, chunks[4]);
        }

        // Memory gauge
        if chunks.len() > 5 {
            let mem_label = format!("{:.0} MB", self.metrics.memory_mb);
            let mem_pct = ((self.metrics.memory_mb / 1024.0) * 100.0).min(100.0) as u16;
            let mem_gauge = Gauge::default()
                .block(Block::default().title("Mem"))
                .gauge_style(Style::default().fg(Color::Magenta))
                .label(mem_label)
                .percent(mem_pct);
            frame.render_widget(mem_gauge, chunks[5]);
        }
    }

    fn render_activity_feed(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .title(" Recent Activity ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow));

        let items: Vec<ListItem> = self
            .activity_feed
            .iter()
            .rev()
            .take(area.height as usize)
            .map(|entry| {
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {} ", entry.timestamp),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("{} ", entry.icon),
                        Style::default().fg(Color::Cyan),
                    ),
                    Span::raw(&entry.message),
                ]))
            })
            .collect();

        let list = List::new(items).block(block);
        frame.render_widget(list, area);
    }

    fn render_quick_actions(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default()
            .title(" Quick Actions ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Green));

        let actions = vec![
            "[n] New Session",
            "[a] New Agent",
            "[c] Open Chat",
            "[/] Command Palette",
            "[?] Help",
        ];

        let items: Vec<ListItem> = actions
            .iter()
            .map(|a| {
                ListItem::new(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(*a, Style::default().fg(Color::White)),
                ]))
            })
            .collect();

        let list = List::new(items).block(block);
        frame.render_widget(list, area);
    }
}

impl Screen for DashboardScreen {
    fn name(&self) -> &str {
        "Dashboard"
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let main_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
            .split(area);

        let left_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(main_chunks[0]);

        self.render_metrics_panel(frame, left_chunks[0]);
        self.render_quick_actions(frame, left_chunks[1]);
        self.render_activity_feed(frame, main_chunks[1]);
    }

    fn handle_key(&mut self, key: KeyEvent) -> ScreenAction {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Char('n') => ScreenAction::Command("session:new".to_string()),
            KeyCode::Char('c') => ScreenAction::SwitchTab(super::Tab::Chat),
            KeyCode::Char('/') => ScreenAction::Command("palette:open".to_string()),
            KeyCode::Char('?') => ScreenAction::Command("help:open".to_string()),
            KeyCode::Tab => {
                self.selected_panel = (self.selected_panel + 1) % 3;
                ScreenAction::None
            }
            _ => ScreenAction::None,
        }
    }

    fn on_tick(&mut self) {
        // Metrics would be polled from backend here
    }
}
