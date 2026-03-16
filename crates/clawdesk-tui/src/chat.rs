//! Chat view — message display, input buffer, and streaming output.

use crate::btw_overlay::BtwInlineMessage;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::Instant;

/// Maximum number of messages retained in view.
const MAX_VISIBLE_MESSAGES: usize = 500;

/// Chat message role displayed in TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
}

impl Role {
    pub fn label(&self) -> &'static str {
        match self {
            Self::User => "You",
            Self::Assistant => "AI",
            Self::System => "System",
            Self::Tool => "Tool",
        }
    }

    pub fn color_index(&self) -> u8 {
        match self {
            Self::User => 4,    // blue
            Self::Assistant => 2, // green
            Self::System => 3,  // yellow
            Self::Tool => 5,    // magenta
        }
    }
}

/// A single chat message for display.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
    pub timestamp: String,
    pub model: Option<String>,
    pub tokens: Option<u32>,
    /// True while streaming is in progress.
    pub streaming: bool,
}

/// Chat view state.
pub struct ChatView {
    messages: VecDeque<ChatMessage>,
    input_buffer: String,
    cursor_pos: usize,
    scroll_offset: usize,
    active_stream: Option<StreamState>,
    /// Ephemeral BTW overlay (not persisted in message history).
    btw_message: Option<BtwInlineMessage>,
}

/// Active streaming state.
struct StreamState {
    started: Instant,
    chunks_received: usize,
    model: String,
}

impl ChatView {
    pub fn new() -> Self {
        Self {
            messages: VecDeque::with_capacity(MAX_VISIBLE_MESSAGES),
            input_buffer: String::new(),
            cursor_pos: 0,
            scroll_offset: 0,
            active_stream: None,
            btw_message: None,
        }
    }

    /// Add a complete message.
    pub fn push_message(&mut self, msg: ChatMessage) {
        if self.messages.len() >= MAX_VISIBLE_MESSAGES {
            self.messages.pop_front();
        }
        self.messages.push_back(msg);
        self.scroll_to_bottom();
    }

    /// Start streaming a new assistant message.
    pub fn start_stream(&mut self, model: &str) {
        let msg = ChatMessage {
            role: Role::Assistant,
            content: String::new(),
            timestamp: current_time_str(),
            model: Some(model.to_string()),
            tokens: None,
            streaming: true,
        };
        self.messages.push_back(msg);
        self.active_stream = Some(StreamState {
            started: Instant::now(),
            chunks_received: 0,
            model: model.to_string(),
        });
        self.scroll_to_bottom();
    }

    /// Append a chunk to the active stream.
    pub fn append_chunk(&mut self, text: &str) {
        if let Some(msg) = self.messages.back_mut() {
            if msg.streaming {
                msg.content.push_str(text);
            }
        }
        if let Some(stream) = &mut self.active_stream {
            stream.chunks_received += 1;
        }
    }

    /// Finish the active stream.
    pub fn finish_stream(&mut self, tokens: Option<u32>) {
        if let Some(msg) = self.messages.back_mut() {
            if msg.streaming {
                msg.streaming = false;
                msg.tokens = tokens;
            }
        }
        self.active_stream = None;
    }

    /// Get stream duration.
    pub fn stream_elapsed_ms(&self) -> Option<u128> {
        self.active_stream.as_ref().map(|s| s.started.elapsed().as_millis())
    }

    /// Insert character at cursor.
    pub fn insert_char(&mut self, ch: char) {
        self.input_buffer.insert(self.cursor_pos, ch);
        self.cursor_pos += ch.len_utf8();
    }

    /// Delete character before cursor.
    pub fn backspace(&mut self) {
        if self.cursor_pos > 0 {
            let prev = self.input_buffer[..self.cursor_pos]
                .chars()
                .last()
                .map(|c| c.len_utf8())
                .unwrap_or(1);
            self.cursor_pos -= prev;
            self.input_buffer.remove(self.cursor_pos);
        }
    }

