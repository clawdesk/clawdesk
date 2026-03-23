//! Conscious Gateway — unified pipeline orchestrating L0→L1→L2→L3→L4.
//!
//! Single entry point: `gateway.evaluate(tool, args, ctx) → GatewayOutcome`.
//! Replaces the Three separate systems (tool_policy, permission_engine, approval_gate)
//! with one graduated awareness pipeline.

use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::awareness::{AwarenessClassifier, ConsciousnessLevel, RiskScore};
use crate::deliberation::{DeliberationOutcome, Deliberator};
use crate::sentinel::{Escalation, Sentinel};
use crate::trace::{ConsciousTrace, GatePath, TraceEntry, TraceOutcome};
use crate::veto::{VetoConfig, VetoDecision, VetoGate};
use crate::workspace::GlobalWorkspace;

/// Context for a tool execution decision.
#[derive(Debug, Clone)]
pub struct SessionContext {
    pub session_id: String,
    pub turn_number: u32,
    pub total_tool_calls: u32,
}

/// Gateway outcome — what should happen with this tool call.
#[derive(Debug)]
pub enum GatewayOutcome {
    /// Execute the tool (possibly at a gated level).
    Execute {
        level: ConsciousnessLevel,
        risk_score: RiskScore,
    },
    /// Tool was self-blocked by deliberation (L2 pattern match).
    SelfBlocked {
        reason: String,
        alternative: Option<String>,
    },
    /// Tool was blocked by the sentinel (L1 anomaly).
    SentinelBlocked {
        escalation: Escalation,
    },
    /// Human vetoed the tool (L3).
    HumanVetoed {
        tool: String,
    },
    /// Human timed out (L3 → deny by default).
    HumanTimeout {
        tool: String,
    },
    /// Human approved with modified arguments (L3).
    Modified {
        modified_args: serde_json::Value,
        level: ConsciousnessLevel,
        risk_score: RiskScore,
    },
}

impl GatewayOutcome {
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Execute { .. } | Self::Modified { .. })
    }
}

/// The Conscious Gateway — unified entry point for all tool execution decisions.
///
/// Orchestrates the five layers:
/// 1. **L0 Awareness Classifier** — risk scoring and level assignment
/// 2. **L1 Sentinel** — anomaly detection and escalation
/// 3. **L2 Deliberation** — pattern matching and optional LLM self-review
/// 4. **L3 Veto Gate** — human approval for Critical-level tools
/// 5. **L4 Trace** — immutable audit trail with feedback learning
pub struct ConsciousGateway {
    /// L0: Risk classifier.
    classifier: RwLock<AwarenessClassifier>,
    /// L1: Anomaly detector.
    sentinel: RwLock<Sentinel>,
    /// L2: Pattern matcher + optional LLM review.
    deliberator: Deliberator,
    /// L3: Human approval gate (pluggable: CLI, TUI, GUI, webhook).
    veto_gate: Option<Arc<dyn VetoGate>>,
    /// L3 config.
    veto_config: VetoConfig,
    /// L4: Audit trail with feedback.
    trace: RwLock<ConsciousTrace>,
    /// Session-scoped veto decisions (AllowForSession/DenyForSession cache).
    session_cache: dashmap::DashMap<String, VetoDecision>,
    /// Global workspace for broadcasting events.
    workspace: Option<Arc<GlobalWorkspace>>,
}

impl ConsciousGateway {
    /// Create a gateway with default configuration.
    pub fn new() -> Self {
        Self {
            classifier: RwLock::new(AwarenessClassifier::new()),
            sentinel: RwLock::new(Sentinel::default()),
            deliberator: Deliberator::default(),
            veto_gate: None,
            veto_config: VetoConfig::default(),
            trace: RwLock::new(ConsciousTrace::default()),
            session_cache: dashmap::DashMap::new(),
            workspace: None,
        }
    }

    /// Set a custom veto gate (CLI, TUI, GUI, etc.).
    pub fn with_veto_gate(mut self, gate: Arc<dyn VetoGate>) -> Self {
        self.veto_gate = Some(gate);
        self
    }

    /// Set veto configuration.
    pub fn with_veto_config(mut self, config: VetoConfig) -> Self {
        self.veto_config = config;
        self
    }

    /// Connect to the global workspace.
    pub fn with_global_workspace(mut self, ws: Arc<GlobalWorkspace>) -> Self {
        self.workspace = Some(ws);
        self
    }

    /// Set custom awareness thresholds.
    pub async fn set_thresholds(&self, thresholds: crate::awareness::LevelThresholds) {
        self.classifier.write().await.set_thresholds(thresholds);
    }

    /// Get a reference to the global workspace (if connected).
    ///
    /// Used by the cognitive event loop and external subsystems to publish
    /// events into the workspace bus.
    pub fn global_workspace(&self) -> Option<&Arc<GlobalWorkspace>> {
        self.workspace.as_ref()
    }

