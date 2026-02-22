//! A2A Discovery Federation — connect mDNS discovery to AgentCard exchange.
//!
//! ## Architecture
//!
//! ClawDesk instances discover each other via mDNS (`_clawdesk._tcp.local.`).
//! Once a peer is discovered, this module:
//!
//! 1. Fetches `GET http://{host}:{port}/.well-known/agent.json` to retrieve
//!    the peer's `AgentCard`.
//! 2. Validates the card and checks trust (only SPAKE2-paired peers are
//!    eligible for task dispatch).
//! 3. Registers the agent in the local routing table.
//! 4. Propagates health status changes (peer goes unreachable → deregister).
//!
//! ## Why not a cloud bridge?
//!
//! ClawDesk is the desktop server. Building a client that polls a cloud API
//! is architecturally backwards. Instead, we serve `/.well-known/agent.json`
//! ourselves and discover other ClawDesk instances via mDNS — true
//! peer-to-peer federation with no external dependency.
//!
//! ## Trust model
//!
//! ```text
//! ┌──────────────────┐         ┌──────────────────┐
//! │  ClawDesk-A       │  mDNS  │  ClawDesk-B       │
//! │                   │◄──────►│                   │
//! │  PeerRegistry     │        │  PeerRegistry     │
//! │  ├─ status:Paired │        │  ├─ status:Paired │
//! │  └─ trust: ✓      │        │  └─ trust: ✓      │
//! │                   │        │                   │
//! │  AgentCard fetch  │        │  AgentCard fetch  │
//! │  GET /agent.json ─┼───────►│  (serves card)    │
//! │  (registers card) │        │                   │
//! └──────────────────┘         └──────────────────┘
//! ```

use crate::registry::{Peer, PeerRegistry, PeerStatus};
use clawdesk_acp::agent_card::{AgentCard, AgentCapability};

use std::collections::HashMap;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};

// ═══════════════════════════════════════════════════════════════════════════
// Federation configuration
// ═══════════════════════════════════════════════════════════════════════════

/// Configuration for the federation engine.
#[derive(Debug, Clone)]
pub struct FederationConfig {
    /// How often to poll discovered peers for their AgentCard.
    pub poll_interval: Duration,
    /// Timeout for HTTP requests to fetch agent cards.
    pub fetch_timeout: Duration,
    /// Only register agents from peers that have been SPAKE2-paired.
    pub require_pairing: bool,
    /// Maximum number of federated agents to track.
    pub max_agents: usize,
    /// How long to keep a stale agent before deregistering.
    pub stale_timeout: Duration,
}

impl Default for FederationConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(30),
            fetch_timeout: Duration::from_secs(5),
            require_pairing: true,
            max_agents: 64,
            stale_timeout: Duration::from_secs(120),
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Federated agent entry
// ═══════════════════════════════════════════════════════════════════════════

/// A federated agent discovered via mDNS + AgentCard exchange.
#[derive(Debug, Clone)]
pub struct FederatedAgent {
    /// The agent card fetched from the peer.
    pub card: AgentCard,
    /// The peer that hosts this agent.
    pub peer_id: String,
    /// When the card was last successfully fetched.
    pub last_fetched: Instant,
    /// Whether the peer is trusted (paired via SPAKE2).
    pub trusted: bool,
    /// The base URL used to fetch the card.
    pub endpoint_url: String,
}

/// Result of a federation sync cycle.
#[derive(Debug, Default)]
pub struct FederationSyncResult {
    /// Number of new agents registered.
    pub registered: usize,
    /// Number of agents updated (card changed).
    pub updated: usize,
    /// Number of agents deregistered (peer went stale).
    pub deregistered: usize,
    /// Agents that failed to fetch.
    pub fetch_errors: Vec<(String, String)>,
}

// ═══════════════════════════════════════════════════════════════════════════
// Card fetcher trait
// ═══════════════════════════════════════════════════════════════════════════

/// Abstraction for fetching agent cards from peers.
///
/// In production, this wraps an HTTP client hitting
/// `GET http://{host}:{port}/.well-known/agent.json`.
/// In tests, a mock can return static cards.
pub trait CardFetcher: Send + Sync {
    /// Fetch the agent card from the given URL.
    fn fetch_card(
        &self,
        url: &str,
        timeout: Duration,
    ) -> Result<AgentCard, FederationError>;
}

