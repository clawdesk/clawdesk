//! Theming — color schemes for terminal UI.

use serde::{Deserialize, Serialize};

/// Color represented as RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Color {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Convert to ANSI 256-color index (approximate).
    pub fn to_ansi256(&self) -> u8 {
        if self.r == self.g && self.g == self.b {
            // Grayscale
            if self.r < 8 {
                return 16;
            }
            if self.r > 248 {
                return 231;
            }
            return ((self.r as u16 - 8) / 10 + 232) as u8;
        }

        let r = (self.r as u16 * 5 / 255) as u8;
        let g = (self.g as u16 * 5 / 255) as u8;
        let b = (self.b as u16 * 5 / 255) as u8;
        16 + 36 * r + 6 * g + b
    }

    /// ANSI escape for foreground.
    pub fn fg_escape(&self) -> String {
        format!("\x1b[38;2;{};{};{}m", self.r, self.g, self.b)
    }

    /// ANSI escape for background.
    pub fn bg_escape(&self) -> String {
        format!("\x1b[48;2;{};{};{}m", self.r, self.g, self.b)
    }
}

/// Theme configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    pub bg: Color,
    pub fg: Color,
    pub accent: Color,
    pub user_color: Color,
    pub assistant_color: Color,
    pub system_color: Color,
    pub error_color: Color,
    pub border_color: Color,
    pub dim_color: Color,
    pub status_bg: Color,
}

impl Theme {
    /// Dark theme (default).
    pub fn dark() -> Self {
        Self {
            name: "dark".to_string(),
            bg: Color::rgb(30, 30, 46),
            fg: Color::rgb(205, 214, 244),
            accent: Color::rgb(137, 180, 250),
            user_color: Color::rgb(116, 199, 236),
            assistant_color: Color::rgb(166, 227, 161),
            system_color: Color::rgb(249, 226, 175),
            error_color: Color::rgb(243, 139, 168),
            border_color: Color::rgb(88, 91, 112),
            dim_color: Color::rgb(127, 132, 156),
            status_bg: Color::rgb(24, 24, 37),
        }
    }

    /// Light theme.
    pub fn light() -> Self {
        Self {
            name: "light".to_string(),
            bg: Color::rgb(239, 241, 245),
            fg: Color::rgb(76, 79, 105),
            accent: Color::rgb(30, 102, 245),
            user_color: Color::rgb(4, 165, 229),
            assistant_color: Color::rgb(64, 160, 43),
            system_color: Color::rgb(223, 142, 29),
            error_color: Color::rgb(210, 15, 57),
            border_color: Color::rgb(172, 176, 190),
            dim_color: Color::rgb(140, 143, 161),
            status_bg: Color::rgb(220, 224, 232),
        }
    }

    /// High contrast theme.
    pub fn high_contrast() -> Self {
        Self {
            name: "high-contrast".to_string(),
            bg: Color::rgb(0, 0, 0),
            fg: Color::rgb(255, 255, 255),
            accent: Color::rgb(0, 200, 255),
            user_color: Color::rgb(100, 200, 255),
            assistant_color: Color::rgb(0, 255, 0),
            system_color: Color::rgb(255, 255, 0),
            error_color: Color::rgb(255, 50, 50),
            border_color: Color::rgb(128, 128, 128),
            dim_color: Color::rgb(160, 160, 160),
            status_bg: Color::rgb(20, 20, 20),
        }
    }

    /// Get theme by name.
    pub fn by_name(name: &str) -> Self {
        match name {
            "light" => Self::light(),
            "high-contrast" | "hc" => Self::high_contrast(),
            _ => Self::dark(),
        }
    }

    /// Available theme names.
    pub fn available() -> &'static [&'static str] {
        &["dark", "light", "high-contrast"]
    }

    /// ANSI reset sequence.
    pub fn reset() -> &'static str {
        "\x1b[0m"
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dark_theme_default() {
        let t = Theme::default();
        assert_eq!(t.name, "dark");
    }

    #[test]
    fn theme_by_name() {
        let t = Theme::by_name("light");
        assert_eq!(t.name, "light");
        let t = Theme::by_name("unknown");
        assert_eq!(t.name, "dark");
    }

    #[test]
    fn color_ansi_escape() {
        let c = Color::rgb(255, 0, 0);
        assert!(c.fg_escape().contains("255;0;0"));
    }

    #[test]
    fn available_themes() {
        assert!(Theme::available().contains(&"dark"));
        assert!(Theme::available().contains(&"light"));
    }
}
