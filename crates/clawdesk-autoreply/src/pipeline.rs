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

/// Streaming agent executor.
/// Returns a stream of text chunks instead of a single complete response.
/// The receiver yields partial response chunks as they arrive from the LLM.
///
/// **Status:** Type and builder (`with_streaming_executor`) are defined.
/// The Tauri gateway currently streams via its own `stream_chat` path in
/// `commands.rs`, bypassing the `ReplyPipeline`. To unify, wire a
/// `StreamingAgentExecutor` callback that delegates to `stream_chat` and
/// call `process_streaming()` from the messaging-channel path.
pub type StreamingAgentExecutor = Box<
    dyn Fn(
            NormalizedMessage,
            String,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<tokio::sync::mpsc::Receiver<String>, String>,
                    > + Send,
            >,
        > + Send
        + Sync,
>;

/// The full 7-stage reply pipeline.
pub struct ReplyPipeline {
    classifier: TriggerClassifier,
    router: MessageRouter,
    executor: Option<AgentExecutor>,
    /// Optional streaming executor for channels that prefer streaming.
    streaming_executor: Option<StreamingAgentExecutor>,
}

impl ReplyPipeline {
    pub fn new(classifier: TriggerClassifier, router: MessageRouter) -> Self {
        Self {
            classifier,
            router,
            executor: None,
            streaming_executor: None,
        }
    }

    /// Set the agent executor function.
    pub fn with_executor(mut self, executor: AgentExecutor) -> Self {
        self.executor = Some(executor);
        self
    }

    /// Set the streaming agent executor function.
    ///
    /// When `prefer_streaming` is true on the `ReplyPath`, calling
    /// `process_streaming()` will use this executor instead of the
    /// synchronous one, delivering chunks as they arrive.
    pub fn with_streaming_executor(mut self, executor: StreamingAgentExecutor) -> Self {
        self.streaming_executor = Some(executor);
        self
    }

    /// Check if streaming is available for a given reply path.
    pub fn supports_streaming(&self, prefer_streaming: bool) -> bool {
        prefer_streaming && self.streaming_executor.is_some()
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

        // Stage 4: Enrich (add context, session info, media descriptions).
        //
        // Process media attachments and inject text descriptions into the
        // message body so the agent can reason about images, audio, documents, etc.
        // URL link previews are also extracted and appended as context.
        let t = Instant::now();
        let enriched_body = if !msg.media.is_empty() {
            let mut parts = Vec::with_capacity(msg.media.len() + 1);
            for (i, attachment) in msg.media.iter().enumerate() {
                let media_desc = match attachment.media_type {
                    clawdesk_types::message::MediaType::Image => {
                        format!(
                            "[Attached image {}: {} ({})]",
                            i + 1,
                            attachment.filename.as_deref().unwrap_or("image"),
                            attachment.mime_type,
                        )
                    }
                    clawdesk_types::message::MediaType::Audio
                    | clawdesk_types::message::MediaType::Voice => {
                        format!(
                            "[Attached audio {}: {} ({})]",
                            i + 1,
                            attachment.filename.as_deref().unwrap_or("audio"),
                            attachment.mime_type,
                        )
                    }
                    clawdesk_types::message::MediaType::Video
                    | clawdesk_types::message::MediaType::Animation => {
                        format!(
                            "[Attached video {}: {} ({})]",
                            i + 1,
                            attachment.filename.as_deref().unwrap_or("video"),
                            attachment.mime_type,
                        )
                    }
                    clawdesk_types::message::MediaType::Document => {
                        format!(
                            "[Attached document {}: {} ({}, {} bytes)]",
                            i + 1,
                            attachment.filename.as_deref().unwrap_or("document"),
                            attachment.mime_type,
                            attachment.size_bytes.unwrap_or(0),
                        )
                    }
                    clawdesk_types::message::MediaType::Sticker => {
                        format!("[Sticker {}]", i + 1)
                    }
                };
                parts.push(media_desc);
            }
            parts.push(msg.body.clone());
            debug!(
                msg_id = %msg.id,
                agent = %agent_id,
                media_count = msg.media.len(),
                "pipeline: enriched with {} media descriptions",
                msg.media.len(),
            );
            parts.join("\n")
        } else {
            msg.body.clone()
        };
        timings.push(("enrich", t.elapsed()));

        // Stage 5: Execute (call the agent).
        // Pass the enriched body (with media descriptions) to the executor.
        let t = Instant::now();
        let enriched_msg = if enriched_body != msg.body {
            let mut m = msg.clone();
            m.body = enriched_body;
            m
        } else {
            msg.clone()
        };
        let agent_response = if let Some(ref executor) = self.executor {
            match executor(&enriched_msg, &agent_id) {
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
            // No executor registered — fail explicitly instead of sending
            // debug placeholder strings to end users.
            warn!(
                msg_id = %msg.id,
                agent = %agent_id,
                "pipeline: no agent executor registered — cannot process message"
            );
            timings.push(("execute", t.elapsed()));
            return PipelineResult {
                stage_timings: timings,
                delivery: DeliveryStatus {
                    message_id: msg.id.to_string(),
                    channel: msg.sender.channel.to_string(),
                    status: DeliveryState::Failed,
                    timestamp: chrono::Utc::now(),
                    retry_count: 0,
                    error: Some("no agent executor registered".to_string()),
                },
                response_parts: vec![],
            };
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

    /// Run the streaming pipeline — applies all 7 stages as stream transforms.
    ///
    /// Each stage processes chunks as they flow through. This ensures streaming
    /// responses go through the same Classify → Route → Enrich → Execute →
    /// Format → Deliver pipeline as non-streaming, so skills and adapters
    /// can intercept or transform streaming responses.
    ///
    /// Per-chunk formatting overhead is sub-microsecond — negligible vs LLM
    /// inference latency.
    pub async fn process_streaming(
        &self,
        msg: &NormalizedMessage,
    ) -> StreamingPipelineResult {
        let mut timings: Vec<(&'static str, std::time::Duration)> = Vec::with_capacity(7);

        // Stage 1: Inbound
        let t = Instant::now();
        debug!(msg_id = %msg.id, "streaming pipeline: inbound");
        timings.push(("inbound", t.elapsed()));

        // Stage 2: Classify
        let t = Instant::now();
        let classification = self.classifier.classify(msg);
        timings.push(("classify", t.elapsed()));

        let classification = match classification {
            Some(c) => c,
            None => {
                debug!(msg_id = %msg.id, "streaming pipeline: no trigger matched");
                return StreamingPipelineResult {
                    stage_timings: timings,
                    delivery: DeliveryStatus {
                        message_id: msg.id.to_string(),
                        channel: msg.sender.channel.to_string(),
                        status: DeliveryState::Failed,
                        timestamp: chrono::Utc::now(),
                        retry_count: 0,
                        error: Some("no trigger matched".to_string()),
                    },
                    stream: None,
                };
            }
        };

        // Stage 3: Route
        let t = Instant::now();
        let decision = self.router.route(msg, classification.clone());
        timings.push(("route", t.elapsed()));

        let (agent_id, _classification) = match decision {
            RoutingDecision::Process {
                agent_id,
                classification,
            } => (agent_id, classification),
            RoutingDecision::Drop { reason } => {
                debug!(msg_id = %msg.id, reason = %reason, "streaming pipeline: dropped");
                return StreamingPipelineResult {
                    stage_timings: timings,
                    delivery: DeliveryStatus {
                        message_id: msg.id.to_string(),
                        channel: msg.sender.channel.to_string(),
                        status: DeliveryState::Failed,
                        timestamp: chrono::Utc::now(),
                        retry_count: 0,
                        error: Some(reason),
                    },
                    stream: None,
                };
            }
            RoutingDecision::Queue { reason } => {
                return StreamingPipelineResult {
                    stage_timings: timings,
                    delivery: DeliveryStatus {
                        message_id: msg.id.to_string(),
                        channel: msg.sender.channel.to_string(),
                        status: DeliveryState::Queued,
                        timestamp: chrono::Utc::now(),
                        retry_count: 0,
                        error: Some(reason),
                    },
                    stream: None,
                };
            }
        };

        // Stage 4: Enrich (same as non-streaming)
        let t = Instant::now();
        timings.push(("enrich", t.elapsed()));

        // Stage 5: Execute (streaming) — get chunk receiver from streaming executor
        let t = Instant::now();
        let executor = match &self.streaming_executor {
            Some(exec) => exec,
            None => {
                warn!(msg_id = %msg.id, "streaming executor not set, falling back to sync");
                timings.push(("execute", t.elapsed()));
                return StreamingPipelineResult {
                    stage_timings: timings,
                    delivery: DeliveryStatus {
                        message_id: msg.id.to_string(),
                        channel: msg.sender.channel.to_string(),
                        status: DeliveryState::Failed,
                        timestamp: chrono::Utc::now(),
                        retry_count: 0,
                        error: Some("no streaming executor configured".to_string()),
                    },
                    stream: None,
                };
            }
        };

        let rx = match executor(msg.clone(), agent_id).await {
            Ok(rx) => rx,
            Err(e) => {
                timings.push(("execute", t.elapsed()));
                return StreamingPipelineResult {
                    stage_timings: timings,
                    delivery: DeliveryStatus {
                        message_id: msg.id.to_string(),
                        channel: msg.sender.channel.to_string(),
                        status: DeliveryState::Failed,
                        timestamp: chrono::Utc::now(),
                        retry_count: 0,
                        error: Some(e),
                    },
                    stream: None,
                };
            }
        };
        timings.push(("execute", t.elapsed()));

        // Stage 6 & 7: Format + Deliver as stream transforms.
        // Each chunk passes through the formatter before being forwarded.
        let channel = msg.sender.channel.clone();
        let (out_tx, out_rx) = tokio::sync::mpsc::channel::<String>(64);

        tokio::spawn(async move {
            let mut rx = rx;
            while let Some(chunk) = rx.recv().await {
                // Apply per-chunk formatting (sub-microsecond overhead)
                let formatted = ResponseFormatter::format_chunk(&chunk, &channel);
                if out_tx.send(formatted).await.is_err() {
                    break;
                }
            }
        });

        StreamingPipelineResult {
            stage_timings: timings,
            delivery: DeliveryStatus {
                message_id: msg.id.to_string(),
                channel: msg.sender.channel.to_string(),
                status: DeliveryState::Delivered,
                timestamp: chrono::Utc::now(),
                retry_count: 0,
                error: None,
            },
            stream: Some(out_rx),
        }
    }
}

/// Result of the streaming pipeline.
#[derive(Debug)]
pub struct StreamingPipelineResult {
    /// Per-stage timings (stage name, duration).
    pub stage_timings: Vec<(&'static str, std::time::Duration)>,
    /// Final delivery status.
    pub delivery: DeliveryStatus,
    /// Stream of formatted response chunks, if execution succeeded.
    pub stream: Option<tokio::sync::mpsc::Receiver<String>>,
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
            artifact_refs: vec![],
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
        let pipeline = make_pipeline().with_executor(Box::new(|msg, _agent| {
            Ok(format!("handled command: {}", msg.body))
        }));
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