// ═══════════════════════════════════════════════════════════════════════════
// Federation error
// ═══════════════════════════════════════════════════════════════════════════

/// Errors during federation operations.
#[derive(Debug)]
pub enum FederationError {
    /// HTTP fetch failed.
    FetchFailed { url: String, detail: String },
    /// Card validation failed.
    InvalidCard { agent_id: String, detail: String },
    /// Peer is not trusted (not paired).
    Untrusted { peer_id: String },
    /// Maximum agent limit reached.
    CapacityExceeded { max: usize },
}

impl std::fmt::Display for FederationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::FetchFailed { url, detail } => {
                write!(f, "failed to fetch card from {url}: {detail}")
            }
            Self::InvalidCard { agent_id, detail } => {
                write!(f, "invalid card for agent {agent_id}: {detail}")
            }
            Self::Untrusted { peer_id } => {
                write!(f, "peer {peer_id} is not trusted (pairing required)")
            }
            Self::CapacityExceeded { max } => {
                write!(f, "federation capacity exceeded (max {max} agents)")
            }
        }
    }
}

impl std::error::Error for FederationError {}

// ═══════════════════════════════════════════════════════════════════════════
// Federation engine
// ═══════════════════════════════════════════════════════════════════════════

/// The federation engine connects mDNS discovery to A2A AgentCard exchange.
///
/// It watches the `PeerRegistry` for new/changed peers, fetches their
/// AgentCards, validates trust, and maintains a local table of federated
/// agents that can be registered with the `SessionRouter`.
pub struct FederationEngine<F: CardFetcher> {
    config: FederationConfig,
    fetcher: F,
    /// Federated agents keyed by agent ID.
    agents: HashMap<String, FederatedAgent>,
    /// Mapping from peer_id → list of agent IDs hosted by that peer.
    peer_agents: HashMap<String, Vec<String>>,
}

impl<F: CardFetcher> FederationEngine<F> {
    /// Create a new federation engine.
    pub fn new(config: FederationConfig, fetcher: F) -> Self {
        Self {
            config,
            fetcher,
            agents: HashMap::new(),
            peer_agents: HashMap::new(),
        }
    }

    /// Get all currently federated agents.
    pub fn federated_agents(&self) -> &HashMap<String, FederatedAgent> {
        &self.agents
    }

    /// Get the number of federated agents.
    pub fn agent_count(&self) -> usize {
        self.agents.len()
    }

    /// Check if a specific agent is federated.
    pub fn has_agent(&self, agent_id: &str) -> bool {
        self.agents.contains_key(agent_id)
    }

    /// Get a federated agent by ID.
    pub fn get_agent(&self, agent_id: &str) -> Option<&FederatedAgent> {
        self.agents.get(agent_id)
    }

