//! # BTW Inline Message — Ephemeral side-question overlay for TUI.
//!
//! Renders a dismissible popup showing the `/btw` question and AI answer.
//! Does not persist in the main chat log.

use serde::{Deserialize, Serialize};

/// State for a BTW inline message displayed as an overlay in the TUI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BtwInlineMessage {
    /// The original question.
    pub question: String,
    /// The AI's answer (may be incrementally built during streaming).
    pub answer: String,
    /// Whether this is an error response.
    pub is_error: bool,
    /// Whether the message is currently visible.
    pub visible: bool,
}

impl BtwInlineMessage {
    /// Create a new BTW overlay with the given question.
    pub fn new(question: &str) -> Self {
        Self {
            question: question.to_string(),
            answer: String::new(),
            is_error: false,
            visible: true,
        }
    }

    /// Set the answer text (replaces any previous text).
    pub fn set_answer(&mut self, answer: &str) {
        self.answer = answer.to_string();
    }

    /// Append to the answer text (for streaming).
    pub fn append_answer(&mut self, chunk: &str) {
        self.answer.push_str(chunk);
    }

    /// Mark as error.
    pub fn set_error(&mut self, error: &str) {
        self.answer = error.to_string();
        self.is_error = true;
    }

    /// Dismiss the overlay.
    pub fn dismiss(&mut self) {
        self.visible = false;
    }

    /// Whether the overlay should be rendered.
    pub fn is_visible(&self) -> bool {
        self.visible
    }

    /// Format for TUI rendering.
    ///
    /// Returns lines suitable for display in a bordered popup:
    /// ```text
    /// ╭─ BTW ──────────────────────╮
    /// │ Q: what does this func do? │
    /// │                            │
    /// │ It calculates the hash...  │
    /// │                            │
    /// │ [Enter/Esc to dismiss]     │
    /// ╰────────────────────────────╯
    /// ```
    pub fn render_lines(&self, max_width: usize) -> Vec<String> {
        let mut lines = Vec::new();
        let inner_width = max_width.saturating_sub(4); // borders + padding

        // Question line.
        let q_label = if self.is_error { "ERR" } else { "Q" };
        let q_line = format!("{q_label}: {}", self.question);
        for chunk in wrap_text(&q_line, inner_width) {
            lines.push(chunk);
        }

        if !self.answer.is_empty() {
            lines.push(String::new()); // blank separator
            for chunk in wrap_text(&self.answer, inner_width) {
                lines.push(chunk);
            }
        }

        lines.push(String::new());
        lines.push("[Enter/Esc to dismiss]".to_string());

        lines
    }
}

/// Simple word-aware text wrapping.
fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![text.to_string()];
    }

    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            lines.push(String::new());
            continue;
        }

        let mut current_line = String::new();
        for word in paragraph.split_whitespace() {
            if current_line.is_empty() {
                current_line = word.to_string();
            } else if current_line.len() + 1 + word.len() <= max_width {
                current_line.push(' ');
                current_line.push_str(word);
            } else {
                lines.push(current_line);
                current_line = word.to_string();
            }
        }
        if !current_line.is_empty() {
            lines.push(current_line);
        }
    }

    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_set_answer() {
        let mut btw = BtwInlineMessage::new("what is this?");
        assert!(btw.is_visible());
        assert_eq!(btw.question, "what is this?");
        assert!(btw.answer.is_empty());

        btw.set_answer("It's a hash function.");
        assert_eq!(btw.answer, "It's a hash function.");
        assert!(!btw.is_error);
    }

    #[test]
    fn append_streaming() {
        let mut btw = BtwInlineMessage::new("q");
        btw.append_answer("Hello ");
        btw.append_answer("world");
        assert_eq!(btw.answer, "Hello world");
    }

    #[test]
    fn dismiss_hides() {
        let mut btw = BtwInlineMessage::new("q");
        assert!(btw.is_visible());
        btw.dismiss();
        assert!(!btw.is_visible());
    }

    #[test]
    fn error_state() {
        let mut btw = BtwInlineMessage::new("q");
        btw.set_error("LLM timeout");
        assert!(btw.is_error);
        assert_eq!(btw.answer, "LLM timeout");
    }

    #[test]
    fn render_lines_has_dismiss_hint() {
        let mut btw = BtwInlineMessage::new("what?");
        btw.set_answer("something");
        let lines = btw.render_lines(60);
        assert!(lines.last().unwrap().contains("dismiss"));
    }

    #[test]
    fn wrap_text_splits_long_lines() {
        let lines = wrap_text("hello world foo bar baz", 12);
        assert!(lines.len() >= 2);
    }
}
