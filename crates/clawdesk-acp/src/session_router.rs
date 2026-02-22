//! A2A Session Router — session-key-aware agent routing with discovery.
//!
//! ## Bidirectional Agent Routing
//!
//! Extends the capability-based `AgentRouter` with:
//! - **Session affinity**: a routing table maps `(agent_id, session_key)` pairs
//!   so multi-turn conversations route to the same agent instance.
//! - **OpenClaw gateway integration**: OC agents appear as first-class entries
//!   in `AgentDirectory`, tagged with `AgentSource::OpenClaw`.
//! - **Discovery protocol**: periodic crawl of `/.well-known/agent.json` to
//!   auto-register remote agents.
//! - **Circuit breaker**: per-agent failure tracking with configurable trip threshold.
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────────┐
//! │                 SessionRouter                         │
//! │  ┌─────────────────────┐ ┌────────────────────────┐  │
//! │  │  AgentDirectory     │ │  SessionTable          │  │
//! │  │  (capabilities)     │ │  agent→session→conn    │  │
//! │  └──────────┬──────────┘ └───────────┬────────────┘  │
//! │             │                        │               │
//! │  ┌──────────▼──────────────────────▼──────────────┐  │
//! │  │           AgentRouter (score + load)            │  │
//! │  └────────────────────┬───────────────────────────┘  │
//! │                       │                              │
//! │  ┌────────────────────▼───────────────────────────┐  │
//! │  │         CircuitBreakerState (per agent)        │  │
//! │  └────────────────────────────────────────────────┘  │
//! └──────────────────────────────────────────────────────┘
//! ```
//!
//! ## Session affinity
//!
//! Multi-turn conversations need sticky routing: once an agent picks up
//! turn 1 of a session, subsequent turns should route to the same agent.
//! The session table implements this:
//!
//! $$
//! \text{route}(task) = \begin{cases}
//!   \text{sessionTable}[agent\_id][session\_key] & \text{if affinity hit} \\
//!   \arg\max_a \text{score}(a, task) & \text{otherwise}
//! \end{cases}
//! $$

use crate::agent_card::{AgentCard, AgentCapability};
use crate::router::{AgentDirectory, AgentRouter, RoutingDecision};
use chrono::{DateTime, Utc};
use rustc_hash::FxHashMap;
use std::time::Duration;
use tracing::{debug, info, warn};

// Re-export the typed SessionKey from clawdesk-types.
// Task 8 (Session Key algebra): replaces the old `type SessionKey = String`
// alias with the structured `SessionKey { channel: ChannelId, identifier: CompactId }`.
// The typed key carries routing metadata (which channel the session belongs to)
// and uses stack-allocated `CompactId` (≤63 bytes inline, no heap alloc).
pub use clawdesk_types::session::SessionKey;

// ═══════════════════════════════════════════════════════════════════════════
// Types
// ═══════════════════════════════════════════════════════════════════════════

/// Where an agent was discovered from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentSource {
    /// Native ClawDesk agent (Rust, in-process or local).
    ClawDesk,
    /// OpenClaw gateway agent (TypeScript, remote RPC).
    OpenClaw { gateway_url: String },
    /// External A2A agent discovered via well-known URL.
    External { discovery_url: String },
}

/// An entry in the session affinity table.
#[derive(Debug, Clone)]
struct AffinityEntry {
    agent_id: String,
    established_at: DateTime<Utc>,
    turn_count: u32,
    last_used: DateTime<Utc>,
}

/// Per-agent circuit breaker state.
#[derive(Debug, Clone)]
pub struct CircuitBreaker {
    /// Consecutive failure count.
    pub failures: u32,
    /// Failures needed to trip the breaker.
    pub trip_threshold: u32,
    /// When the breaker was tripped (None = closed/healthy).
    pub tripped_at: Option<DateTime<Utc>>,
    /// How long to wait before allowing a probe request.
    pub recovery_timeout: Duration,
    /// Total successes for health ratio computation.
    pub total_successes: u64,
    /// Total failures for health ratio computation.
    pub total_failures: u64,
}

impl CircuitBreaker {
    fn new(trip_threshold: u32, recovery_timeout: Duration) -> Self {
        Self {
            failures: 0,
            trip_threshold,
            tripped_at: None,
            recovery_timeout,
            total_successes: 0,
            total_failures: 0,
        }
    }