    /// Perform a federation sync cycle.
    ///
    /// 1. Iterates active peers in the registry.
    /// 2. For eligible peers (paired if `require_pairing` is set), fetches
    ///    the AgentCard from `http://{host}:{port}/.well-known/agent.json`.
    /// 3. Registers new agents, updates changed ones.
    /// 4. Deregisters agents whose peers are no longer active.
    pub fn sync(&mut self, registry: &PeerRegistry) -> FederationSyncResult {
        let mut result = FederationSyncResult::default();

        // Collect eligible peers
        let active = registry.active_peers();
        let eligible: Vec<&Peer> = active
            .into_iter()
            .filter(|p| {
                if self.config.require_pairing {
                    p.paired
                } else {
                    true
                }
            })
            .collect();

        let active_peer_ids: std::collections::HashSet<String> =
            eligible.iter().map(|p| p.id.clone()).collect();

        // Fetch cards from eligible peers
        for peer in &eligible {
            let card_url = format!(
                "http://{}:{}/.well-known/agent.json",
                peer.host, peer.port
            );

            match self.fetcher.fetch_card(&card_url, self.config.fetch_timeout) {
                Ok(card) => {
                    if let Err(e) = validate_card(&card) {
                        warn!(
                            peer_id = %peer.id,
                            error = %e,
                            "invalid agent card from peer"
                        );
                        result.fetch_errors.push((peer.id.clone(), e.to_string()));
                        continue;
                    }

                    if self.agents.len() >= self.config.max_agents
                        && !self.agents.contains_key(&card.id)
                    {
                        debug!(
                            max = self.config.max_agents,
                            "federation capacity reached, skipping"
                        );
                        continue;
                    }

                    let is_update = self.agents.contains_key(&card.id);
                    let agent_id = card.id.clone();

                    let federated = FederatedAgent {
                        card,
                        peer_id: peer.id.clone(),
                        last_fetched: Instant::now(),
                        trusted: peer.paired,
                        endpoint_url: card_url,
                    };

                    self.agents.insert(agent_id.clone(), federated);

                    // Track peer→agent mapping
                    self.peer_agents
                        .entry(peer.id.clone())
                        .or_default()
                        .retain(|id| id != &agent_id);
                    self.peer_agents
                        .entry(peer.id.clone())
                        .or_default()
                        .push(agent_id);

                    if is_update {
                        result.updated += 1;
                    } else {
                        result.registered += 1;
                        info!(
                            agent_id = %self.agents.keys().last().unwrap(),
                            peer_id = %peer.id,
                            "federated agent registered"
                        );
                    }
                }
                Err(e) => {
                    debug!(
                        peer_id = %peer.id,
                        url = %card_url,
                        error = %e,
                        "failed to fetch agent card"
                    );
                    result.fetch_errors.push((peer.id.clone(), e.to_string()));
                }
            }
        }

        // Deregister agents whose peers are no longer active
        let stale_peers: Vec<String> = self
            .peer_agents
            .keys()
            .filter(|pid| !active_peer_ids.contains(*pid))
            .cloned()
            .collect();

        for peer_id in stale_peers {
            if let Some(agent_ids) = self.peer_agents.remove(&peer_id) {
                for agent_id in &agent_ids {
                    if self.agents.remove(agent_id).is_some() {
                        result.deregistered += 1;
                        info!(
                            agent_id = %agent_id,
                            peer_id = %peer_id,
                            "federated agent deregistered (peer stale)"
                        );
                    }
                }
            }
        }

        result
    }

    /// Deregister all agents from a specific peer.
    pub fn deregister_peer(&mut self, peer_id: &str) -> usize {
        let mut count = 0;
        if let Some(agent_ids) = self.peer_agents.remove(peer_id) {
            for agent_id in &agent_ids {
                if self.agents.remove(agent_id).is_some() {
                    count += 1;
                }
            }
        }
        count
    }

    /// Prune agents that haven't been refreshed within the stale timeout.
    pub fn prune_stale(&mut self) -> usize {
        let stale_cutoff = self.config.stale_timeout;
        let stale_ids: Vec<String> = self
            .agents
            .iter()
            .filter(|(_, a)| a.last_fetched.elapsed() > stale_cutoff)
            .map(|(id, _)| id.clone())
            .collect();

        let count = stale_ids.len();
        for id in &stale_ids {
            self.agents.remove(id);
        }

        // Clean up peer_agents mappings
        for agents in self.peer_agents.values_mut() {
            agents.retain(|id| !stale_ids.contains(id));
        }
        self.peer_agents.retain(|_, agents| !agents.is_empty());

        count
    }