    /// Get mutable access to the sentinel (for injecting external signals).
    pub fn sentinel(&self) -> &RwLock<Sentinel> {
        &self.sentinel
    }

    /// Get mutable access to the classifier (for L4→L0 feedback).
    pub fn classifier(&self) -> &RwLock<AwarenessClassifier> {
        &self.classifier
    }

    /// The main evaluation pipeline — L0 → L1 → L2 → L3 → (execute) → L4.
    ///
    /// Returns a `GatewayOutcome` that tells the caller whether to execute,
    /// block, or request modified arguments.
    pub async fn evaluate(
        &self,
        tool: &str,
        args: &serde_json::Value,
        ctx: &SessionContext,
    ) -> GatewayOutcome {
        // ═══════════════════════════════════════════════════════════════
        // L1: Sentinel — observe and compute escalation boost
        // ═══════════════════════════════════════════════════════════════
        let escalation = {
            let mut sentinel = self.sentinel.write().await;
            sentinel.observe(tool, args)
        };

        // ═══════════════════════════════════════════════════════════════
        // L0: Awareness Classifier — risk scoring
        // ═══════════════════════════════════════════════════════════════
        let (mut level, risk_score) = {
            let classifier = self.classifier.read().await;
            classifier.classify(tool, args, escalation.risk_boost)
        };

        // Apply sentinel escalation overrides
        if escalation.force_human_veto && level < ConsciousnessLevel::Critical {
            let from = level;
            level = ConsciousnessLevel::Critical;
            self.publish_escalation(tool, from, level);
        } else if escalation.force_deliberation && level < ConsciousnessLevel::Deliberative {
            let from = level;
            level = ConsciousnessLevel::Deliberative;
            self.publish_escalation(tool, from, level);
        }

        debug!(
            tool, level = %level, risk = risk_score.composite,
            "consciousness classification"
        );

        // ═══════════════════════════════════════════════════════════════
        // L0: Reflexive — execute immediately, no gating
        // ═══════════════════════════════════════════════════════════════
        if level == ConsciousnessLevel::Reflexive {
            self.record_trace(tool, args, &risk_score, level, ctx, TraceOutcome::Executed, false, None, &[]).await;
            return GatewayOutcome::Execute { level, risk_score };
        }

        // ═══════════════════════════════════════════════════════════════
        // L2: Deliberation — pattern matching for Deliberative+ levels
        // ═══════════════════════════════════════════════════════════════
        if level >= ConsciousnessLevel::Deliberative {
            let delib_outcome = self.deliberator.evaluate(tool, args);
            match delib_outcome {
                DeliberationOutcome::PatternBlock { pattern, explanation } => {
                    warn!(tool, %pattern, "deliberation: pattern blocked");
                    self.record_trace(
                        tool, args, &risk_score, level, ctx,
                        TraceOutcome::PatternBlocked { pattern: pattern.clone() },
                        false, None,
                        &escalation.signals.iter().map(|s| format!("{s:?}")).collect::<Vec<_>>(),
                    ).await;

                    if let Some(ref ws) = self.workspace {
                        ws.publish(crate::workspace::CognitiveEvent::ToolBlocked {
                            tool: tool.to_string(),
                            level: level.as_str().to_string(),
                            reason: explanation.clone(),
                        });
                    }

                    return GatewayOutcome::SelfBlocked {
                        reason: explanation,
                        alternative: None,
                    };
                }
                DeliberationOutcome::SelfBlock { reasoning, alternative } => {
                    self.record_trace(
                        tool, args, &risk_score, level, ctx,
                        TraceOutcome::SelfBlocked { reasoning: reasoning.clone() },
                        false, None, &[],
                    ).await;
                    return GatewayOutcome::SelfBlocked { reason: reasoning, alternative };
                }
                DeliberationOutcome::Escalate { reasoning } => {
                    info!(tool, %reasoning, "deliberation: escalating to human veto");
                    level = ConsciousnessLevel::Critical;
                }
                DeliberationOutcome::Approve { .. } | DeliberationOutcome::Skipped => {
                    // Approved by deliberation — continue to next layer
                }
            }
        }

        // ═══════════════════════════════════════════════════════════════
        // L3: Human Veto — for Critical-level tools
        // ═══════════════════════════════════════════════════════════════
        if level >= ConsciousnessLevel::Critical {
            // Check session cache first
            if let Some(cached) = self.session_cache.get(tool) {
                match cached.value() {
                    VetoDecision::AllowForSession => {
                        self.record_trace(
                            tool, args, &risk_score, level, ctx,
                            TraceOutcome::HumanApproved, false, None, &[],
                        ).await;
                        return GatewayOutcome::Execute { level, risk_score };
                    }
                    VetoDecision::DenyForSession => {
                        self.record_trace(
                            tool, args, &risk_score, level, ctx,
                            TraceOutcome::HumanVetoed, false, None, &[],
                        ).await;
                        return GatewayOutcome::HumanVetoed {
                            tool: tool.to_string(),
                        };
                    }
                    _ => {} // not session-scoped, fall through
                }
            }

            if let Some(ref gate) = self.veto_gate {
                let decision = gate.request_veto(
                    tool, args, risk_score.composite, level.as_str(),
                    &escalation.explanation, &self.veto_config,
                ).await;

                // Cache session-scoped decisions
                if decision.is_session_scoped() {
                    self.session_cache.insert(tool.to_string(), decision.clone());
                }

                match decision {
                    VetoDecision::Allow | VetoDecision::AllowForSession => {
                        self.record_trace(
                            tool, args, &risk_score, level, ctx,
                            TraceOutcome::HumanApproved, false, None, &[],
                        ).await;
                        return GatewayOutcome::Execute { level, risk_score };
                    }
                    VetoDecision::Deny | VetoDecision::DenyForSession => {
                        self.record_trace(
                            tool, args, &risk_score, level, ctx,
                            TraceOutcome::HumanVetoed, false, None, &[],
                        ).await;
                        if let Some(ref ws) = self.workspace {
                            ws.publish(crate::workspace::CognitiveEvent::HumanVeto {
                                tool: tool.to_string(),
                            });
                        }
                        return GatewayOutcome::HumanVetoed {
                            tool: tool.to_string(),
                        };
                    }
                    VetoDecision::Modify { modified_args } => {
                        self.record_trace(
                            tool, args, &risk_score, level, ctx,
                            TraceOutcome::HumanModified, false, None, &[],
                        ).await;
                        match serde_json::from_str(&modified_args) {
                            Ok(parsed) => {
                                return GatewayOutcome::Modified {
                                    modified_args: parsed,
                                    level,
                                    risk_score,
                                };
                            }
                            Err(_) => {
                                return GatewayOutcome::HumanVetoed {
                                    tool: tool.to_string(),
                                };
                            }
                        }
                    }
                    VetoDecision::Timeout => {
                        self.record_trace(
                            tool, args, &risk_score, level, ctx,
                            TraceOutcome::HumanTimeout, false, None, &[],
                        ).await;
                        return GatewayOutcome::HumanTimeout {
                            tool: tool.to_string(),
                        };
                    }
                }
            }
            // No veto gate configured — fail closed for Critical tools
            warn!(tool, "no veto gate configured for Critical-level tool — denying");
            return GatewayOutcome::HumanVetoed {
                tool: tool.to_string(),
            };
        }

        // ═══════════════════════════════════════════════════════════════
        // Preconscious / Deliberative (approved) — execute with trace
        // ═══════════════════════════════════════════════════════════════
        self.record_trace(
            tool, args, &risk_score, level, ctx,
            TraceOutcome::Executed, false, None, &[],
        ).await;

        GatewayOutcome::Execute { level, risk_score }
    }

