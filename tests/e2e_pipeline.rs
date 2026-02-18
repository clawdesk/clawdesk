//! End-to-end integration tests for ClawDesk pipeline.
//!
//! Tests the complete flow: inbound message → processing → response,
//! using mock channels and providers.

mod harness;

use harness::mock_channel::MockChannel;
use harness::mock_provider::MockProvider;
use harness::fixtures::*;
use harness::helpers::*;

use std::time::Duration;

#[tokio::test]
async fn test_mock_channel_roundtrip() {
    let channel = MockChannel::new("telegram");
    let provider = MockProvider::new(MOCK_LLM_RESPONSE);

    // Simulate inbound message
    channel.inject_inbound(TEST_MSG_HELLO).await;
    let inbound = channel.drain_inbound().await;
    assert_eq!(inbound.len(), 1);
    assert_eq!(inbound[0], TEST_MSG_HELLO);

    // Simulate provider completion
    let response = provider.complete(&inbound[0]).await;
    assert_eq!(response, MOCK_LLM_RESPONSE);

    // Simulate outbound response
    channel.send(TEST_USER_ID, &response).await;

    channel.assert_sent_count(1).await;
    channel.assert_last_contains("help you further").await;
    provider.assert_request_count(1).await;
}

#[tokio::test]
async fn test_multi_channel_isolation() {
    let telegram = MockChannel::new("telegram");
    let discord = MockChannel::new("discord");

    telegram.send(TEST_USER_ID, "Hello from Telegram").await;
    discord.send(TEST_USER_ID, "Hello from Discord").await;

    telegram.assert_sent_count(1).await;
    discord.assert_sent_count(1).await;

    telegram.assert_last_contains("Telegram").await;
    discord.assert_last_contains("Discord").await;
}

#[tokio::test]
async fn test_provider_queued_responses_in_conversation() {
    let provider = MockProvider::new("fallback");
    provider.queue_responses(&[
        "Nice to meet you!",
        "I can help with that.",
        "Here's the answer: 42",
    ]).await;

    let r1 = provider.complete("Hi").await;
    let r2 = provider.complete("Can you help?").await;
    let r3 = provider.complete("What is the meaning of life?").await;
    let r4 = provider.complete("Another question").await;

    assert_eq!(r1, "Nice to meet you!");
    assert_eq!(r2, "I can help with that.");
    assert_eq!(r3, "Here's the answer: 42");
    assert_eq!(r4, "fallback"); // Queue exhausted, falls back
}

#[tokio::test]
async fn test_high_throughput_message_processing() {
    let channel = MockChannel::new("load-test");
    let provider = MockProvider::new("ok");
    let messages = generate_test_messages(100);
    let mut tracker = LatencyTracker::new();

    for msg in &messages {
        let start = std::time::Instant::now();

        channel.inject_inbound(msg).await;
        let inbound = channel.drain_inbound().await;
        let response = provider.complete(&inbound[0]).await;
        channel.send(TEST_USER_ID, &response).await;

        tracker.record(start.elapsed());
    }

    assert_eq!(channel.sent_count().await, 100);
    provider.assert_request_count(100).await;

    // All mock operations should be sub-millisecond
    tracker.assert_p95_under(Duration::from_millis(50));
}

#[tokio::test]
async fn test_session_key_generation() {
    let key = test_session_key("telegram", "12345");
    assert_eq!(key, "telegram:12345");
}

#[tokio::test]
async fn test_wait_for_condition() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let flag = Arc::new(AtomicBool::new(false));
    let flag_clone = flag.clone();

    // Set flag after 50ms
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        flag_clone.store(true, Ordering::SeqCst);
    });

    let result = wait_for(
        Duration::from_secs(1),
        Duration::from_millis(10),
        || {
            let f = flag.clone();
            async move { f.load(Ordering::SeqCst) }
        },
    ).await;

    assert!(result, "Condition should have become true");
}