    /// Get all agent cards suitable for registration with SessionRouter.
    ///
    /// Returns `(AgentCard, endpoint_url)` pairs for trusted agents only.
    pub fn cards_for_registration(&self) -> Vec<(&AgentCard, &str)> {
        self.agents
            .values()
            .filter(|a| a.trusted)
            .map(|a| (&a.card, a.endpoint_url.as_str()))
            .collect()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Card validation
// ═══════════════════════════════════════════════════════════════════════════

/// Validate an agent card for minimum requirements.
fn validate_card(card: &AgentCard) -> Result<(), FederationError> {
    if card.id.is_empty() {
        return Err(FederationError::InvalidCard {
            agent_id: "(empty)".into(),
            detail: "agent ID is empty".into(),
        });
    }
    if card.name.is_empty() {
        return Err(FederationError::InvalidCard {
            agent_id: card.id.clone(),
            detail: "agent name is empty".into(),
        });
    }
    if card.endpoint.url.is_empty() {
        return Err(FederationError::InvalidCard {
            agent_id: card.id.clone(),
            detail: "endpoint URL is empty".into(),
        });
    }
    if card.capabilities.is_empty() && card.skills.is_empty() {
        return Err(FederationError::InvalidCard {
            agent_id: card.id.clone(),
            detail: "no capabilities or skills declared".into(),
        });
    }
    Ok(())
}

/// Build the well-known URL from a peer's host and port.
pub fn well_known_url(host: &str, port: u16) -> String {
    format!("http://{host}:{port}/.well-known/agent.json")
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;
    use clawdesk_acp::agent_card::{AgentAuth, AgentEndpoint};

    /// Mock card fetcher that returns pre-configured cards.
    struct MockFetcher {
        cards: HashMap<String, AgentCard>,
    }

    impl MockFetcher {
        fn new() -> Self {
            Self {
                cards: HashMap::new(),
            }
        }

        fn add_card(&mut self, url: &str, card: AgentCard) {
            self.cards.insert(url.to_string(), card);
        }
    }

    impl CardFetcher for MockFetcher {
        fn fetch_card(
            &self,
            url: &str,
            _timeout: Duration,
        ) -> Result<AgentCard, FederationError> {
            self.cards.get(url).cloned().ok_or(FederationError::FetchFailed {
                url: url.into(),
                detail: "not found".into(),
            })
        }
    }

    fn make_card(id: &str) -> AgentCard {
        AgentCard {
            id: id.to_string(),
            name: format!("Agent {id}"),
            description: format!("Test agent {id}"),
            version: "1.0.0".to_string(),
            endpoint: AgentEndpoint {
                url: format!("http://{id}.local"),
                supports_streaming: false,
                supports_push: false,
                push_url: None,
            },
            auth: AgentAuth::None,
            capabilities: vec![AgentCapability::TextGeneration],
            skills: vec![],
            protocol_versions: vec!["1.0".into()],
            max_concurrent_tasks: Some(10),
            metadata: serde_json::Value::Null,
        }
    }

    fn make_peer(id: &str, host: &str, port: u16, paired: bool) -> Peer {
        Peer {
            id: id.to_string(),
            name: format!("Peer {id}"),
            host: host.to_string(),
            port,
            version: "0.1.0".to_string(),
            status: if paired {
                PeerStatus::Paired
            } else {
                PeerStatus::Discovered
            },
            capabilities: vec!["chat".into()],
            last_seen: None,
            paired,
        }
    }

    fn make_registry(peers: Vec<Peer>) -> PeerRegistry {
        let mut reg = PeerRegistry::new(Duration::from_secs(60));
        for peer in peers {
            reg.upsert(peer);
        }
        reg
    }

    #[test]
    fn sync_discovers_and_registers_agent() {
        let peer = make_peer("peer-1", "192.168.1.10", 18789, true);
        let registry = make_registry(vec![peer]);

        let mut fetcher = MockFetcher::new();
        fetcher.add_card(
            "http://192.168.1.10:18789/.well-known/agent.json",
            make_card("remote-agent"),
        );

        let config = FederationConfig {
            require_pairing: true,
            ..FederationConfig::default()
        };

        let mut engine = FederationEngine::new(config, fetcher);
        let result = engine.sync(&registry);

        assert_eq!(result.registered, 1);
        assert_eq!(result.deregistered, 0);
        assert!(engine.has_agent("remote-agent"));
        assert!(engine.get_agent("remote-agent").unwrap().trusted);
    }

    #[test]
    fn sync_skips_unpaired_peers_when_required() {
        let peer = make_peer("peer-1", "192.168.1.10", 18789, false);
        let registry = make_registry(vec![peer]);

        let mut fetcher = MockFetcher::new();
        fetcher.add_card(
            "http://192.168.1.10:18789/.well-known/agent.json",
            make_card("remote-agent"),
        );

        let config = FederationConfig {
            require_pairing: true,
            ..FederationConfig::default()
        };

        let mut engine = FederationEngine::new(config, fetcher);
        let result = engine.sync(&registry);

        assert_eq!(result.registered, 0);
        assert!(!engine.has_agent("remote-agent"));
    }

    #[test]
    fn sync_allows_unpaired_when_not_required() {
        let peer = make_peer("peer-1", "192.168.1.10", 18789, false);
        let registry = make_registry(vec![peer]);

        let mut fetcher = MockFetcher::new();
        fetcher.add_card(
            "http://192.168.1.10:18789/.well-known/agent.json",
            make_card("remote-agent"),
        );

        let config = FederationConfig {
            require_pairing: false,
            ..FederationConfig::default()
        };

        let mut engine = FederationEngine::new(config, fetcher);
        let result = engine.sync(&registry);

        assert_eq!(result.registered, 1);
    }

    #[test]
    fn sync_deregisters_stale_peers() {
        // Start with a peer
        let peer = make_peer("peer-1", "192.168.1.10", 18789, true);
        let registry = make_registry(vec![peer]);

        let mut fetcher = MockFetcher::new();
        fetcher.add_card(
            "http://192.168.1.10:18789/.well-known/agent.json",
            make_card("agent-1"),
        );

        let config = FederationConfig::default();
        let mut engine = FederationEngine::new(config, fetcher);

        // First sync registers the agent
        let r1 = engine.sync(&registry);
        assert_eq!(r1.registered, 1);
        assert!(engine.has_agent("agent-1"));

        // Second sync with empty registry → agent deregistered
        let empty_registry = make_registry(vec![]);
        let r2 = engine.sync(&empty_registry);
        assert_eq!(r2.deregistered, 1);
        assert!(!engine.has_agent("agent-1"));
    }

    #[test]
    fn sync_handles_fetch_errors_gracefully() {
        let peer = make_peer("peer-1", "192.168.1.10", 18789, true);
        let registry = make_registry(vec![peer]);

        // No cards configured → fetch will fail
        let fetcher = MockFetcher::new();
        let config = FederationConfig::default();

        let mut engine = FederationEngine::new(config, fetcher);
        let result = engine.sync(&registry);

        assert_eq!(result.registered, 0);
        assert_eq!(result.fetch_errors.len(), 1);
    }

    #[test]
    fn validate_card_rejects_empty_id() {
        let mut card = make_card("test");
        card.id = String::new();
        assert!(validate_card(&card).is_err());
    }

    #[test]
    fn validate_card_rejects_no_capabilities() {
        let mut card = make_card("test");
        card.capabilities.clear();
        card.skills.clear();
        assert!(validate_card(&card).is_err());
    }

    #[test]
    fn deregister_peer_removes_all_agents() {
        let peer = make_peer("peer-1", "192.168.1.10", 18789, true);
        let registry = make_registry(vec![peer]);

        let mut fetcher = MockFetcher::new();
        fetcher.add_card(
            "http://192.168.1.10:18789/.well-known/agent.json",
            make_card("agent-1"),
        );

        let config = FederationConfig::default();
        let mut engine = FederationEngine::new(config, fetcher);

        engine.sync(&registry);
        assert_eq!(engine.agent_count(), 1);

        let removed = engine.deregister_peer("peer-1");
        assert_eq!(removed, 1);
        assert_eq!(engine.agent_count(), 0);
    }

    #[test]
    fn cards_for_registration_only_trusted() {
        let peer_trusted = make_peer("peer-1", "192.168.1.10", 18789, true);
        let peer_untrusted = make_peer("peer-2", "192.168.1.20", 18789, false);
        let registry = make_registry(vec![peer_trusted, peer_untrusted]);

        let mut fetcher = MockFetcher::new();
        fetcher.add_card(
            "http://192.168.1.10:18789/.well-known/agent.json",
            make_card("trusted-agent"),
        );
        fetcher.add_card(
            "http://192.168.1.20:18789/.well-known/agent.json",
            make_card("untrusted-agent"),
        );

        let config = FederationConfig {
            require_pairing: false, // allow both to register
            ..FederationConfig::default()
        };

        let mut engine = FederationEngine::new(config, fetcher);
        engine.sync(&registry);

        // Both registered but only trusted is returned for registration
        assert_eq!(engine.agent_count(), 2);
        let for_reg = engine.cards_for_registration();
        assert_eq!(for_reg.len(), 1);
        assert_eq!(for_reg[0].0.id, "trusted-agent");
    }

    #[test]
    fn well_known_url_format() {
        let url = well_known_url("192.168.1.42", 18789);
        assert_eq!(url, "http://192.168.1.42:18789/.well-known/agent.json");
    }

    #[test]
    fn capacity_limit_respected() {
        let peer = make_peer("peer-1", "192.168.1.10", 18789, true);
        let registry = make_registry(vec![peer]);

        let mut fetcher = MockFetcher::new();
        fetcher.add_card(
            "http://192.168.1.10:18789/.well-known/agent.json",
            make_card("agent-1"),
        );

        let config = FederationConfig {
            max_agents: 0, // capacity = 0
            ..FederationConfig::default()
        };

        let mut engine = FederationEngine::new(config, fetcher);
        let result = engine.sync(&registry);

        // Should not register due to capacity
        assert_eq!(result.registered, 0);
        assert_eq!(engine.agent_count(), 0);
    }
}
