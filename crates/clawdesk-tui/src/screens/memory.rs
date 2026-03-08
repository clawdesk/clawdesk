//! Memory screen — browse and search memory entries.

use super::{Screen, ScreenAction};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use clawdesk_types::truncate_to_char_boundary;

#[derive(Debug, Clone)]
pub struct MemoryEntry {
    pub key: String,
    pub namespace: String,
    pub value_preview: String,
    pub size_bytes: usize,
    pub created_at: String,
    pub ttl: Option<String>,
}

pub struct MemoryScreen {
    entries: Vec<MemoryEntry>,
    list_state: ListState,
    search_query: String,
    searching: bool,
    filtered_indices: Vec<usize>,
    selected_namespace: Option<String>,
    namespaces: Vec<String>,
}

impl MemoryScreen {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            list_state: ListState::default(),
            search_query: String::new(),
            searching: false,
            filtered_indices: Vec::new(),
            selected_namespace: None,
            namespaces: Vec::new(),
        }
    }

    pub fn set_entries(&mut self, entries: Vec<MemoryEntry>) {
        self.namespaces = entries
            .iter()
            .map(|e| e.namespace.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        self.entries = entries;
        self.apply_filter();
    }

    fn apply_filter(&mut self) {
        let query = self.search_query.to_lowercase();
        self.filtered_indices = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                let ns_match = self
                    .selected_namespace
                    .as_ref()
                    .map_or(true, |ns| e.namespace == *ns);
                let query_match = query.is_empty()
                    || e.key.to_lowercase().contains(&query)
                    || e.value_preview.to_lowercase().contains(&query);
                ns_match && query_match
            })
            .map(|(i, _)| i)
            .collect();

        if !self.filtered_indices.is_empty() {
            self.list_state.select(Some(0));
        } else {
            self.list_state.select(None);
        }
    }

    fn selected_entry(&self) -> Option<&MemoryEntry> {
        let idx = self.list_state.selected()?;
        let real_idx = *self.filtered_indices.get(idx)?;
        self.entries.get(real_idx)
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

impl Screen for MemoryScreen {
    fn name(&self) -> &str {
        "Memory"
    }

    fn render(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(20), Constraint::Min(40)])
            .split(area);

        // Namespace sidebar
        let ns_items: Vec<ListItem> = std::iter::once(ListItem::new(Line::from(vec![
            Span::styled(
                "  All",
                if self.selected_namespace.is_none() {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                },
            ),
        ])))
        .chain(self.namespaces.iter().map(|ns| {
            ListItem::new(Line::from(vec![Span::styled(
                format!("  {}", ns),
                if self.selected_namespace.as_ref() == Some(ns) {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                },
            )]))
        }))
        .collect();

        let ns_list = List::new(ns_items).block(
            Block::default()
                .title(" Namespaces ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
        );
        frame.render_widget(ns_list, chunks[0]);

        // Main area
        let right_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),  // search
                Constraint::Min(5),    // list
                Constraint::Length(6), // detail
            ])
            .split(chunks[1]);

        // Search
        let search_block = Block::default()
            .title(" Search Memory ")
            .borders(Borders::ALL)
            .border_style(if self.searching {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            });

        let search_text = if self.search_query.is_empty() && !self.searching {
            "  / to search..."
        } else {
            &format!("  🔍 {}", self.search_query)
        };

        frame.render_widget(
            Paragraph::new(search_text.to_string()).block(search_block),
            right_chunks[0],
        );

        // Entry list
        let items: Vec<ListItem> = self
            .filtered_indices
            .iter()
            .map(|&idx| {
                let e = &self.entries[idx];
                ListItem::new(vec![
                    Line::from(vec![
                        Span::styled(
                            format!("  {} ", e.key),
                            Style::default().fg(Color::White),
                        ),
                        Span::styled(
                            format!("[{}]", e.namespace),
                            Style::default().fg(Color::Cyan),
                        ),
                        Span::styled(
                            format!("  {} B", e.size_bytes),
                            Style::default().fg(Color::DarkGray),
                        ),
                    ]),
                    Line::from(vec![Span::styled(
                        format!(
                            "    {}",
                            if e.value_preview.len() > 60 {
                                let end = truncate_to_char_boundary(&e.value_preview, 60);
                                format!("{}…", &e.value_preview[..end])
                            } else {
                                e.value_preview.clone()
                            }
                        ),
                        Style::default().fg(Color::DarkGray),
                    )]),
                ])
            })
            .collect();

        let list = List::new(items)
            .block(
                Block::default()
                    .title(format!(
                        " Entries ({}/{}) ",
                        self.filtered_indices.len(),
                        self.entries.len()
                    ))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            );

        frame.render_stateful_widget(list, right_chunks[1], &mut self.list_state.clone());

        // Detail
        let detail_block = Block::default()
            .title(" Value Preview ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray));

        let detail_text = if let Some(entry) = self.selected_entry() {
            format!(
                "Key: {}\nNamespace: {}\nSize: {} bytes\nCreated: {}\n{}",
                entry.key,
                entry.namespace,
                entry.size_bytes,
                entry.created_at,
                entry.ttl
                    .as_ref()
                    .map(|t| format!("TTL: {}", t))
                    .unwrap_or_default()
            )
        } else {
            "No entry selected".to_string()
        };

        frame.render_widget(
            Paragraph::new(detail_text)
                .block(detail_block)
                .wrap(Wrap { trim: false }),
            right_chunks[2],
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
                KeyCode::Char('d') => {
                    if let Some(entry) = self.selected_entry() {
                        ScreenAction::Command(format!("memory:delete:{}", entry.key))
                    } else {
                        ScreenAction::None
                    }
                }
                KeyCode::Char('r') => ScreenAction::Command("memory:refresh".to_string()),
                KeyCode::Char('q') => ScreenAction::Quit,
                _ => ScreenAction::None,
            }
        }
    }

    fn handle_backend_event(&mut self, event: &crate::event::BackendEvent) {
        if let crate::event::BackendEvent::MemoryUpdate { count, .. } = event {
            // Trigger refresh if count changed
        }
    }
}
