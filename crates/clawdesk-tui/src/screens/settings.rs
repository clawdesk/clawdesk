//! Settings screen — application configuration.

use super::{Screen, ScreenAction};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

#[derive(Debug, Clone)]
pub struct SettingEntry {
    pub key: String,
    pub label: String,
    pub value: String,
    pub category: String,
    pub description: String,
    pub editable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SettingsFocus {
    Categories,
    Settings,
    Editor,
}

pub struct SettingsScreen {
    settings: Vec<SettingEntry>,
    categories: Vec<String>,
    selected_category: usize,
    list_state: ListState,
    focus: SettingsFocus,
    editing: bool,
    edit_buffer: String,
}

impl SettingsScreen {
    pub fn new() -> Self {
        Self {
            settings: Vec::new(),
            categories: vec![
                "General".to_string(),
                "Models".to_string(),
                "Channels".to_string(),
                "Security".to_string(),
                "Display".to_string(),
                "Advanced".to_string(),
            ],
            selected_category: 0,
            list_state: ListState::default(),
            focus: SettingsFocus::Categories,
            editing: false,
            edit_buffer: String::new(),
        }
    }

    pub fn set_settings(&mut self, settings: Vec<SettingEntry>) {
        self.settings = settings;
    }

    fn current_category(&self) -> &str {
        self.categories
            .get(self.selected_category)
            .map(|s| s.as_str())
            .unwrap_or("General")
    }

    fn filtered_settings(&self) -> Vec<(usize, &SettingEntry)> {
        let cat = self.current_category();
        self.settings
            .iter()
            .enumerate()
            .filter(|(_, s)| s.category == cat)
            .collect()
    }

    fn selected_setting(&self) -> Option<&SettingEntry> {
        let idx = self.list_state.selected()?;
        let filtered = self.filtered_settings();
        filtered.get(idx).map(|(_, s)| *s)
    }
}

impl Screen for SettingsScreen {
    fn name(&self) -> &str {
        "Settings"
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(18), Constraint::Min(40)])
            .split(area);

        // Category sidebar
        let cat_items: Vec<ListItem> = self
            .categories
            .iter()
            .enumerate()
            .map(|(i, cat)| {
                let style = if i == self.selected_category {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };
                ListItem::new(Line::from(Span::styled(format!("  {}", cat), style)))
            })
            .collect();

        let cat_list = List::new(cat_items).block(
            Block::default()
                .title(" Categories ")
                .borders(Borders::ALL)
                .border_style(if self.focus == SettingsFocus::Categories {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default().fg(Color::DarkGray)
                }),
        );
        frame.render_widget(cat_list, chunks[0]);

        // Settings panel
        let right_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(5)])
            .split(chunks[1]);

        let filtered = self.filtered_settings();
        let items: Vec<ListItem> = filtered
            .iter()
            .map(|(_, setting)| {
                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled(
                            format!("  {} ", setting.label),
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled("    ", Style::default()),
                        Span::styled(
                            &setting.value,
                            Style::default().fg(if setting.editable {
                                Color::Green
                            } else {
                                Color::DarkGray
                            }),
                        ),
                        if !setting.editable {
                            Span::styled(" (read-only)", Style::default().fg(Color::DarkGray))
                        } else {
                            Span::raw("")
                        },
                    ]),
                ])
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .title(format!(
                        " {} Settings ({}) ",
                        self.current_category(),
                        filtered.len()
                    ))
                    .borders(Borders::ALL)
                    .border_style(if self.focus == SettingsFocus::Settings {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default().fg(Color::DarkGray)
                    }),
            )
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▸ ");

        frame.render_stateful_widget(list, right_chunks[0], &mut self.list_state.clone());

        // Description / edit area
        let desc_block = Block::default()
            .title(if self.editing { " Edit Value " } else { " Description " })
            .borders(Borders::ALL)
            .border_style(if self.editing {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            });

        let desc_text = if self.editing {
            format!("> {}", self.edit_buffer)
        } else if let Some(setting) = self.selected_setting() {
            format!("{}\n\nKey: {}", setting.description, setting.key)
        } else {
            "Select a setting".to_string()
        };

        frame.render_widget(
            Paragraph::new(desc_text)
                .block(desc_block)
                .wrap(Wrap { trim: false }),
            right_chunks[1],
        );
    }

    fn handle_key(&mut self, key: KeyEvent) -> ScreenAction {
        if self.editing {
            match key.code {
                KeyCode::Esc => {
                    self.editing = false;
                    self.edit_buffer.clear();
                    ScreenAction::None
                }
                KeyCode::Enter => {
                    let value = self.edit_buffer.clone();
                    self.editing = false;
                    self.edit_buffer.clear();
                    if let Some(setting) = self.selected_setting() {
                        ScreenAction::Command(format!(
                            "setting:set:{}:{}",
                            setting.key, value
                        ))
                    } else {
                        ScreenAction::None
                    }
                }
                KeyCode::Backspace => {
                    self.edit_buffer.pop();
                    ScreenAction::None
                }
                KeyCode::Char(ch) => {
                    self.edit_buffer.push(ch);
                    ScreenAction::None
                }
                _ => ScreenAction::None,
            }
        } else {
            match (self.focus, key.code) {
                // Tab between panels
                (_, KeyCode::Tab) => {
                    self.focus = match self.focus {
                        SettingsFocus::Categories => SettingsFocus::Settings,
                        SettingsFocus::Settings => SettingsFocus::Categories,
                        SettingsFocus::Editor => SettingsFocus::Categories,
                    };
                    ScreenAction::None
                }
                // Category navigation
                (SettingsFocus::Categories, KeyCode::Char('j') | KeyCode::Down) => {
                    self.selected_category =
                        (self.selected_category + 1).min(self.categories.len() - 1);
                    self.list_state.select(Some(0));
                    ScreenAction::None
                }
                (SettingsFocus::Categories, KeyCode::Char('k') | KeyCode::Up) => {
                    self.selected_category = self.selected_category.saturating_sub(1);
                    self.list_state.select(Some(0));
                    ScreenAction::None
                }
                (SettingsFocus::Categories, KeyCode::Enter | KeyCode::Char('l')) => {
                    self.focus = SettingsFocus::Settings;
                    ScreenAction::None
                }
                // Settings navigation
                (SettingsFocus::Settings, KeyCode::Char('j') | KeyCode::Down) => {
                    let count = self.filtered_settings().len();
                    if count > 0 {
                        let current = self.list_state.selected().unwrap_or(0);
                        self.list_state.select(Some((current + 1).min(count - 1)));
                    }
                    ScreenAction::None
                }
                (SettingsFocus::Settings, KeyCode::Char('k') | KeyCode::Up) => {
                    let current = self.list_state.selected().unwrap_or(0);
                    self.list_state
                        .select(Some(current.saturating_sub(1)));
                    ScreenAction::None
                }
                (SettingsFocus::Settings, KeyCode::Char('h') | KeyCode::Left) => {
                    self.focus = SettingsFocus::Categories;
                    ScreenAction::None
                }
                (SettingsFocus::Settings, KeyCode::Enter) => {
                    let should_edit = self.selected_setting()
                        .map(|s| (s.editable, s.value.clone()))
                        .filter(|(editable, _)| *editable);
                    if let Some((_, value)) = should_edit {
                        self.editing = true;
                        self.edit_buffer = value;
                    }
                    ScreenAction::None
                }
                (_, KeyCode::Char('q')) => ScreenAction::Quit,
                _ => ScreenAction::None,
            }
        }
    }
}
