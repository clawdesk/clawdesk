//! Central event dispatcher — routes events from topics through subscriptions to pipelines.
//!
//! The dispatcher owns all topics and subscriptions, providing the single
//! entry point for event publishing and the reactive trigger mechanism
//! that converts events into pipeline executions.

use crate::event::{Event, EventKind, Priority};
use crate::priority::ShardedWfqScheduler;
use crate::subscription::Subscription;
use crate::topic::{Topic, TopicConfig};
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Central event bus dispatcher.
///
/// Manages topics, subscriptions, and the WFQ priority dispatch loop.
///
/// Subscriptions are indexed by topic for O(1) lookup. Subscriptions
/// with exact topic patterns go into `topic_subs`; those with glob wildcards
/// go into `glob_subs` (typically < 5, scanned on every publish).
///
/// Integrated WFQ scheduler for priority-weighted dispatch ordering.
/// Events enqueued via `publish()` are available for weighted-fair dequeue
/// through `drain_prioritized()`.
pub struct EventBus {
    /// Topic name → Topic ring buffer
    topics: DashMap<String, Arc<Topic>>,
    /// Topic-indexed subscriptions: topic_name → Vec<Subscription>
    /// Only subscriptions with exact (non-glob) topic patterns.
    topic_subs: DashMap<String, Vec<Subscription>>,
    /// Subscriptions with glob patterns (e.g., "email.*") — scanned on every publish.
    glob_subs: RwLock<Vec<Subscription>>,
    /// Sharded WFQ scheduler for priority-ordered dispatch.
    /// K=3 independent heaps (one per priority class) to reduce enqueue contention.
    wfq: ShardedWfqScheduler<DispatchItem>,
    /// Monotonic virtual clock for WFQ arrival timestamps.
    wfq_clock: std::sync::atomic::AtomicU64,
    /// Default topic capacity
    default_capacity: usize,
}

/// Item enqueued into the WFQ scheduler for priority dispatch.
#[derive(Debug, Clone)]
pub struct DispatchItem {
    /// Topic the event was published to.
    pub topic: String,
    /// Offset within the topic ring buffer.
    pub offset: u64,
    /// Pipeline IDs that matched the event.
    pub pipeline_ids: Vec<String>,
    /// Original event priority.
    pub priority: Priority,
}

impl EventBus {
    /// Create a new event bus.
    pub fn new(default_capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            topics: DashMap::new(),
            topic_subs: DashMap::new(),
            glob_subs: RwLock::new(Vec::new()),
            wfq: ShardedWfqScheduler::new(),
            wfq_clock: std::sync::atomic::AtomicU64::new(0),
            default_capacity,
        })
    }

    /// Create or retrieve a topic by name.
    pub async fn topic(&self, name: &str) -> Arc<Topic> {
        if let Some(t) = self.topics.get(name) {
            return t.clone();
        }
        self.topics
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
    /// O(1) topic-indexed lookup for exact subscriptions + O(G) scan
    /// for glob subscriptions where G ≪ S. Total: O(K + G) instead of O(S).
    pub async fn publish(&self, event: Event) -> Vec<String> {
        let topic_name = event.topic.clone();
        let kind = event.kind.clone();
        let priority = event.priority;

        // Write to topic
        let topic = self.topic(&topic_name).await;
        let offset = topic.publish(event).await;
        debug!(topic = %topic_name, offset, "Event published");

        let mut matches: Vec<String> = Vec::new();

        // O(1) lookup: exact topic subscriptions
        if let Some(subs) = self.topic_subs.get(&topic_name) {
            for s in subs.value() {
                if s.matches(&topic_name, &kind, priority) {
                    matches.push(s.pipeline_id.clone());
                }
            }
        }

        // O(G) scan: glob subscriptions (typically < 5)
        {
            let gsubs = self.glob_subs.read().await;
            for s in gsubs.iter() {
                if s.matches(&topic_name, &kind, priority) {
                    matches.push(s.pipeline_id.clone());
                }
            }
        }

        if !matches.is_empty() {
            debug!(topic = %topic_name, matched = matches.len(), "Subscriptions triggered");

            // Enqueue into WFQ for priority-weighted dispatch ordering.
            let arrival = self.wfq_clock.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as f64;
            let item = DispatchItem {
                topic: topic_name.clone(),
                offset,
                pipeline_ids: matches.clone(),
                priority,
            };
            self.wfq.enqueue(item, priority, arrival).await;
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
    ///
    /// Routes to topic_subs (indexed) or glob_subs based on pattern.
    pub async fn subscribe(&self, sub: Subscription) {
        info!(id = %sub.id, name = %sub.name, "Registering event subscription");
        let has_glob = sub.topic_patterns.iter().any(|p| p.contains('*'));
        if has_glob {
            let mut gsubs = self.glob_subs.write().await;
            gsubs.push(sub);
        } else {
            for pat in &sub.topic_patterns {
                self.topic_subs.entry(pat.clone()).or_default().push(sub.clone());
            }
        }
    }

    /// Remove a subscription by ID.
    pub async fn unsubscribe(&self, sub_id: &str) {
        for mut entry in self.topic_subs.iter_mut() {
            entry.value_mut().retain(|s| s.id != sub_id);
        }
        {
            let mut gsubs = self.glob_subs.write().await;
            gsubs.retain(|s| s.id != sub_id);
        }
    }

    /// List all registered topics.
    pub async fn list_topics(&self) -> Vec<String> {
        self.topics.iter().map(|e| e.key().clone()).collect()
    }

    /// List all subscriptions.
    pub async fn list_subscriptions(&self) -> Vec<Subscription> {
        let mut all = Vec::new();
        // Collect topic-indexed subs (dedup by id since a sub with multiple patterns appears multiple times)
        {
            let mut seen = std::collections::HashSet::new();
            for entry in self.topic_subs.iter() {
                for s in entry.value() {
                    if seen.insert(s.id.clone()) {
                        all.push(s.clone());
                    }
                }
            }
        }
        {
            let gsubs = self.glob_subs.read().await;
            all.extend(gsubs.iter().cloned());
        }
        all
    }

    /// Get aggregate stats for monitoring.
    pub async fn stats(&self) -> BusStats {
        let mut topic_stats = Vec::new();
        for entry in self.topics.iter() {
            topic_stats.push(TopicStats {
                name: entry.key().clone(),
                head_offset: entry.value().head_offset(),
                buffered: entry.value().buffered_count().await,
            });
        }
        let sub_count = {
            let mut seen = std::collections::HashSet::new();
            for entry in self.topic_subs.iter() {
                for s in entry.value() {
                    seen.insert(s.id.clone());
                }
            }
            let gsubs = self.glob_subs.read().await;
            seen.len() + gsubs.len()
        };
        BusStats {
            topic_count: topic_stats.len(),
            subscription_count: sub_count,
            topics: topic_stats,
        }
    }

    /// Drain up to `max` items in weighted-fair priority order.
    ///
    /// Returns dispatch items ordered by WFQ virtual finish time, giving
    /// Urgent events ~8× the throughput of Batch events. Callers should
    /// process the returned items sequentially to honour the WFQ schedule.
    ///
    /// O(max × log K) where K = 3 priority classes.
    pub async fn drain_prioritized(&self, max: usize) -> Vec<DispatchItem> {
        self.wfq
            .drain(max)
            .await
            .into_iter()
            .map(|pi| pi.item)
            .collect()
    }

    /// Number of events pending in the WFQ dispatch queue.
    pub async fn pending_dispatch_count(&self) -> usize {
        self.wfq.len().await
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
