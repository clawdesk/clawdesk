//! Event Bus Bridge — connects the reactive event bus to the pipeline executor.
//!
//! ## Event Bus ↔ Gateway Bridge
//!
//! The `EventBus::publish()` method returns `Vec<String>` of matching pipeline
//! IDs, but prior to this module, **nothing consumed them**. This bridge closes
//! the loop:
//!
//! ```text
//! Event → EventBus::publish() → [pipeline_ids] → BusBridge → PipelineExecutor
//! ```
//!
//! ## Design
//!
//! The bridge runs a background task that:
//! 1. Wraps `EventBus::publish()` with callback-driven pipeline dispatch.
//! 2. Resolves pipeline IDs to `AgentPipeline` definitions from a registry.
//! 3. Spawns pipeline execution via the `PipelineExecutor`.
//! 4. Collects results and emits `PipelineCompleted` events back to the bus.
//!
//! ## Concurrency
//!
//! Pipeline executions run concurrently via `JoinSet` with a configurable
//! concurrency limit. A semaphore bounds the number of in-flight pipelines
//! to prevent unbounded resource usage.

use crate::dispatch::EventBus;
use crate::event::{Event, EventKind, Priority};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock, Semaphore};
use tokio::task::JoinSet;
use tracing::{debug, error, info, warn};

// ═══════════════════════════════════════════════════════════════════════════
// Pipeline definition trait (decouples from clawdesk-agents)
// ═══════════════════════════════════════════════════════════════════════════

/// Definition of a pipeline that can be triggered by an event.
///
/// This trait decouples the bus bridge from the `clawdesk-agents` crate's
/// concrete `AgentPipeline` type to avoid circular dependencies.
#[derive(Debug, Clone)]
pub struct PipelineDefinition {
    /// Pipeline identifier (matches subscription's `pipeline_id`).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Serialized pipeline config (deserialized by the executor).
    pub config: Value,
    /// Whether the pipeline supports concurrent executions.
    pub allow_concurrent: bool,
}

/// Result of a pipeline execution triggered by the bridge.
#[derive(Debug, Clone)]
pub struct PipelineRunResult {
    /// Pipeline ID that was executed.
    pub pipeline_id: String,
    /// Whether execution succeeded.
    pub success: bool,
    /// Output of the pipeline (final step output).
    pub output: Value,
    /// Duration in milliseconds.
    pub duration_ms: u64,
    /// Error message, if any.
    pub error: Option<String>,
    /// Trigger event ID that caused this execution.
    pub trigger_event_id: String,
}

/// Backend for executing pipelines (injected by the application layer).
///
/// This trait keeps the bus crate independent of clawdesk-agents.
/// The application glue layer implements it by delegating to PipelineExecutor.
#[async_trait]
pub trait PipelineRunner: Send + Sync + 'static {
    /// Execute a pipeline with the given input and return the result.
    async fn run(&self, pipeline: &PipelineDefinition, input: &str) -> PipelineRunResult;
}

// ═══════════════════════════════════════════════════════════════════════════
// Pipeline registry
// ═══════════════════════════════════════════════════════════════════════════

/// Registry of pipeline definitions, indexed by pipeline ID.
///
/// Thread-safe (wrapped in `RwLock`) for concurrent access.
pub struct PipelineRegistry {
    pipelines: RwLock<HashMap<String, PipelineDefinition>>,
}

impl PipelineRegistry {
    pub fn new() -> Self {
        Self {
            pipelines: RwLock::new(HashMap::new()),
        }
    }

    /// Register a pipeline definition.
    pub async fn register(&self, pipeline: PipelineDefinition) {
        info!(id = %pipeline.id, name = %pipeline.name, "registering pipeline");
        self.pipelines
            .write()
            .await
            .insert(pipeline.id.clone(), pipeline);
    }

    /// Unregister a pipeline by ID.
    pub async fn unregister(&self, id: &str) -> bool {
        self.pipelines.write().await.remove(id).is_some()
    }

    /// Look up a pipeline by ID.
    pub async fn get(&self, id: &str) -> Option<PipelineDefinition> {
        self.pipelines.read().await.get(id).cloned()
    }

