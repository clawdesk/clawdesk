//! Mock LLM provider for integration testing.
//!
//! Returns canned or scripted responses without making real API calls.
//! Useful for deterministic, fast integration tests.

use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::Mutex;

/// A recorded request to the mock provider.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub prompt: String,
    pub model: String,
    pub timestamp: std::time::Instant,
}

/// Mock LLM provider that returns pre-configured responses.
#[derive(Clone)]
pub struct MockProvider {
    responses: Arc<Mutex<VecDeque<String>>>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    default_response: String,
    model_name: String,
}

impl MockProvider {
    /// Create a new mock provider with a default response.
    pub fn new(default_response: &str) -> Self {
        Self {
            responses: Arc::new(Mutex::new(VecDeque::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
            default_response: default_response.to_string(),
            model_name: "mock-model".to_string(),
        }
    }

    /// Create with a specific model name.
    pub fn with_model(mut self, model: &str) -> Self {
        self.model_name = model.to_string();
        self
    }

    /// Queue a response to be returned for the next request.
    pub async fn queue_response(&self, response: &str) {
        self.responses.lock().await.push_back(response.to_string());
    }

    /// Queue multiple responses.
    pub async fn queue_responses(&self, responses: &[&str]) {
        let mut queue = self.responses.lock().await;
        for r in responses {
            queue.push_back(r.to_string());
        }
    }

    /// Simulate a completion request. Returns queued response or default.
    pub async fn complete(&self, prompt: &str) -> String {
        // Record the request
        self.requests.lock().await.push(RecordedRequest {
            prompt: prompt.to_string(),
            model: self.model_name.clone(),
            timestamp: std::time::Instant::now(),
        });

        // Return queued response or default
        self.responses
            .lock()
            .await
            .pop_front()
            .unwrap_or_else(|| self.default_response.clone())
    }

    /// Get all recorded requests.
    pub async fn recorded_requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().await.clone()
    }

    /// Get the number of requests made.
    pub async fn request_count(&self) -> usize {
        self.requests.lock().await.len()
    }

    /// Clear all state (queued responses + recorded requests).
    pub async fn reset(&self) {
        self.responses.lock().await.clear();
        self.requests.lock().await.clear();
    }

    /// Assert that N requests were made.
    pub async fn assert_request_count(&self, expected: usize) {
        let count = self.request_count().await;
        assert_eq!(
            count, expected,
            "Expected {expected} provider requests, got {count}"
        );
    }

    /// Assert that the last request prompt contains the given substring.
    pub async fn assert_last_prompt_contains(&self, substring: &str) {
        let requests = self.recorded_requests().await;
        let last = requests
            .last()
            .expect("No requests recorded");
        assert!(
            last.prompt.contains(substring),
            "Expected last prompt to contain '{substring}', got: {}",
            last.prompt
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_provider_default_response() {
        let provider = MockProvider::new("I am a mock bot");
        let resp = provider.complete("Hello").await;
        assert_eq!(resp, "I am a mock bot");
    }

    #[tokio::test]
    async fn test_mock_provider_queued_responses() {
        let provider = MockProvider::new("default");
        provider.queue_response("first").await;
        provider.queue_response("second").await;

        assert_eq!(provider.complete("q1").await, "first");
        assert_eq!(provider.complete("q2").await, "second");
        assert_eq!(provider.complete("q3").await, "default"); // Falls back to default
    }

    #[tokio::test]
    async fn test_mock_provider_records_requests() {
        let provider = MockProvider::new("ok");
        provider.complete("Hello world").await;
        provider.complete("How are you?").await;

        provider.assert_request_count(2).await;
        provider.assert_last_prompt_contains("How are you").await;
    }

    #[tokio::test]
    async fn test_mock_provider_reset() {
        let provider = MockProvider::new("ok");
        provider.queue_response("queued").await;
        provider.complete("test").await;
        provider.reset().await;

        assert_eq!(provider.request_count().await, 0);
        assert_eq!(provider.complete("test").await, "ok"); // Queue was cleared
    }
}
