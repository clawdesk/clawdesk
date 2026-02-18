//! Reply pipeline — 7-stage message processing pipeline.
//!
//! Stages: Inbound → Classify → Route → Enrich → Execute → Format → Deliver

use crate::classifier::TriggerClassifier;
use crate::formatter::ResponseFormatter;
use crate::router::{MessageRouter, RoutingDecision};
use clawdesk_types::autoreply::{DeliveryState, DeliveryStatus, TriggerClassification};
use clawdesk_types::message::NormalizedMessage;
use std::time::Instant;
use tracing::{debug, warn};

/// Result of running the reply pipeline.
#[derive(Debug)]
pub struct PipelineResult {
    /// Per-stage timings (stage name, duration).
    pub stage_timings: Vec<(&'static str, std::time::Duration)>,
    /// Final delivery status.
    pub delivery: DeliveryStatus,
    /// The formatted response parts, if any.
    pub response_parts: Vec<String>,
}

/// Agent execution function type.
/// Returns the agent's text response for a given message and agent ID.
pub type AgentExecutor =
    Box<dyn Fn(&NormalizedMessage, &str) -> Result<String, String> + Send + Sync>;

/// The full 7-stage reply pipeline.
pub struct ReplyPipeline {
    classifier: TriggerClassifier,
    router: MessageRouter,
    executor: Option<AgentExecutor>,
}

impl ReplyPipeline {
    pub fn new(classifier: TriggerClassifier, router: MessageRouter) -> Self {
        Self {
            classifier,
            router,
            executor: None,
        }
    }

    /// Set the agent executor function.
    pub fn with_executor(mut self, executor: AgentExecutor) -> Self {
        self.executor = Some(executor);
        self
    }