    /// Record the result after tool execution (for trace completeness).
    pub async fn record_result(&self, tool: &str, success: bool, duration_ms: u64, cost: Option<f64>) {
        // Update sentinel with cost
        if let Some(c) = cost {
            self.sentinel.write().await.record_cost(c);
        }

        // Periodic feedback: apply L4→L0 learning
        let trace = self.trace.read().await;
        if trace.len() % 50 == 0 && trace.len() > 0 {
            let feedback = trace.compute_feedback();
            drop(trace);
            if !feedback.is_empty() {
                let mut classifier = self.classifier.write().await;
                for (tool_name, delta) in &feedback {
                    classifier.adjust_base_risk(tool_name, *delta);
                    debug!(tool = tool_name, delta, "L4→L0 risk feedback applied");
                }
            }
        }
    }

    /// Reset session state (veto cache, sentinel, etc.).
    pub async fn reset_session(&self) {
        self.session_cache.clear();
        self.sentinel.write().await.reset();
    }

    /// Get recent trace entries.
    pub async fn recent_trace(&self, n: usize) -> Vec<TraceEntry> {
        self.trace.read().await.recent(n).into_iter().cloned().collect()
    }

    // ─── Internal helpers ──────────────────────────────────────────────

    #[allow(clippy::too_many_arguments)]
    async fn record_trace(
        &self,
        tool: &str,
        args: &serde_json::Value,
        risk_score: &RiskScore,
        level: ConsciousnessLevel,
        ctx: &SessionContext,
        outcome: TraceOutcome,
        sentinel_escalated: bool,
        _duration_ms: Option<u64>,
        sentinel_signals: &[String],
    ) {
        let entry = TraceEntry {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: chrono::Utc::now(),
            session_id: ctx.session_id.clone(),
            tool: tool.to_string(),
            args: args.clone(),
            risk_score: risk_score.clone(),
            level,
            gate_path: GatePath {
                classified_level: level,
                sentinel_escalated,
                deliberation: if level >= ConsciousnessLevel::Deliberative {
                    Some("checked".to_string())
                } else {
                    None
                },
                human_veto: if level >= ConsciousnessLevel::Critical {
                    Some(format!("{outcome:?}"))
                } else {
                    None
                },
            },
            outcome,
            duration_ms: _duration_ms,
            cost_delta: None,
            sentinel_signals: sentinel_signals.to_vec(),
        };
        self.trace.write().await.record(entry);
    }

