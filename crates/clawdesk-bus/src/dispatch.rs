//! Central event dispatcher — routes events from topics through subscriptions to pipelines.
//!
//! The dispatcher owns all topics and subscriptions, providing the single
//! entry point for event publishing and the reactive trigger mechanism
//! that converts events into pipeline executions.

use crate::event::{Event, EventKind, Priority};
use crate::priority::WfqScheduler;
use crate::subscription::Subscription;
use crate::topic::{Topic, TopicConfig};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Central event bus dispatcher.
///
/// Manages topics, subscriptions, and the WFQ priority dispatch loop.
pub struct EventBus {
    /// Topic name → Topic ring buffer
    topics: RwLock<HashMap<String, Arc<Topic>>>,
    /// Active subscriptions
    subscriptions: RwLock<Vec<Subscription>>,
    /// Default topic capacity
    default_capacity: usize,
}

impl EventBus {
    /// Create a new event bus.
    pub fn new(default_capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            topics: RwLock::new(HashMap::new()),
            subscriptions: RwLock::new(Vec::new()),
            default_capacity,
        })
    }

    /// Create or retrieve a topic by name.
    pub async fn topic(&self, name: &str) -> Arc<Topic> {
        {
            let topics = self.topics.read().await;
            if let Some(t) = topics.get(name) {
                return t.clone();
            }
        }
        let mut topics = self.topics.write().await;
        topics
            .entry(name.to_string())
            .or_insert_with(|| {
                info!(topic = name, "Creating new event bus topic");
                Topic::new(TopicConfig {
                    name: name.to_string(),
                    capacity: self.default_capacity,
                    persistent: true,
                })
            })
            .clone()
    }

    /// Publish an event to its topic and return matching subscription IDs.
    ///
    /// The event is written to the topic ring buffer, then all subscriptions
    /// are evaluated. Matching subscription pipeline IDs are returned for
    /// the caller (typically the gateway) to trigger pipeline execution.
    pub async fn publish(&self, event: Event) -> Vec<String> {
        let topic_name = event.topic.clone();
        let kind = event.kind.clone();
        let priority = event.priority;

        // Write to topic
        let topic = self.topic(&topic_name).await;
        let offset = topic.publish(event).await;
        debug!(topic = %topic_name, offset, "Event published");

        // Find matching subscriptions
        let subs = self.subscriptions.read().await;
        let matches: Vec<String> = subs
            .iter()
            .filter(|s| s.matches(&topic_name, &kind, priority))
            .map(|s| s.pipeline_id.clone())
            .collect();

        if !matches.is_empty() {
            debug!(topic = %topic_name, matched = matches.len(), "Subscriptions triggered");
        }

        matches
    }

    /// Convenience: publish and return the offset.
    pub async fn emit(
        &self,
        topic: impl Into<String>,
        kind: EventKind,
        priority: Priority,
        payload: serde_json::Value,
        source: impl Into<String>,
    ) -> u64 {
        let event = Event::new(topic, kind, priority, payload, source);
        let t = self.topic(&event.topic).await;
        t.publish(event).await
    }

    /// Register a subscription.
    pub async fn subscribe(&self, sub: Subscription) {
        info!(id = %sub.id, name = %sub.name, "Registering event subscription");
        let mut subs = self.subscriptions.write().await;
        subs.push(sub);
    }

    /// Remove a subscription by ID.
    pub async fn unsubscribe(&self, sub_id: &str) {
        let mut subs = self.subscriptions.write().await;
        subs.retain(|s| s.id != sub_id);
    }

    /// List all registered topics.
    pub async fn list_topics(&self) -> Vec<String> {
        self.topics.read().await.keys().cloned().collect()
    }

    /// List all subscriptions.
    pub async fn list_subscriptions(&self) -> Vec<Subscription> {
        self.subscriptions.read().await.clone()
    }

    /// Get aggregate stats for monitoring.
    pub async fn stats(&self) -> BusStats {
        let topics = self.topics.read().await;
        let mut topic_stats = Vec::new();
        for (name, topic) in topics.iter() {
            topic_stats.push(TopicStats {
                name: name.clone(),
                head_offset: topic.head_offset(),
                buffered: topic.buffered_count().await,
            });
        }
        let subs = self.subscriptions.read().await;
        BusStats {
            topic_count: topic_stats.len(),
            subscription_count: subs.len(),
            topics: topic_stats,
        }
    }
}

/// Aggregate bus statistics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct BusStats {
    pub topic_count: usize,
    pub subscription_count: usize,
    pub topics: Vec<TopicStats>,
}

/// Per-topic statistics.
#[derive(Debug, Clone, serde::Serialize)]
pub struct TopicStats {
    pub name: String,
    pub head_offset: u64,
    pub buffered: usize,
}