    /// List all registered pipeline IDs.
    pub async fn list(&self) -> Vec<String> {
        self.pipelines.read().await.keys().cloned().collect()
    }
}

impl Default for PipelineRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Bus bridge
// ═══════════════════════════════════════════════════════════════════════════

/// Event bus bridge — connects event subscriptions to pipeline execution.
///
/// Wraps `EventBus::publish()` to automatically dispatch matching
/// pipelines when events are published.
pub struct BusBridge {
    bus: Arc<EventBus>,
    registry: Arc<PipelineRegistry>,
    runner: Arc<dyn PipelineRunner>,
    /// Limits concurrent pipeline executions.
    concurrency_semaphore: Arc<Semaphore>,
    /// Channel for pipeline results (for monitoring).
    result_tx: mpsc::UnboundedSender<PipelineRunResult>,
    result_rx: Option<mpsc::UnboundedReceiver<PipelineRunResult>>,
    /// Maximum concurrent pipeline executions.
    max_concurrency: usize,
}

impl BusBridge {
    /// Create a new bus bridge.
    pub fn new(
        bus: Arc<EventBus>,
        registry: Arc<PipelineRegistry>,
        runner: Arc<dyn PipelineRunner>,
    ) -> Self {
        let max_concurrency = 32;
        let (result_tx, result_rx) = mpsc::unbounded_channel();
        Self {
            bus,
            registry,
            runner,
            concurrency_semaphore: Arc::new(Semaphore::new(max_concurrency)),
            result_tx,
            result_rx: Some(result_rx),
            max_concurrency,
        }
    }

    /// Set the maximum number of concurrent pipeline executions.
    pub fn with_concurrency(mut self, max: usize) -> Self {
        self.max_concurrency = max;
        self.concurrency_semaphore = Arc::new(Semaphore::new(max));
        self
    }

    /// Take the result receiver (for monitoring/logging pipeline outcomes).
    ///
    /// Can only be called once; subsequent calls return None.
    pub fn take_result_receiver(&mut self) -> Option<mpsc::UnboundedReceiver<PipelineRunResult>> {
        self.result_rx.take()
    }

    /// Publish an event through the bridge.
    ///
    /// This is the primary entry point. It:
    /// 1. Publishes the event to the bus.
    /// 2. Resolves matched pipeline IDs.
    /// 3. Spawns pipeline execution for each match.
    ///
    /// Returns the list of pipeline IDs that were triggered.
    pub async fn publish_and_dispatch(&self, event: Event) -> Vec<String> {
        let event_id = event.id.to_string();
        let event_topic = event.topic.clone();

        // Serialize the event as input text for the pipeline
        let input = serde_json::to_string(&event).unwrap_or_default();

        // Publish to the bus, get matching pipeline IDs
        let pipeline_ids = self.bus.publish(event).await;

        if pipeline_ids.is_empty() {
            return vec![];
        }

        debug!(
            topic = %event_topic,
            triggered = pipeline_ids.len(),
            "dispatching pipelines from event"
        );

        // Spawn pipeline executions
        for pipeline_id in &pipeline_ids {
            let pipeline_id = pipeline_id.clone();
            let registry = Arc::clone(&self.registry);
            let runner = Arc::clone(&self.runner);
            let semaphore = Arc::clone(&self.concurrency_semaphore);
            let result_tx = self.result_tx.clone();
            let input = input.clone();
            let event_id = event_id.clone();

            tokio::spawn(async move {
                // Acquire concurrency permit
                let _permit = match semaphore.acquire().await {
                    Ok(permit) => permit,
                    Err(_) => {
                        warn!(pipeline = %pipeline_id, "semaphore closed, skipping");
                        return;
                    }
                };

                // Look up pipeline definition
                let Some(pipeline_def) = registry.get(&pipeline_id).await else {
                    warn!(pipeline = %pipeline_id, "pipeline not found in registry");
                    return;
                };

                // Execute
                let start = std::time::Instant::now();
                let mut result = runner.run(&pipeline_def, &input).await;
                result.trigger_event_id = event_id;

                let duration = start.elapsed().as_millis() as u64;
                info!(
                    pipeline = %pipeline_id,
                    success = result.success,
                    duration_ms = duration,
                    "pipeline execution completed"
                );

                // Send result for monitoring
                let _ = result_tx.send(result);
            });
        }

        pipeline_ids
    }