    /// Run the full pipeline on an inbound message.
    pub fn process(&self, msg: &NormalizedMessage) -> PipelineResult {
        let mut timings: Vec<(&'static str, std::time::Duration)> = Vec::with_capacity(7);

        // Stage 1: Inbound (normalization already done upstream).
        let t = Instant::now();
        debug!(msg_id = %msg.id, "pipeline: inbound");
        timings.push(("inbound", t.elapsed()));

        // Stage 2: Classify.
        let t = Instant::now();
        let classification = self.classifier.classify(msg);
        timings.push(("classify", t.elapsed()));

        let classification = match classification {
            Some(c) => c,
            None => {
                debug!(msg_id = %msg.id, "pipeline: no trigger matched, dropping");
                return PipelineResult {
                    stage_timings: timings,
                    delivery: DeliveryStatus {
                        message_id: msg.id.to_string(),
                        channel: msg.sender.channel.to_string(),
                        status: DeliveryState::Failed,
                        timestamp: chrono::Utc::now(),
                        retry_count: 0,
                        error: Some("no trigger matched".to_string()),
                    },
                    response_parts: vec![],
                };
            }
        };

        // Stage 3: Route.
        let t = Instant::now();
        let decision = self.router.route(msg, classification.clone());
        timings.push(("route", t.elapsed()));

        let (agent_id, _classification) = match decision {
            RoutingDecision::Process {
                agent_id,
                classification,
            } => (agent_id, classification),
            RoutingDecision::Drop { reason } => {
                debug!(msg_id = %msg.id, reason = %reason, "pipeline: routed to drop");
                return PipelineResult {
                    stage_timings: timings,
                    delivery: DeliveryStatus {
                        message_id: msg.id.to_string(),
                        channel: msg.sender.channel.to_string(),
                        status: DeliveryState::Failed,
                        timestamp: chrono::Utc::now(),
                        retry_count: 0,
                        error: Some(reason),
                    },
                    response_parts: vec![],
                };
            }
            RoutingDecision::Queue { reason } => {
                debug!(msg_id = %msg.id, reason = %reason, "pipeline: queued");
                return PipelineResult {
                    stage_timings: timings,
                    delivery: DeliveryStatus {
                        message_id: msg.id.to_string(),
                        channel: msg.sender.channel.to_string(),
                        status: DeliveryState::Queued,
                        timestamp: chrono::Utc::now(),
                        retry_count: 0,
                        error: Some(reason),
                    },
                    response_parts: vec![],
                };
            }
        };

        // Stage 4: Enrich (add context, session info, etc.).
        let t = Instant::now();
        debug!(msg_id = %msg.id, agent = %agent_id, "pipeline: enriching context");
        timings.push(("enrich", t.elapsed()));

        // Stage 5: Execute (call the agent).
        let t = Instant::now();
        let agent_response = if let Some(ref executor) = self.executor {
            match executor(msg, &agent_id) {
                Ok(resp) => resp,
                Err(e) => {
                    warn!(msg_id = %msg.id, error = %e, "pipeline: agent execution failed");
                    timings.push(("execute", t.elapsed()));
                    return PipelineResult {
                        stage_timings: timings,
                        delivery: DeliveryStatus {
                            message_id: msg.id.to_string(),
                            channel: msg.sender.channel.to_string(),
                            status: DeliveryState::Failed,
                            timestamp: chrono::Utc::now(),
                            retry_count: 1,
                            error: Some(e),
                        },
                        response_parts: vec![],
                    };
                }
            }
        } else {
            // No executor registered — return a placeholder.
            format!("[agent:{agent_id}] would respond to: {}", msg.body)
        };
        timings.push(("execute", t.elapsed()));

        // Stage 6: Format.
        let t = Instant::now();
        let channel = &msg.sender.channel;
        let segments = ResponseFormatter::format(&agent_response, channel);
        let response_parts: Vec<String> = segments.into_iter().map(|s| s.text).collect();
        timings.push(("format", t.elapsed()));

        // Stage 7: Deliver.
        let t = Instant::now();
        debug!(
            msg_id = %msg.id,
            parts = response_parts.len(),
            "pipeline: delivering response"
        );
        timings.push(("deliver", t.elapsed()));

        PipelineResult {
            stage_timings: timings,
            delivery: DeliveryStatus {
                message_id: msg.id.to_string(),
                channel: msg.sender.channel.to_string(),
                status: DeliveryState::Delivered,
                timestamp: chrono::Utc::now(),
                retry_count: 0,
                error: None,
            },
            response_parts,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classifier::ClassifierConfig;
    use crate::router::RouterConfig;
    use clawdesk_types::channel::ChannelId;

    fn test_msg(body: &str, channel: ChannelId) -> NormalizedMessage {
        NormalizedMessage {
            id: uuid::Uuid::new_v4(),
            session_key: clawdesk_types::session::SessionKey::new(clawdesk_types::ChannelId::Telegram, "test"),
            body: body.to_string(),
            body_for_agent: None,
            sender: clawdesk_types::message::SenderIdentity {
                id: "user-1".to_string(),
                display_name: "Test".to_string(),
                channel,
            },
            media: vec![],
            reply_context: None,
            origin: clawdesk_types::message::MessageOrigin::Internal {
                source: "test".to_string(),
            },
            timestamp: chrono::Utc::now(),
        }
    }

    fn make_pipeline() -> ReplyPipeline {
        let classifier = TriggerClassifier::new(ClassifierConfig::default());
        let router = MessageRouter::new(RouterConfig::default());
        ReplyPipeline::new(classifier, router)
    }

    #[test]
    fn test_pipeline_with_command() {
        let pipeline = make_pipeline();
        let msg = test_msg("/help", ChannelId::Discord);
        let result = pipeline.process(&msg);
        assert_eq!(result.delivery.status, DeliveryState::Delivered);
        assert!(!result.response_parts.is_empty());
        assert_eq!(result.stage_timings.len(), 7);
    }

    #[test]
    fn test_pipeline_with_executor() {
        let classifier = TriggerClassifier::new(ClassifierConfig::default());
        let router = MessageRouter::new(RouterConfig::default());
        let pipeline = ReplyPipeline::new(classifier, router).with_executor(Box::new(
            |msg, agent| Ok(format!("Response from {agent}: processed '{}'", msg.body)),
        ));
        let msg = test_msg("/ping", ChannelId::Slack);
        let result = pipeline.process(&msg);
        assert_eq!(result.delivery.status, DeliveryState::Delivered);
        assert!(result.response_parts[0].contains("Response from default"));
    }

    #[test]
    fn test_pipeline_executor_error() {
        let classifier = TriggerClassifier::new(ClassifierConfig::default());
        let router = MessageRouter::new(RouterConfig::default());
        let pipeline = ReplyPipeline::new(classifier, router)
            .with_executor(Box::new(|_, _| Err("agent crashed".to_string())));
        let msg = test_msg("/test", ChannelId::Telegram);
        let result = pipeline.process(&msg);
        assert_eq!(result.delivery.status, DeliveryState::Failed);
        assert_eq!(result.delivery.error, Some("agent crashed".to_string()));
    }
}
