//! Skills screen — browse and manage skills/tools.

use super::{Screen, ScreenAction};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

#[derive(Debug, Clone)]
pub struct SkillEntry {
    pub name: String,
    pub category: String,
    pub description: String,
    pub enabled: bool,
    pub version: String,
    pub usage_count: u64,
}

pub struct SkillsScreen {
    skills: Vec<SkillEntry>,
    list_state: ListState,
    show_detail: bool,
    filter_category: Option<String>,
    categories: Vec<String>,
}

impl SkillsScreen {
    pub fn new() -> Self {
        Self {
            skills: Vec::new(),
            list_state: ListState::default(),
            show_detail: false,
            filter_category: None,
            categories: Vec::new(),
        }
    }

    pub fn set_skills(&mut self, skills: Vec<SkillEntry>) {
        self.categories = skills
            .iter()
            .map(|s| s.category.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        self.skills = skills;
        if !self.skills.is_empty() {
            self.list_state.select(Some(0));
        }
    }

    fn filtered_skills(&self) -> Vec<(usize, &SkillEntry)> {
        self.skills
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                self.filter_category
                    .as_ref()
                    .map_or(true, |cat| s.category == *cat)
            })
            .collect()
    }

    fn selected_skill(&self) -> Option<&SkillEntry> {
        let idx = self.list_state.selected()?;
        let filtered = self.filtered_skills();
        filtered.get(idx).map(|(_, s)| *s)
    }

    fn move_selection(&mut self, delta: i32) {
        let count = self.filtered_skills().len();
        if count == 0 {
            return;
        }
        let current = self.list_state.selected().unwrap_or(0) as i32;
        let new = (current + delta).clamp(0, count as i32 - 1) as usize;
        self.list_state.select(Some(new));
    }
}

impl Screen for SkillsScreen {
    fn name(&self) -> &str {
        "Skills"
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let main_layout = if self.show_detail {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                .split(area)
        } else {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(100)])
                .split(area)
        };

        // Skills list
        let filtered = self.filtered_skills();
        let items: Vec<ListItem> = filtered
            .iter()
            .map(|(_, skill)| {
                let enabled_icon = if skill.enabled { "✓" } else { "✗" };
                let enabled_color = if skill.enabled {
                    Color::Green
                } else {
                    Color::Red
                };

                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled(
                            format!(" {} ", enabled_icon),
                            Style::default().fg(enabled_color),
                        ),
                        Span::styled(
                            &skill.name,
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("  v{}", skill.version),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled(
                            format!("    [{}] ", skill.category),
                            Style::default().fg(Color::Cyan),
                        ),
                        Span::styled(
                            format!("{} uses", skill.usage_count),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]),
                ])
            })
            .collect();

        let category_display = self
            .filter_category
            .as_deref()
            .unwrap_or("All");

        let list = List::new(items)
            .block(
                Block::default()
                    .title(format!(
                        " Skills ({}) — [{}] ",
                        filtered.len(),
                        category_display
                    ))
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

        frame.render_stateful_widget(list, main_layout[0], &mut self.list_state.clone());

        // Detail panel
        if self.show_detail && main_layout.len() > 1 {
            let detail_block = Block::default()
                .title(" Skill Details ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow));

            if let Some(skill) = self.selected_skill() {
                let text = vec![
                    Line::from(vec![
                        Span::styled("Name:     ", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            &skill.name,
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled("Version:  ", Style::default().fg(Color::DarkGray)),
                        Span::styled(&skill.version, Style::default().fg(Color::Cyan)),
                    ]),
                    Line::from(vec![
                        Span::styled("Category: ", Style::default().fg(Color::DarkGray)),
                        Span::styled(&skill.category, Style::default().fg(Color::Cyan)),
                    ]),
                    Line::from(vec![
                        Span::styled("Enabled:  ", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            if skill.enabled { "Yes" } else { "No" },
                            Style::default().fg(if skill.enabled {
                                Color::Green
                            } else {
                                Color::Red
                            }),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled("Uses:     ", Style::default().fg(Color::DarkGray)),
                        Span::styled(
                            format!("{}", skill.usage_count),
                            Style::default().fg(Color::White),
                        ),
                    ]),
                    Line::from(""),
                    Line::from(Span::styled(
                        &skill.description,
                        Style::default().fg(Color::White),
                    )),
                ];

                frame.render_widget(
                    Paragraph::new(text)
                        .block(detail_block)
                        .wrap(Wrap { trim: false }),
                    main_layout[1],
                );
            } else {
                frame.render_widget(
                    Paragraph::new("Select a skill").block(detail_block),
                    main_layout[1],
                );
            }
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
            KeyCode::Enter | KeyCode::Char('l') => {
                self.show_detail = !self.show_detail;
                ScreenAction::None
            }
            KeyCode::Char('h') => {
                self.show_detail = false;
                ScreenAction::None
            }
            KeyCode::Char('e') => {
                // Toggle enable/disable
                if let Some(idx) = self.list_state.selected() {
                    let filtered = self.filtered_skills();
                    if let Some(&(real_idx, _)) = filtered.get(idx) {
                        self.skills[real_idx].enabled = !self.skills[real_idx].enabled;
                        let name = self.skills[real_idx].name.clone();
                        let enabled = self.skills[real_idx].enabled;
                        return ScreenAction::Command(format!(
                            "skill:toggle:{}:{}",
                            name, enabled
                        ));
                    }
                }
                ScreenAction::None
            }
            KeyCode::Char('c') => {
                // Cycle category filter
                if self.filter_category.is_none() {
                    self.filter_category = self.categories.first().cloned();
                } else {
                    let current = self.filter_category.as_ref().unwrap();
                    let idx = self
                        .categories
                        .iter()
                        .position(|c| c == current)
                        .unwrap_or(0);
                    if idx + 1 < self.categories.len() {
                        self.filter_category = Some(self.categories[idx + 1].clone());
                    } else {
                        self.filter_category = None;
                    }
                }
                self.list_state.select(Some(0));
                ScreenAction::None
            }
            KeyCode::Char('q') => ScreenAction::Quit,
            _ => ScreenAction::None,
        }
    }
}
