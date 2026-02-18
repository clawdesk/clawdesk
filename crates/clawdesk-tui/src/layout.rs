//! Layout engine — responsive terminal layout with split panes.

use serde::{Deserialize, Serialize};

/// Terminal area (row, col, width, height).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
}

impl Rect {
    pub fn new(x: u16, y: u16, width: u16, height: u16) -> Self {
        Self { x, y, width, height }
    }

    pub fn area(&self) -> u32 {
        self.width as u32 * self.height as u32
    }

    /// Split horizontally, returning (top, bottom) with given top height.
    pub fn split_horizontal(&self, top_height: u16) -> (Rect, Rect) {
        let clamped = top_height.min(self.height);
        (
            Rect::new(self.x, self.y, self.width, clamped),
            Rect::new(self.x, self.y + clamped, self.width, self.height - clamped),
        )
    }

    /// Split vertically, returning (left, right) with given left width.
    pub fn split_vertical(&self, left_width: u16) -> (Rect, Rect) {
        let clamped = left_width.min(self.width);
        (
            Rect::new(self.x, self.y, clamped, self.height),
            Rect::new(self.x + clamped, self.y, self.width - clamped, self.height),
        )
    }
}

/// Application layout mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayoutMode {
    /// Full-width chat.
    Chat,
    /// Chat + sidebar (model list, status).
    ChatWithSidebar,
    /// Dashboard view (multiple panels).
    Dashboard,
}

/// Computed layout regions.
#[derive(Debug, Clone)]
pub struct AppLayout {
    pub mode: LayoutMode,
    pub chat_area: Rect,
    pub input_area: Rect,
    pub status_area: Rect,
    pub sidebar_area: Option<Rect>,
}

impl AppLayout {
    /// Compute layout for the given terminal size.
    pub fn compute(width: u16, height: u16, mode: LayoutMode) -> Self {
        let full = Rect::new(0, 0, width, height);

        // Status bar is always 1 row at bottom
        let (main, status_area) = full.split_horizontal(height.saturating_sub(1));

        // Input area is 3 rows above status
        let (upper, input_area) = main.split_horizontal(main.height.saturating_sub(3));

        match mode {
            LayoutMode::Chat => AppLayout {
                mode,
                chat_area: upper,
                input_area,
                status_area,
                sidebar_area: None,
            },
            LayoutMode::ChatWithSidebar => {
                let sidebar_width = (width / 4).max(20).min(40);
                let (chat, sidebar) = upper.split_vertical(width - sidebar_width);
                AppLayout {
                    mode,
                    chat_area: chat,
                    input_area,
                    status_area,
                    sidebar_area: Some(sidebar),
                }
            }
            LayoutMode::Dashboard => {
                // Dashboard: top half is chat, bottom half is panels
                let (chat, panels) = upper.split_horizontal(upper.height / 2);
                AppLayout {
                    mode,
                    chat_area: chat,
                    input_area: panels, // Reuse as dashboard panels
                    status_area,
                    sidebar_area: None,
                }
            }
        }
    }

    /// Minimum terminal size for usable display.
    pub fn minimum_size() -> (u16, u16) {
        (40, 10)
    }

    /// Check if terminal is too small.
    pub fn is_usable(width: u16, height: u16) -> bool {
        let (min_w, min_h) = Self::minimum_size();
        width >= min_w && height >= min_h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_split_horizontal() {
        let r = Rect::new(0, 0, 80, 24);
        let (top, bottom) = r.split_horizontal(10);
        assert_eq!(top.height, 10);
        assert_eq!(bottom.height, 14);
        assert_eq!(bottom.y, 10);
    }

    #[test]
    fn rect_split_vertical() {
        let r = Rect::new(0, 0, 100, 30);
        let (left, right) = r.split_vertical(30);
        assert_eq!(left.width, 30);
        assert_eq!(right.width, 70);
        assert_eq!(right.x, 30);
    }

    #[test]
    fn layout_chat_mode() {
        let layout = AppLayout::compute(80, 24, LayoutMode::Chat);
        assert_eq!(layout.status_area.height, 1);
        assert!(layout.sidebar_area.is_none());
    }

    #[test]
    fn layout_with_sidebar() {
        let layout = AppLayout::compute(120, 40, LayoutMode::ChatWithSidebar);
        assert!(layout.sidebar_area.is_some());
        let sidebar = layout.sidebar_area.unwrap();
        assert!(sidebar.width >= 20);
    }

    #[test]
    fn minimum_size_check() {
        assert!(AppLayout::is_usable(80, 24));
        assert!(!AppLayout::is_usable(10, 5));
    }
}
