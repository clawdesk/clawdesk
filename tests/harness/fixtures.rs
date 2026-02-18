//! Test fixtures for consistent test data across integration tests.

/// Standard test user IDs.
pub const TEST_USER_ID: &str = "test-user-001";
pub const TEST_USER_ID_2: &str = "test-user-002";
pub const TEST_BOT_ID: &str = "bot-clawdesk-test";

/// Standard test channel identifiers.
pub const TEST_CHANNEL_TELEGRAM: &str = "telegram:12345";
pub const TEST_CHANNEL_DISCORD: &str = "discord:guild:channel";
pub const TEST_CHANNEL_SLACK: &str = "slack:workspace:channel";
pub const TEST_CHANNEL_WEBCHAT: &str = "webchat:session-abc";

/// Standard test message content.
pub const TEST_MSG_HELLO: &str = "Hello, ClawDesk!";
pub const TEST_MSG_SKILL: &str = "/weather London";
pub const TEST_MSG_LONG: &str = "This is a longer message that tests how the system handles multi-sentence input. It contains multiple clauses and should be processed correctly by the pipeline. The third sentence adds even more content for testing purposes.";

/// Generate a unique test session key.
pub fn test_session_key(channel: &str, id: &str) -> String {
    format!("{channel}:{id}")
}

/// Generate N test messages with sequential content.
pub fn generate_test_messages(count: usize) -> Vec<String> {
    (0..count)
        .map(|i| format!("Test message #{i}"))
        .collect()
}

/// Standard LLM response for testing.
pub const MOCK_LLM_RESPONSE: &str = "I understand your message. How can I help you further?";

/// Standard error response for testing error paths.
pub const MOCK_ERROR_RESPONSE: &str = "An error occurred while processing your request.";