    /// Is the breaker currently open (blocking requests)?
    fn is_open(&self) -> bool {
        if let Some(tripped) = self.tripped_at {
            let elapsed = Utc::now().signed_duration_since(tripped);
            elapsed < chrono::Duration::from_std(self.recovery_timeout).unwrap_or(chrono::Duration::seconds(60))
        } else {
            false
        }
    }

    /// Record a successful call.
    fn record_success(&mut self) {
        self.failures = 0;
        self.tripped_at = None;
        self.total_successes += 1;
    }

    /// Record a failed call. Returns true if the breaker just tripped.
    fn record_failure(&mut self) -> bool {
        self.failures += 1;
        self.total_failures += 1;
        if self.failures >= self.trip_threshold && self.tripped_at.is_none() {
            self.tripped_at = Some(Utc::now());
            true
        } else {
            false
        }
    }

    /// Health ratio: successes / (successes + failures). NaN-safe.
    fn health_ratio(&self) -> f64 {
        let total = self.total_successes + self.total_failures;
        if total == 0 {
            1.0
        } else {
            self.total_successes as f64 / total as f64
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Session router
// ═══════════════════════════════════════════════════════════════════════════

/// Session-aware agent router with circuit breakers and source tracking.
pub struct SessionRouter {
    /// Underlying capability-based router.
    pub router: AgentRouter,
    /// Agent directory (shared with the base router).
    pub directory: AgentDirectory,
    /// Session affinity table: typed SessionKey → AffinityEntry.
    /// Task 8: Uses `clawdesk_types::session::SessionKey` (channel + identifier)
    /// instead of bare `String` for routing-aware affinity lookup.
    affinity: FxHashMap<SessionKey, AffinityEntry>,
    /// Agent source registry: agent_id → source.
    sources: FxHashMap<String, AgentSource>,
    /// Per-agent circuit breakers.
    breakers: FxHashMap<String, CircuitBreaker>,
    /// Default circuit-breaker config.
    cb_trip_threshold: u32,
    cb_recovery_timeout: Duration,
    /// Max age for session affinity entries before eviction.
    affinity_ttl: Duration,
}

impl SessionRouter {
    pub fn new() -> Self {
        Self {
            router: AgentRouter::new(),
            directory: AgentDirectory::new(),
            affinity: FxHashMap::default(),
            sources: FxHashMap::default(),
            breakers: FxHashMap::default(),
            cb_trip_threshold: 5,
            cb_recovery_timeout: Duration::from_secs(30),
            affinity_ttl: Duration::from_secs(3600), // 1 hour
        }
    }

    /// Configure circuit breaker thresholds.
    pub fn with_circuit_breaker(mut self, trip_threshold: u32, recovery_timeout: Duration) -> Self {
        self.cb_trip_threshold = trip_threshold;
        self.cb_recovery_timeout = recovery_timeout;
        self
    }

    /// Configure session affinity TTL.
    pub fn with_affinity_ttl(mut self, ttl: Duration) -> Self {
        self.affinity_ttl = ttl;
        self
    }

    // ─── Registration ────────────────────────────────────────────────────

    /// Register a ClawDesk-native agent.
    pub fn register_clawdesk(&mut self, card: AgentCard) {
        let id = card.id.clone();
        self.sources.insert(id.clone(), AgentSource::ClawDesk);
        self.breakers.insert(
            id,
            CircuitBreaker::new(self.cb_trip_threshold, self.cb_recovery_timeout),
        );
        self.directory.register(card);
    }

    /// Register an OpenClaw gateway agent.
    pub fn register_openclaw(&mut self, card: AgentCard, gateway_url: &str) {
        let id = card.id.clone();
        self.sources.insert(
            id.clone(),
            AgentSource::OpenClaw {
                gateway_url: gateway_url.to_string(),
            },
        );
        self.breakers.insert(
            id,
            CircuitBreaker::new(self.cb_trip_threshold, self.cb_recovery_timeout),
        );
        self.directory.register(card);
    }

    /// Register an externally discovered agent.
    pub fn register_external(&mut self, card: AgentCard, discovery_url: &str) {
        let id = card.id.clone();
        self.sources.insert(
            id.clone(),
            AgentSource::External {
                discovery_url: discovery_url.to_string(),
            },
        );
        self.breakers.insert(
            id,
            CircuitBreaker::new(self.cb_trip_threshold, self.cb_recovery_timeout),
        );
        self.directory.register(card);
    }

    /// Deregister an agent and clean up session affinities.
    pub fn deregister(&mut self, agent_id: &str) {
        self.directory.deregister(agent_id);
        self.sources.remove(agent_id);
        self.breakers.remove(agent_id);
        self.affinity.retain(|_, entry| entry.agent_id != agent_id);
    }

    /// Get the source of an agent.
    pub fn agent_source(&self, agent_id: &str) -> Option<&AgentSource> {
        self.sources.get(agent_id)
    }

    // ─── Session-Aware Routing ───────────────────────────────────────────

    /// Route a task, respecting session affinity.
    ///
    /// 1. Check session affinity table for an existing binding.
    /// 2. Verify the bound agent is healthy and its circuit breaker is closed.
    /// 3. If no affinity or bound agent unavailable, fall through to
    ///    capability-based routing.
    /// 4. Establish new affinity for the selected agent.
    ///
    /// Task 8: Accepts a typed `SessionKey` (channel + identifier). Use
    /// `SessionKey::from(string)` for backward compatibility with bare strings.
    pub fn route_with_session(
        &mut self,
        session_key: &SessionKey,
        required_capabilities: &[AgentCapability],
        exclude_agents: &[String],
    ) -> RoutingDecision {
        // 1. Check affinity (typed SessionKey lookup)
        let session_display = session_key.to_string();
        if let Some(entry) = self.affinity.get(session_key) {
            let agent_id = entry.agent_id.clone();
            let turn_count = entry.turn_count;

            // Check circuit breaker
            let breaker_ok = self
                .breakers
                .get(&agent_id)
                .map_or(true, |cb| !cb.is_open());

            // Check health
            let healthy = self
                .directory
                .get(&agent_id)
                .map_or(false, |e| e.is_healthy);

            if breaker_ok && healthy && !exclude_agents.contains(&agent_id) {
                debug!(
                    session = %session_display,
                    agent = %agent_id,
                    turns = turn_count,
                    "session affinity hit"
                );
                // Update affinity metadata
                let entry = self.affinity.get_mut(session_key).unwrap();
                entry.turn_count += 1;
                entry.last_used = Utc::now();

                return RoutingDecision::Route {
                    agent_id,
                    score: 1.0, // affinity = perfect score
                    reason: format!(
                        "session affinity (turn {})",
                        entry.turn_count
                    ),
                };
            }

            // Affinity invalid — remove stale entry
            debug!(
                session = %session_display,
                agent = %agent_id,
                "session affinity expired or agent unhealthy, re-routing"
            );
            self.affinity.remove(session_key);
        }

        // 2. Capability-based routing (exclude circuit-broken agents)
        let mut effective_exclude: Vec<String> = exclude_agents.to_vec();
        for (agent_id, breaker) in &self.breakers {
            if breaker.is_open() && !effective_exclude.contains(agent_id) {
                effective_exclude.push(agent_id.clone());
            }
        }

        let decision =
            self.router
                .route(&self.directory, required_capabilities, &effective_exclude);

        // 3. Establish affinity for the matched agent
        if let RoutingDecision::Route { ref agent_id, .. } = decision {
            self.affinity.insert(
                session_key.clone(),
                AffinityEntry {
                    agent_id: agent_id.clone(),
                    established_at: Utc::now(),
                    turn_count: 1,
                    last_used: Utc::now(),
                },
            );
            info!(
                session = %session_display,
                agent = %agent_id,
                "established session affinity"
            );
        }

        decision
    }

    // ─── Circuit breaker feedback ────────────────────────────────────────

    /// Record a successful interaction with an agent.
    pub fn record_success(&mut self, agent_id: &str) {
        if let Some(cb) = self.breakers.get_mut(agent_id) {
            cb.record_success();
        }
    }

    /// Record a failed interaction. Returns true if the circuit breaker tripped.
    pub fn record_failure(&mut self, agent_id: &str) -> bool {
        if let Some(cb) = self.breakers.get_mut(agent_id) {
            let tripped = cb.record_failure();
            if tripped {
                warn!(agent = agent_id, "circuit breaker tripped");
                self.directory.update_health(agent_id, false, None);
            }
            tripped
        } else {
            false
        }
    }

    /// Get circuit breaker state for an agent.
    pub fn circuit_breaker(&self, agent_id: &str) -> Option<&CircuitBreaker> {
        self.breakers.get(agent_id)
    }

    // ─── Session management ──────────────────────────────────────────────

    /// Get the current affinity binding for a session.
    pub fn session_agent(&self, session_key: &SessionKey) -> Option<&str> {
        self.affinity.get(session_key).map(|e| e.agent_id.as_str())
    }

    /// Convenience: look up session agent by raw string.
    /// Parses the string into a `SessionKey` first (bare strings → WebChat channel).
    pub fn session_agent_by_str(&self, session_key_str: &str) -> Option<&str> {
        let key = SessionKey::from(session_key_str.to_string());
        self.session_agent(&key)
    }

    /// Break session affinity (e.g., on explicit re-routing).
    pub fn break_affinity(&mut self, session_key: &SessionKey) -> bool {
        self.affinity.remove(session_key).is_some()
    }

    /// Evict stale session affinities older than the TTL.
    pub fn evict_stale_affinities(&mut self) -> usize {
        let cutoff = Utc::now()
            - chrono::Duration::from_std(self.affinity_ttl)
                .unwrap_or(chrono::Duration::seconds(3600));
        let before = self.affinity.len();
        self.affinity
            .retain(|_, entry| entry.last_used > cutoff);
        let evicted = before - self.affinity.len();
        if evicted > 0 {
            debug!(evicted, "evicted stale session affinities");
        }
        evicted
    }

    // ─── Discovery ───────────────────────────────────────────────────────

    /// Snapshot of all registered agents with their source and health.
    pub fn agent_summary(&self) -> Vec<AgentSummary> {
        self.directory
            .agents
            .iter()
            .map(|(id, entry)| {
                let source = self.sources.get(id).cloned().unwrap_or(AgentSource::ClawDesk);
                let breaker = self.breakers.get(id);
                AgentSummary {
                    id: id.clone(),
                    name: entry.card.name.clone(),
                    source,
                    is_healthy: entry.is_healthy,
                    active_tasks: entry.active_tasks,
                    capabilities: entry.card.capabilities.clone(),
                    health_ratio: breaker.map_or(1.0, |cb| cb.health_ratio()),
                    circuit_open: breaker.map_or(false, |cb| cb.is_open()),
                }
            })
            .collect()
    }
}

impl Default for SessionRouter {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary of a registered agent for monitoring/observability.
#[derive(Debug, Clone)]
pub struct AgentSummary {
    pub id: String,
    pub name: String,
    pub source: AgentSource,
    pub is_healthy: bool,
    pub active_tasks: u32,
    pub capabilities: Vec<AgentCapability>,
    pub health_ratio: f64,
    pub circuit_open: bool,
}

// ═══════════════════════════════════════════════════════════════════════════
// Multi-Turn Ping-Pong Protocol
// ═══════════════════════════════════════════════════════════════════════════

/// Configuration for multi-turn agent-to-agent negotiation.
///
/// Enables iterative ping-pong between two agents where they alternate
/// turns until convergence (one signals `done`) or `max_turns` is reached.
///
/// ## Flow
///
/// ```text
/// Agent A ──→ message ──→ Agent B  (turn 1)
///         ←── reply   ←──          (turn 2)
///         ──→ follow-up ──→        (turn 3)
///         ←── final    ←──         (turn 4, status=done)
/// ```
#[derive(Debug, Clone)]
pub struct PingPongConfig {
    /// Maximum number of round-trip turns before forced termination.
    pub max_turns: u32,
    /// Whether to announce the step number in each message (for traceability).
    pub announce_step: bool,
    /// Optional timeout per individual turn (in milliseconds).
    pub turn_timeout_ms: Option<u64>,
}

impl Default for PingPongConfig {
    fn default() -> Self {
        Self {
            max_turns: 6,
            announce_step: true,
            turn_timeout_ms: Some(30_000),
        }
    }
}

/// A single turn in a ping-pong negotiation.
#[derive(Debug, Clone)]
pub struct PingPongTurn {
    /// Which agent sent this message.
    pub sender_agent_id: String,
    /// The message content.
    pub content: String,
    /// Turn number (1-indexed).
    pub turn_number: u32,
    /// Whether this turn signals completion.
    pub is_terminal: bool,
    /// Timestamp of this turn.
    pub timestamp: DateTime<Utc>,
}

/// Outcome of a ping-pong negotiation session.
#[derive(Debug, Clone)]
pub enum PingPongOutcome {
    /// Negotiation completed normally (one side signaled done).
    Converged {
        turns: Vec<PingPongTurn>,
        final_response: String,
    },
    /// Max turns reached without convergence.
    MaxTurnsReached {
        turns: Vec<PingPongTurn>,
        last_response: String,
    },
    /// An error occurred during negotiation.
    Error {
        turns: Vec<PingPongTurn>,
        error: String,
    },
}

impl PingPongOutcome {
    /// Get the final/last response text regardless of outcome type.
    pub fn response_text(&self) -> &str {
        match self {
            Self::Converged { final_response, .. } => final_response,
            Self::MaxTurnsReached { last_response, .. } => last_response,
            Self::Error { error, .. } => error,
        }
    }

    /// Get all turns in the negotiation.
    pub fn turns(&self) -> &[PingPongTurn] {
        match self {
            Self::Converged { turns, .. } => turns,
            Self::MaxTurnsReached { turns, .. } => turns,
            Self::Error { turns, .. } => turns,
        }
    }

    /// Whether the negotiation completed successfully.
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Converged { .. })
    }
}

/// Tracks an active ping-pong negotiation session.
#[derive(Debug)]
pub struct PingPongSession {
    /// Unique session identifier for this negotiation.
    pub session_id: String,
    /// The two agents involved.
    pub agent_a: String,
    pub agent_b: String,
    /// Configuration for this negotiation.
    pub config: PingPongConfig,
    /// Accumulated turns.
    pub turns: Vec<PingPongTurn>,
    /// Which agent should send next (alternates between agent_a and agent_b).
    pub next_sender: String,
    /// Whether the negotiation is still active.
    pub is_active: bool,
}

impl PingPongSession {
    /// Create a new ping-pong session between two agents.
    pub fn new(
        session_id: impl Into<String>,
        agent_a: impl Into<String>,
        agent_b: impl Into<String>,
        config: PingPongConfig,
    ) -> Self {
        let a = agent_a.into();
        let b = agent_b.into();
        let next = a.clone();
        Self {
            session_id: session_id.into(),
            agent_a: a,
            agent_b: b,
            config,
            turns: Vec::new(),
            next_sender: next,
            is_active: true,
        }
    }

    /// Record a turn and advance the protocol.
    ///
    /// Returns whether the session should continue (true) or is terminated (false).
    pub fn record_turn(&mut self, content: String, is_terminal: bool) -> bool {
        let turn_number = self.turns.len() as u32 + 1;
        let sender = self.next_sender.clone();

        let step_content = if self.config.announce_step {
            format!("[Step {}/{}] {}", turn_number, self.config.max_turns, content)
        } else {
            content
        };

        self.turns.push(PingPongTurn {
            sender_agent_id: sender.clone(),
            content: step_content,
            turn_number,
            is_terminal,
            timestamp: Utc::now(),
        });

        // Alternate sender
        self.next_sender = if sender == self.agent_a {
            self.agent_b.clone()
        } else {
            self.agent_a.clone()
        };

        // Check termination conditions
        if is_terminal || turn_number >= self.config.max_turns {
            self.is_active = false;
            return false;
        }

        true
    }

    /// Build the outcome from accumulated turns.
    pub fn into_outcome(self) -> PingPongOutcome {
        let last_response = self
            .turns
            .last()
            .map(|t| t.content.clone())
            .unwrap_or_default();

        let terminated_normally = self
            .turns
            .last()
            .map_or(false, |t| t.is_terminal);

        if terminated_normally {
            PingPongOutcome::Converged {
                final_response: last_response,
                turns: self.turns,
            }
        } else {
            PingPongOutcome::MaxTurnsReached {
                last_response,
                turns: self.turns,
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn make_card(id: &str, caps: Vec<AgentCapability>) -> AgentCard {
        let mut card = AgentCard::new(id, id, format!("http://{}.local", id));
        card.capabilities = caps;
        card
    }

    fn sk(s: &str) -> SessionKey {
        SessionKey::from(s.to_string())
    }

    #[test]
    fn session_affinity_routes_to_same_agent() {
        let mut sr = SessionRouter::new();
        sr.register_clawdesk(make_card(
            "agent-a",
            vec![AgentCapability::TextGeneration],
        ));
        sr.register_clawdesk(make_card(
            "agent-b",
            vec![AgentCapability::TextGeneration],
        ));

        // First route establishes affinity
        let key = sk("sess-1");
        let d1 = sr.route_with_session(&key, &[AgentCapability::TextGeneration], &[]);
        let first_agent = match &d1 {
            RoutingDecision::Route { agent_id, .. } => agent_id.clone(),
            _ => panic!("expected route"),
        };

        // Second route hits affinity
        let d2 = sr.route_with_session(&key, &[AgentCapability::TextGeneration], &[]);
        match d2 {
            RoutingDecision::Route { agent_id, reason, .. } => {
                assert_eq!(agent_id, first_agent);
                assert!(reason.contains("affinity"));
            }
            _ => panic!("expected route"),
        }
    }

    #[test]
    fn circuit_breaker_trips_after_threshold() {
        let mut sr = SessionRouter::new().with_circuit_breaker(3, Duration::from_secs(60));
        sr.register_clawdesk(make_card(
            "flaky",
            vec![AgentCapability::TextGeneration],
        ));
        sr.register_clawdesk(make_card(
            "stable",
            vec![AgentCapability::TextGeneration],
        ));

        // Trip the breaker for "flaky"
        assert!(!sr.record_failure("flaky"));
        assert!(!sr.record_failure("flaky"));
        assert!(sr.record_failure("flaky")); // trips on 3rd failure

        // Now routing should exclude "flaky"
        let decision = sr.route_with_session(&sk("sess-2"), &[AgentCapability::TextGeneration], &[]);
        match decision {
            RoutingDecision::Route { agent_id, .. } => {
                assert_eq!(agent_id, "stable");
            }
            _ => panic!("expected route to stable"),
        }
    }

    #[test]
    fn openclaw_agents_are_routable() {
        let mut sr = SessionRouter::new();
        sr.register_openclaw(
            make_card("oc-writer", vec![AgentCapability::TextGeneration]),
            "http://openclaw:3000",
        );
        sr.register_clawdesk(make_card(
            "cd-coder",
            vec![AgentCapability::CodeExecution],
        ));

        // Route to OC agent for text generation
        let decision = sr.route_with_session(&sk("sess-3"), &[AgentCapability::TextGeneration], &[]);
        match &decision {
            RoutingDecision::Route { agent_id, .. } => {
                assert_eq!(agent_id, "oc-writer");
            }
            _ => panic!("expected route"),
        }
        assert_eq!(
            sr.agent_source("oc-writer"),
            Some(&AgentSource::OpenClaw {
                gateway_url: "http://openclaw:3000".into()
            })
        );
    }

    #[test]
    fn break_affinity_allows_reroute() {
        let mut sr = SessionRouter::new();
        sr.register_clawdesk(make_card(
            "agent-x",
            vec![AgentCapability::TextGeneration],
        ));

        // Establish affinity
        let key = sk("sess-4");
        sr.route_with_session(&key, &[AgentCapability::TextGeneration], &[]);
        assert!(sr.session_agent(&key).is_some());

        // Break affinity
        assert!(sr.break_affinity(&key));
        assert!(sr.session_agent(&key).is_none());
    }

    #[test]
    fn evict_stale_sessions() {
        let mut sr = SessionRouter::new().with_affinity_ttl(Duration::from_secs(0));
        sr.register_clawdesk(make_card(
            "agent-y",
            vec![AgentCapability::TextGeneration],
        ));
        let key = sk("old-session");
        sr.route_with_session(&key, &[AgentCapability::TextGeneration], &[]);

        // Evict immediately (TTL = 0)
        let evicted = sr.evict_stale_affinities();
        assert_eq!(evicted, 1);
        assert!(sr.session_agent(&key).is_none());
    }

    #[test]
    fn agent_summary_includes_all_sources() {
        let mut sr = SessionRouter::new();
        sr.register_clawdesk(make_card("cd-1", vec![AgentCapability::TextGeneration]));
        sr.register_openclaw(
            make_card("oc-1", vec![AgentCapability::WebSearch]),
            "http://oc:3000",
        );
        sr.register_external(
            make_card("ext-1", vec![AgentCapability::Mathematics]),
            "http://ext.agent/.well-known/agent.json",
        );

        let summary = sr.agent_summary();
        assert_eq!(summary.len(), 3);

        let sources: Vec<_> = summary.iter().map(|s| &s.source).collect();
        assert!(sources.contains(&&AgentSource::ClawDesk));
        assert!(sources.iter().any(|s| matches!(s, AgentSource::OpenClaw { .. })));
        assert!(sources.iter().any(|s| matches!(s, AgentSource::External { .. })));
    }
}