    fn publish_escalation(&self, tool: &str, from: ConsciousnessLevel, to: ConsciousnessLevel) {
        if let Some(ref ws) = self.workspace {
            ws.publish(crate::workspace::CognitiveEvent::RiskEscalated {
                tool: tool.to_string(),
                from_level: from.as_str().to_string(),
                to_level: to.as_str().to_string(),
            });
        }
    }
}

impl Default for ConsciousGateway {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::veto::AutoApproveGate;

    #[tokio::test]
    async fn reflexive_tool_executes_immediately() {
        let gw = ConsciousGateway::new();
        let ctx = SessionContext {
            session_id: "test".into(),
            turn_number: 1,
            total_tool_calls: 0,
        };
        let outcome = gw.evaluate("file_read", &serde_json::json!({"path": "/tmp/test"}), &ctx).await;
        assert!(outcome.is_allowed());
    }

    #[tokio::test]
    async fn fork_bomb_is_blocked() {
        let gw = ConsciousGateway::new();
        let ctx = SessionContext {
            session_id: "test".into(),
            turn_number: 1,
            total_tool_calls: 0,
        };
        let outcome = gw.evaluate(
            "shell_exec",
            &serde_json::json!({"command": ":(){ :|:& };:"}),
            &ctx,
        ).await;
        assert!(!outcome.is_allowed());
    }

    #[tokio::test]
    async fn critical_tool_denied_without_veto_gate() {
        let gw = ConsciousGateway::new();
        let ctx = SessionContext {
            session_id: "test".into(),
            turn_number: 1,
            total_tool_calls: 0,
        };
        let outcome = gw.evaluate(
            "deploy",
            &serde_json::json!({}),
            &ctx,
        ).await;
        assert!(!outcome.is_allowed());
    }

    #[tokio::test]
    async fn critical_tool_approved_with_auto_approve_gate() {
        let gw = ConsciousGateway::new()
            .with_veto_gate(Arc::new(AutoApproveGate));
        let ctx = SessionContext {
            session_id: "test".into(),
            turn_number: 1,
            total_tool_calls: 0,
        };
        let outcome = gw.evaluate(
            "deploy",
            &serde_json::json!({}),
            &ctx,
        ).await;
        assert!(outcome.is_allowed());
    }

    #[tokio::test]
    async fn preconscious_tool_executes_with_sentinel() {
        let gw = ConsciousGateway::new();
        let ctx = SessionContext {
            session_id: "test".into(),
            turn_number: 1,
            total_tool_calls: 0,
        };
        let outcome = gw.evaluate(
            "file_write",
            &serde_json::json!({"path": "/tmp/test.rs", "content": "fn main() {}"}),
            &ctx,
        ).await;
        assert!(outcome.is_allowed());
    }

    #[tokio::test]
    async fn session_cache_honors_allow_for_session() {
        let gw = ConsciousGateway::new()
            .with_veto_gate(Arc::new(AutoApproveGate));
        let ctx = SessionContext {
            session_id: "test".into(),
            turn_number: 1,
            total_tool_calls: 0,
        };

        // First call — goes through veto gate
        let outcome = gw.evaluate("deploy", &serde_json::json!({}), &ctx).await;
        assert!(outcome.is_allowed());

        // Cache an AllowForSession decision
        gw.session_cache.insert("deploy".to_string(), VetoDecision::AllowForSession);

        // Second call — should use cache
        let outcome2 = gw.evaluate("deploy", &serde_json::json!({}), &ctx).await;
        assert!(outcome2.is_allowed());
    }

    #[tokio::test]
    async fn trace_records_all_decisions() {
        let gw = ConsciousGateway::new();
        let ctx = SessionContext {
            session_id: "test".into(),
            turn_number: 1,
            total_tool_calls: 0,
        };

        gw.evaluate("file_read", &serde_json::json!({}), &ctx).await;
        gw.evaluate("file_write", &serde_json::json!({}), &ctx).await;
        gw.evaluate("shell_exec", &serde_json::json!({"command": ":(){ :|:& };:"}), &ctx).await;

        let entries = gw.recent_trace(10).await;
        assert_eq!(entries.len(), 3);
    }
}