    /// Move cursor left.
    pub fn cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            let prev = self.input_buffer[..self.cursor_pos]
                .chars()
                .last()
                .map(|c| c.len_utf8())
                .unwrap_or(1);
            self.cursor_pos -= prev;
        }
    }

    /// Move cursor right.
    pub fn cursor_right(&mut self) {
        if self.cursor_pos < self.input_buffer.len() {
            let next = self.input_buffer[self.cursor_pos..]
                .chars()
                .next()
                .map(|c| c.len_utf8())
                .unwrap_or(1);
            self.cursor_pos += next;
        }
    }

    /// Take the input buffer content (clears it).
    pub fn take_input(&mut self) -> String {
        self.cursor_pos = 0;
        std::mem::take(&mut self.input_buffer)
    }

    /// Get input buffer reference.
    pub fn input(&self) -> &str {
        &self.input_buffer
    }

    pub fn cursor_position(&self) -> usize {
        self.cursor_pos
    }

    /// Messages slice.
    pub fn messages(&self) -> &VecDeque<ChatMessage> {
        &self.messages
    }

    /// Visible messages for the current scroll position.
    pub fn visible_messages(&self, height: usize) -> impl Iterator<Item = &ChatMessage> {
        let start = self.messages.len().saturating_sub(height + self.scroll_offset);
        let end = self.messages.len().saturating_sub(self.scroll_offset);
        self.messages.range(start..end)
    }

    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll_offset = (self.scroll_offset + lines).min(self.messages.len());
    }

    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn is_streaming(&self) -> bool {
        self.active_stream.is_some()
    }

    pub fn message_count(&self) -> usize {
        self.messages.len()
    }

    /// Clear all messages.
    pub fn clear(&mut self) {
        self.messages.clear();
        self.scroll_offset = 0;
        self.active_stream = None;
        self.btw_message = None;
    }

    // ─────────────────────────────────────────────────────────────────────
    // BTW overlay methods
    // ─────────────────────────────────────────────────────────────────────

    /// Show a BTW side question overlay.
    pub fn show_btw(&mut self, question: &str) -> &mut BtwInlineMessage {
        self.btw_message = Some(BtwInlineMessage::new(question));
        self.btw_message.as_mut().unwrap()
    }

    /// Dismiss the BTW overlay.
    pub fn dismiss_btw(&mut self) {
        if let Some(btw) = &mut self.btw_message {
            btw.dismiss();
        }
        self.btw_message = None;
    }

    /// Whether a BTW overlay is currently visible.
    pub fn has_visible_btw(&self) -> bool {
        self.btw_message
            .as_ref()
            .map(|b| b.is_visible())
            .unwrap_or(false)
    }

    /// Get a mutable reference to the BTW overlay (for streaming updates).
    pub fn btw_message_mut(&mut self) -> Option<&mut BtwInlineMessage> {
        self.btw_message.as_mut()
    }

    /// Get the BTW overlay for rendering.
    pub fn btw_message(&self) -> Option<&BtwInlineMessage> {
        self.btw_message.as_ref()
    }
}

fn current_time_str() -> String {
    // Simple HH:MM format
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let hours = (now / 3600) % 24;
    let mins = (now / 60) % 60;
    format!("{:02}:{:02}", hours, mins)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_retrieve() {
        let mut chat = ChatView::new();
        chat.push_message(ChatMessage {
            role: Role::User,
            content: "Hello".into(),
            timestamp: "12:00".into(),
            model: None,
            tokens: None,
            streaming: false,
        });
        assert_eq!(chat.message_count(), 1);
        assert_eq!(chat.messages().back().unwrap().content, "Hello");
    }

    #[test]
    fn streaming_flow() {
        let mut chat = ChatView::new();
        chat.start_stream("gpt-4");
        assert!(chat.is_streaming());

        chat.append_chunk("Hello ");
        chat.append_chunk("world");
        assert_eq!(chat.messages().back().unwrap().content, "Hello world");

        chat.finish_stream(Some(10));
        assert!(!chat.is_streaming());
        assert_eq!(chat.messages().back().unwrap().tokens, Some(10));
    }

    #[test]
    fn input_editing() {
        let mut chat = ChatView::new();
        chat.insert_char('H');
        chat.insert_char('i');
        assert_eq!(chat.input(), "Hi");

        chat.backspace();
        assert_eq!(chat.input(), "H");

        let taken = chat.take_input();
        assert_eq!(taken, "H");
        assert_eq!(chat.input(), "");
    }

    #[test]
    fn role_labels() {
        assert_eq!(Role::User.label(), "You");
        assert_eq!(Role::Assistant.label(), "AI");
    }

    #[test]
    fn clear_messages() {
        let mut chat = ChatView::new();
        chat.push_message(ChatMessage {
            role: Role::User,
            content: "test".into(),
            timestamp: "00:00".into(),
            model: None,
            tokens: None,
            streaming: false,
        });
        chat.clear();
        assert_eq!(chat.message_count(), 0);
    }
}
