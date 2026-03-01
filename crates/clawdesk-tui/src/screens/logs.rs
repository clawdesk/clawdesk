//! Logs screen — real-time log viewer with filtering.

use super::{Screen, ScreenAction};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};
use ratatui::Frame;
use std::collections::VecDeque;

#[derive(Debug, Clone)]
pub struct LogLine {
    pub timestamp: String,
    pub level: LogLevel,
    pub target: String,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    fn label(&self) -> &str {
        match self {
            LogLevel::Trace => "TRACE",
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO ",
            LogLevel::Warn => "WARN ",
            LogLevel::Error => "ERROR",
        }
    }

    fn color(&self) -> Color {
        match self {
            LogLevel::Trace => Color::DarkGray,
            LogLevel::Debug => Color::White,
            LogLevel::Info => Color::Cyan,
            LogLevel::Warn => Color::Yellow,
            LogLevel::Error => Color::Red,
        }
    }
}

pub struct LogsScreen {
    logs: VecDeque<LogLine>,
    max_lines: usize,
    scroll_offset: usize,
    auto_scroll: bool,
    min_level: LogLevel,
    filter_target: String,
    filter_text: String,
    searching: bool,
    paused: bool,
}

impl LogsScreen {
    pub fn new() -> Self {
        Self {
            logs: VecDeque::new(),
            max_lines: 10_000,
            scroll_offset: 0,
            auto_scroll: true,
            min_level: LogLevel::Info,
            filter_target: String::new(),
            filter_text: String::new(),
            searching: false,
            paused: false,
        }
    }

    pub fn push_log(&mut self, log: LogLine) {
        if self.paused {
            return;
        }
        self.logs.push_back(log);
        while self.logs.len() > self.max_lines {
            self.logs.pop_front();
        }
        if self.auto_scroll {
            self.scroll_to_bottom();
        }
    }

    fn scroll_to_bottom(&mut self) {
        let filtered_count = self.filtered_logs().count();
        self.scroll_offset = filtered_count.saturating_sub(1);
    }

    fn filtered_logs(&self) -> impl Iterator<Item = &LogLine> {
        let min_level = self.min_level;
        let filter_target = self.filter_target.to_lowercase();
        let filter_text = self.filter_text.to_lowercase();

        self.logs.iter().filter(move |log| {
            log.level >= min_level
                && (filter_target.is_empty()
                    || log.target.to_lowercase().contains(&filter_target))
                && (filter_text.is_empty()
                    || log.message.to_lowercase().contains(&filter_text))
        })
    }

    fn cycle_level(&mut self) {
        self.min_level = match self.min_level {
            LogLevel::Trace => LogLevel::Debug,
            LogLevel::Debug => LogLevel::Info,
            LogLevel::Info => LogLevel::Warn,
            LogLevel::Warn => LogLevel::Error,
            LogLevel::Error => LogLevel::Trace,
        };
    }
}

impl Screen for LogsScreen {
    fn name(&self) -> &str {
        "Logs"
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1), // status bar
                Constraint::Min(5),    // logs
                Constraint::Length(3), // search/filter
            ])
            .split(area);

        // Status bar
        let status = Line::from(vec![
            Span::styled(
                format!(" Level: {} ", self.min_level.label()),
                Style::default()
                    .fg(Color::Black)
                    .bg(self.min_level.color()),
            ),
            Span::styled(
                format!(" {} lines ", self.logs.len()),
                Style::default().fg(Color::DarkGray),
            ),
            if self.auto_scroll {
                Span::styled(" ↓ auto-scroll ", Style::default().fg(Color::Green))
            } else {
                Span::styled(" ↕ manual ", Style::default().fg(Color::Yellow))
            },
            if self.paused {
                Span::styled(" ⏸ paused ", Style::default().fg(Color::Red))
            } else {
                Span::styled(" ● live ", Style::default().fg(Color::Green))
            },
            if !self.filter_target.is_empty() {
                Span::styled(
                    format!(" target:{} ", self.filter_target),
                    Style::default().fg(Color::Cyan),
                )
            } else {
                Span::raw("")
            },
        ]);
        frame.render_widget(Paragraph::new(status), chunks[0]);

        // Log entries
        let visible_height = chunks[1].height as usize;
        let filtered: Vec<&LogLine> = self.filtered_logs().collect();
        let start = if filtered.len() > visible_height {
            filtered
                .len()
                .saturating_sub(visible_height)
                .min(self.scroll_offset)
        } else {
            0
        };

        let items: Vec<ListItem> = filtered
            .iter()
            .skip(start)
            .take(visible_height)
            .map(|log| {
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {} ", log.timestamp),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::styled(
                        format!("{} ", log.level.label()),
                        Style::default().fg(log.level.color()),
                    ),
                    Span::styled(
                        format!("[{}] ", log.target),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(&log.message),
                ]))
            })
            .collect();

        let list = List::new(items).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        frame.render_widget(list, chunks[1]);

        // Search / filter bar
        let filter_block = Block::default()
            .title(if self.searching {
                " Filter (Esc to close) "
            } else {
                " [/] Search  [L] Level  [p] Pause  [G] Bottom "
            })
            .borders(Borders::ALL)
            .border_style(if self.searching {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            });

        let filter_text = if self.searching {
            format!("🔍 {}", self.filter_text)
        } else if !self.filter_text.is_empty() {
            format!("Filter: {}", self.filter_text)
        } else {
            String::new()
        };

        frame.render_widget(
            Paragraph::new(filter_text).block(filter_block),
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
                    self.filter_text.pop();
                    ScreenAction::None
                }
                KeyCode::Char(ch) => {
                    self.filter_text.push(ch);
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
                    self.auto_scroll = false;
                    self.scroll_offset = self.scroll_offset.saturating_add(1);
                    ScreenAction::None
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.auto_scroll = false;
                    self.scroll_offset = self.scroll_offset.saturating_sub(1);
                    ScreenAction::None
                }
                KeyCode::Char('G') => {
                    self.auto_scroll = true;
                    self.scroll_to_bottom();
                    ScreenAction::None
                }
                KeyCode::Char('g') => {
                    self.auto_scroll = false;
                    self.scroll_offset = 0;
                    ScreenAction::None
                }
                KeyCode::Char('L') => {
                    self.cycle_level();
                    ScreenAction::None
                }
                KeyCode::Char('p') => {
                    self.paused = !self.paused;
                    ScreenAction::None
                }
                KeyCode::Char('c') => {
                    self.logs.clear();
                    self.scroll_offset = 0;
                    ScreenAction::None
                }
                KeyCode::Char('q') => ScreenAction::Quit,
                _ => ScreenAction::None,
            }
        }
    }

    fn handle_backend_event(&mut self, event: &crate::event::BackendEvent) {
        if let crate::event::BackendEvent::LogEntry {
            level,
            target,
            message,
        } = event
        {
            let log_level = match level.to_lowercase().as_str() {
                "trace" => LogLevel::Trace,
                "debug" => LogLevel::Debug,
                "info" => LogLevel::Info,
                "warn" | "warning" => LogLevel::Warn,
                "error" => LogLevel::Error,
                _ => LogLevel::Info,
            };

            self.push_log(LogLine {
                timestamp: chrono::Local::now().format("%H:%M:%S%.3f").to_string(),
                level: log_level,
                target: target.clone(),
                message: message.clone(),
            });
        }
    }
}
