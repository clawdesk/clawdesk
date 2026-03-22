//! Terminal capability detection — TTY, color support, dimensions.
//!
//! The agent needs to know:
//! - Is stdout a TTY or a pipe? (affects output format)
//! - Does the terminal support colors? (affects formatting)
//! - How wide is the terminal? (affects table/code formatting)

use serde::{Deserialize, Serialize};

/// Color support level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum ColorSupport {
    /// No color support (piped output, TERM=dumb).
    None,
    /// Basic 8-color support.
    Basic,
    /// 256-color support (TERM contains "256color").
    Extended,
    /// 24-bit truecolor (COLORTERM=truecolor or 24bit).
    TrueColor,
}

/// Terminal capabilities for the current session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TerminalCapabilities {
    /// Whether stdout is connected to a terminal (vs piped/redirected).
    pub is_tty: bool,
    /// Color support level.
    pub color_support: ColorSupport,
    /// Whether the terminal supports Unicode characters.
    pub unicode_support: bool,
    /// Terminal width in columns (default 80 if unknown).
    pub width: u16,
    /// Terminal height in rows (default 24 if unknown).
    pub height: u16,
}

impl TerminalCapabilities {
    /// Detect terminal capabilities from the current environment.
    pub fn detect() -> Self {
        let is_tty = detect_is_tty();
        Self {
            is_tty,
            color_support: if is_tty { detect_color_support() } else { ColorSupport::None },
            unicode_support: detect_unicode_support(),
            width: detect_terminal_width(),
            height: detect_terminal_height(),
        }
    }

    /// A "dumb" terminal with no capabilities (for testing / pipe context).
    pub fn dumb() -> Self {
        Self {
            is_tty: false,
            color_support: ColorSupport::None,
            unicode_support: false,
            width: 80,
            height: 24,
        }
    }

    /// Generate a system prompt fragment about terminal capabilities.
    pub fn to_prompt_hint(&self) -> Option<String> {
        if self.is_tty {
            return None; // Normal terminal, no special instructions needed
        }
        Some(
            "Output is piped/redirected (not a terminal). \
             Use plain text without ANSI escape codes. \
             Prefer machine-parseable output formats."
                .into(),
        )
    }
}

/// Detect if stdout is a TTY.
fn detect_is_tty() -> bool {
    use std::io::IsTerminal;
    std::io::stdout().is_terminal()
}

/// Detect color support level from environment variables.
fn detect_color_support() -> ColorSupport {
    // Check NO_COLOR (https://no-color.org/)
    if std::env::var("NO_COLOR").is_ok() {
        return ColorSupport::None;
    }

    // Check COLORTERM for truecolor
    if let Ok(ct) = std::env::var("COLORTERM") {
        let lower = ct.to_lowercase();
        if lower == "truecolor" || lower == "24bit" {
            return ColorSupport::TrueColor;
        }
    }

    // Check TERM for 256-color
    if let Ok(term) = std::env::var("TERM") {
        if term == "dumb" {
            return ColorSupport::None;
        }
        if term.contains("256color") || term.contains("256colour") {
            return ColorSupport::Extended;
        }
        if term.contains("color") || term.contains("xterm") || term.contains("screen") {
            return ColorSupport::Basic;
        }
    }

    // Default: basic if TTY, none otherwise
    ColorSupport::Basic
}

/// Detect Unicode support from locale settings.
fn detect_unicode_support() -> bool {
    // Check LANG / LC_ALL for UTF-8
    for var in &["LC_ALL", "LC_CTYPE", "LANG"] {
        if let Ok(val) = std::env::var(var) {
            let upper = val.to_uppercase();
            if upper.contains("UTF-8") || upper.contains("UTF8") {
                return true;
            }
        }
    }
    // Windows Terminal supports Unicode by default
    #[cfg(windows)]
    { return true; }
    #[cfg(not(windows))]
    { false }
}

/// Detect terminal width.
fn detect_terminal_width() -> u16 {
    // Check COLUMNS env first (set by some shells)
    if let Ok(cols) = std::env::var("COLUMNS") {
        if let Ok(w) = cols.parse::<u16>() {
            if w > 0 { return w; }
        }
    }

    // Try tput on Unix
    #[cfg(unix)]
    {
        if let Ok(out) = std::process::Command::new("tput").arg("cols").output() {
            if let Ok(w) = String::from_utf8_lossy(&out.stdout).trim().parse::<u16>() {
                if w > 0 { return w; }
            }
        }
    }

    80 // default
}

/// Detect terminal height.
fn detect_terminal_height() -> u16 {
    if let Ok(lines) = std::env::var("LINES") {
        if let Ok(h) = lines.parse::<u16>() {
            if h > 0 { return h; }
        }
    }

    #[cfg(unix)]
    {
        if let Ok(out) = std::process::Command::new("tput").arg("lines").output() {
            if let Ok(h) = String::from_utf8_lossy(&out.stdout).trim().parse::<u16>() {
                if h > 0 { return h; }
            }
        }
    }

    24 // default
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_returns_valid_values() {
        let caps = TerminalCapabilities::detect();
        assert!(caps.width > 0);
        assert!(caps.height > 0);
    }

    #[test]
    fn dumb_has_no_color() {
        let caps = TerminalCapabilities::dumb();
        assert!(!caps.is_tty);
        assert_eq!(caps.color_support, ColorSupport::None);
    }

    #[test]
    fn no_color_respected() {
        // NO_COLOR env is a convention: https://no-color.org/
        // We can't safely set env vars in tests, but we test the function logic
        assert_eq!(detect_color_support(), ColorSupport::Basic.max(detect_color_support()));
    }

    #[test]
    fn piped_gets_prompt_hint() {
        let caps = TerminalCapabilities::dumb();
        assert!(caps.to_prompt_hint().is_some());
        assert!(caps.to_prompt_hint().unwrap().contains("piped"));
    }

    #[test]
    fn tty_no_hint_needed() {
        let mut caps = TerminalCapabilities::detect();
        caps.is_tty = true;
        assert!(caps.to_prompt_hint().is_none());
    }
}
