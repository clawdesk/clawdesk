//! Mock channel adapter for integration testing.
//!
//! Records all sent messages and allows injecting inbound messages
//! for pipeline testing without real network I/O.

use std::sync::Arc;
use tokio::sync::Mutex;

/// A message captured by the mock channel.
#[derive(Debug, Clone)]
pub struct CapturedMessage {
    pub channel: String,
    pub recipient: String,
    pub text: String,
    pub timestamp: std::time::Instant,
}

/// Mock channel that captures outbound messages and provides inbound message injection.
#[derive(Clone)]
pub struct MockChannel {
    pub name: String,
    sent: Arc<Mutex<Vec<CapturedMessage>>>,
    inbound_queue: Arc<Mutex<Vec<String>>>,
}

impl MockChannel {
    /// Create a new mock channel with the given name.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            sent: Arc::new(Mutex::new(Vec::new())),
            inbound_queue: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Simulate sending a message (captures it for assertions).
    pub async fn send(&self, recipient: &str, text: &str) {
        let msg = CapturedMessage {
            channel: self.name.clone(),
            recipient: recipient.to_string(),
            text: text.to_string(),
            timestamp: std::time::Instant::now(),
        };
        self.sent.lock().await.push(msg);
    }

    /// Get all captured sent messages.
    pub async fn sent_messages(&self) -> Vec<CapturedMessage> {
        self.sent.lock().await.clone()
    }

    /// Get the last sent message text, if any.
    pub async fn last_sent_text(&self) -> Option<String> {
        self.sent.lock().await.last().map(|m| m.text.clone())
    }

    /// Clear all captured messages.
    pub async fn clear(&self) {
        self.sent.lock().await.clear();
    }

    /// Inject an inbound message into the queue.
    pub async fn inject_inbound(&self, text: &str) {
        self.inbound_queue.lock().await.push(text.to_string());
    }

    /// Drain all inbound messages.
    pub async fn drain_inbound(&self) -> Vec<String> {
        let mut queue = self.inbound_queue.lock().await;
        queue.drain(..).collect()
    }

    /// Number of sent messages.
    pub async fn sent_count(&self) -> usize {
        self.sent.lock().await.len()
    }

    /// Assert that exactly N messages were sent.
    pub async fn assert_sent_count(&self, expected: usize) {
        let count = self.sent_count().await;
        assert_eq!(
            count, expected,
            "Expected {expected} sent messages on channel '{}', got {count}",
            self.name
        );
    }

    /// Assert that the last message contains the given substring.
    pub async fn assert_last_contains(&self, substring: &str) {
        let last = self
            .last_sent_text()
            .await
            .unwrap_or_else(|| panic!("No messages sent on channel '{}'", self.name));
        assert!(
            last.contains(substring),
            "Expected last message on '{}' to contain '{substring}', got: {last}",
            self.name
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_channel_send_and_capture() {
        let ch = MockChannel::new("test");
        ch.send("user1", "Hello!").await;
        ch.send("user2", "Goodbye!").await;

        assert_eq!(ch.sent_count().await, 2);
        let msgs = ch.sent_messages().await;
        assert_eq!(msgs[0].text, "Hello!");
        assert_eq!(msgs[1].recipient, "user2");
    }

    #[tokio::test]
    async fn test_mock_channel_clear() {
        let ch = MockChannel::new("test");
        ch.send("user1", "msg").await;
        ch.clear().await;
        assert_eq!(ch.sent_count().await, 0);
    }

    #[tokio::test]
    async fn test_mock_channel_inbound() {
        let ch = MockChannel::new("test");
        ch.inject_inbound("Hi bot").await;
        ch.inject_inbound("Do something").await;
        let msgs = ch.drain_inbound().await;
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0], "Hi bot");
    }

    #[tokio::test]
    async fn test_mock_channel_assert_helpers() {
        let ch = MockChannel::new("test");
        ch.send("user1", "The answer is 42").await;
        ch.assert_sent_count(1).await;
        ch.assert_last_contains("42").await;
    }
}