    /// Publish a completed pipeline result back to the event bus.
    ///
    /// Emits a `PipelineCompleted` event so downstream subscriptions
    /// can react to pipeline outputs (e.g., chaining pipelines).
    pub async fn emit_completion(&self, result: &PipelineRunResult) {
        let payload = json!({
            "pipeline_id": result.pipeline_id,
            "success": result.success,
            "output": result.output,
            "duration_ms": result.duration_ms,
            "error": result.error,
            "trigger_event_id": result.trigger_event_id,
        });

        self.bus
            .emit(
                format!("pipeline.{}", result.pipeline_id),
                EventKind::PipelineCompleted,
                Priority::Standard,
                payload,
                "bus-bridge",
            )
            .await;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use crate::subscription::Subscription;

    struct MockRunner;

    #[async_trait]
    impl PipelineRunner for MockRunner {
        async fn run(&self, pipeline: &PipelineDefinition, input: &str) -> PipelineRunResult {
            PipelineRunResult {
                pipeline_id: pipeline.id.clone(),
                success: true,
                output: json!({ "echoed": input.len() }),
                duration_ms: 1,
                error: None,
                trigger_event_id: String::new(),
            }
        }
    }

    fn make_subscription(pipeline_id: &str, topic_pattern: &str) -> Subscription {
        Subscription {
            id: format!("sub-{}", pipeline_id),
            name: format!("Sub for {}", pipeline_id),
            topic_patterns: vec![topic_pattern.to_string()],
            event_kinds: vec![],
            min_priority: None,
            pipeline_id: pipeline_id.to_string(),
            enabled: true,
            batch_size: 1,
            flush_interval_secs: 0,
        }
    }

    #[tokio::test]
    async fn publish_triggers_matching_pipeline() {
        let bus = EventBus::new(64);
        let registry = Arc::new(PipelineRegistry::new());
        let runner = Arc::new(MockRunner);

        // Register pipeline + subscription
        registry
            .register(PipelineDefinition {
                id: "email-pipe".into(),
                name: "Email Pipeline".into(),
                config: json!({}),
                allow_concurrent: true,
            })
            .await;
        bus.subscribe(make_subscription("email-pipe", "email.*"))
            .await;

        // Create bridge and publish
        let mut bridge = BusBridge::new(bus, registry, runner);
        let mut rx = bridge.take_result_receiver().unwrap();

        let event = Event::new(
            "email.incoming",
            EventKind::EmailIngested,
            Priority::Standard,
            json!({"subject": "test"}),
            "test",
        );

        let triggered = bridge.publish_and_dispatch(event).await;
        assert_eq!(triggered, vec!["email-pipe".to_string()]);

        // Wait for result
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            rx.recv(),
        )
        .await
        .unwrap()
        .unwrap();

        assert!(result.success);
        assert_eq!(result.pipeline_id, "email-pipe");
    }

    #[tokio::test]
    async fn no_match_triggers_nothing() {
        let bus = EventBus::new(64);
        let registry = Arc::new(PipelineRegistry::new());
        let runner = Arc::new(MockRunner);

        bus.subscribe(make_subscription("email-pipe", "email.*"))
            .await;

        let bridge = BusBridge::new(bus, registry, runner);
        let event = Event::new(
            "calendar.event",
            EventKind::CalendarEvent,
            Priority::Standard,
            json!({}),
            "test",
        );

        let triggered = bridge.publish_and_dispatch(event).await;
        assert!(triggered.is_empty());
    }

    #[tokio::test]
    async fn pipeline_registry_operations() {
        let registry = PipelineRegistry::new();

        registry
            .register(PipelineDefinition {
                id: "p1".into(),
                name: "Pipeline 1".into(),
                config: json!({}),
                allow_concurrent: false,
            })
            .await;

        assert!(registry.get("p1").await.is_some());
        assert!(registry.get("p2").await.is_none());
        assert_eq!(registry.list().await, vec!["p1".to_string()]);

        assert!(registry.unregister("p1").await);
        assert!(registry.get("p1").await.is_none());
    }
}
