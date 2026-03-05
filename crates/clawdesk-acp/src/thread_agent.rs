//! Thread-as-Agent bridge — makes every chat thread an A2A-capable agent.
//!
//! ## Design principle
//!
//! Every session belongs to exactly one agent and the session key encodes
//! `agent:{id}:{rest}`. Threads already carry an `agent_id` — this module
//! closes the gap by:
//!
//! 1. **Per-thread AgentCard generation**: each thread gets its own capability
//!    advertisement derived from its `ThreadMeta` + optional agent config.
//! 2. **Agent-scoped session keys**: `agent:{agent_id}:{thread_hex}` format
//!    for A2A routing.
//! 3. **Sub-agent spawning**: create a child thread as a sub-agent with
//!    A2A task delegation (run or session mode).
//! 4. **Result announcement**: deliver sub-agent results back to the parent
//!    thread via the `AnnounceRouter`.
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────────┐
//! │               ThreadAgentBridge                       │
//! │                                                      │
//! │  ThreadMeta ─→ AgentCard  (per-thread card)          │
//! │  ThreadMeta ─→ SessionKey (agent-scoped key)         │
//! │                                                      │
//! │  spawn_subagent_thread()                             │
//! │    ├─ create child ThreadMeta (spawn_mode=run|session)│
//! │    ├─ generate child AgentCard                       │
//! │    ├─ register in SubAgentManager                    │
//! │    └─ create A2A Task (thread_id bound)              │
//! │                                                      │
//! │  announce_to_parent()                                │
//! │    └─ AnnounceRouter.deliver(parent_thread)          │
//! └──────────────────────────────────────────────────────┘
//! ```

use crate::agent_card::{AgentCard, AgentEndpoint, AgentAuth, AgentSkill};
use crate::capability::CapabilityId;
use crate::task::{CleanupPolicy, SpawnMode, Task};
use serde::{Deserialize, Serialize};

// ═══════════════════════════════════════════════════════════════════════════
// Agent-scoped session key
// ═══════════════════════════════════════════════════════════════════════════

/// Build an agent-scoped session key from agent ID and thread ID.
///
/// Format: `agent:{agent_id}:{thread_hex}`.
pub fn agent_session_key(agent_id: &str, thread_id: u128) -> String {
    format!("agent:{}:{:032x}", agent_id, thread_id)
}

/// Parse an agent-scoped session key back into (agent_id, thread_hex).
///
/// Returns `None` if the key is not in the expected format.
pub fn parse_agent_session_key(key: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = key.splitn(3, ':').collect();
    if parts.len() == 3 && parts[0] == "agent" {
        Some((parts[1].to_string(), parts[2].to_string()))
    } else {
        None
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Per-thread AgentCard generation
// ═══════════════════════════════════════════════════════════════════════════

/// Configuration for a thread-agent (optional per-thread overrides).
///
/// Per-thread agent overrides (name, model, capabilities, limits).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThreadAgentConfig {
    /// Display name for this agent (defaults to thread title).
    pub name: Option<String>,
    /// Description of what this agent does.
    pub description: Option<String>,
    /// Model override (e.g. "claude-sonnet-4-20250514").
    pub model: Option<String>,
    /// Capabilities this agent offers.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Skills this agent can perform (IDs from the skill registry).
    #[serde(default)]
    pub skills: Vec<String>,
    /// Maximum concurrent tasks this agent can handle.
    pub max_concurrent_tasks: Option<u32>,
    /// Maximum sub-agent spawn depth.
    pub max_depth: Option<u32>,
    /// Maximum concurrent sub-agents.
    pub max_concurrent_subagents: Option<usize>,
}

/// Input data for generating a thread's `AgentCard`.
///
/// This struct decouples the bridge from the `clawdesk-threads` crate
/// (which lives in a different workspace member) to avoid circular deps.
#[derive(Debug, Clone)]
pub struct ThreadInfo {
    pub thread_id: u128,
    pub agent_id: String,
    pub title: String,
    pub model: Option<String>,
    pub capabilities: Vec<String>,
    pub skills: Vec<String>,
    pub spawn_mode: String,
    pub parent_thread_id: Option<u128>,
}

/// Generate an `AgentCard` for a thread-agent.
///
/// Every thread is an agent. This function synthesizes the card from the
/// thread's metadata and optional agent config overrides.
pub fn thread_agent_card(
    info: &ThreadInfo,
    config: Option<&ThreadAgentConfig>,
    gateway_base_url: &str,
) -> AgentCard {
    let agent_id = format!("thread:{}", info.agent_id);
    let name = config
        .and_then(|c| c.name.clone())
        .unwrap_or_else(|| info.title.clone());
    let description = config
        .and_then(|c| c.description.clone())
        .unwrap_or_else(|| format!("Thread agent: {}", info.title));

    // Map capability strings → CapabilityId enum (unified type system)
    let cap_source = config
        .map(|c| &c.capabilities)
        .filter(|c| !c.is_empty())
        .unwrap_or(&info.capabilities);

    let capabilities: Vec<CapabilityId> = cap_source
        .iter()
        .filter_map(|s| CapabilityId::from_str_loose(s))
        .collect();

    // Default: every thread-agent can at least generate text
    let capabilities = if capabilities.is_empty() {
        vec![CapabilityId::TextGeneration]
    } else {
        capabilities
    };

    let max_tasks = config
        .and_then(|c| c.max_concurrent_tasks)
        .unwrap_or(5);

    let cap_set = {
        let mut cs = crate::capability::CapSet::empty();
        for cap in &capabilities {
            cs.insert(*cap);
        }
        cs.close()
    };

    AgentCard {
        id: agent_id,
        name,
        description,
        version: "0.1.0".to_string(),
        endpoint: AgentEndpoint {
            url: format!("{}/a2a", gateway_base_url.trim_end_matches('/')),
            supports_streaming: true,
            supports_push: false,
            push_url: None,
        },
        auth: AgentAuth::None,
        capabilities,
        cap_set: {
            let lock = std::sync::OnceLock::new();
            let _ = lock.set(cap_set);
            lock
        },
        skills: vec![], // Skills are wired separately via SkillWiring
        protocol_versions: vec!["1.0".to_string()],
        max_concurrent_tasks: Some(max_tasks),
        metadata: serde_json::json!({
            "thread_id": format!("{:032x}", info.thread_id),
            "spawn_mode": info.spawn_mode,
            "parent_thread_id": info.parent_thread_id.map(|id| format!("{:032x}", id)),
        }),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Sub-agent thread spawning
// ═══════════════════════════════════════════════════════════════════════════

/// Parameters for spawning a sub-agent thread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnRequest {
    /// Agent ID for the child thread-agent.
    pub child_agent_id: String,
    /// Human-readable title for the child thread.
    pub title: String,
    /// The task/prompt to give the sub-agent.
    pub task_prompt: String,
    /// Spawn mode: fire-and-forget or persistent.
    #[serde(default)]
    pub spawn_mode: SpawnMode,
    /// Cleanup policy after task completion.
    #[serde(default)]
    pub cleanup: CleanupPolicy,
    /// Model override for the child agent.
    pub model: Option<String>,
    /// Capabilities for the child agent.
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Skills for the child agent.
    #[serde(default)]
    pub skills: Vec<String>,
}

/// Result of a spawn operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnResult {
    /// The child thread ID.
    pub child_thread_id: u128,
    /// The child's agent-scoped session key.
    pub child_session_key: String,
    /// The A2A task created for the delegation.
    pub task: Task,
}

/// Create an A2A task for a sub-agent thread spawn.
///
/// This wires up the task with the correct thread bindings and session keys.
/// The caller is responsible for:
/// 1. Creating the child `ThreadMeta` with the returned `child_thread_id`
/// 2. Registering in `SubAgentManager` via `spawn_from_thread()`
/// 3. Actually executing the task (via `AgentRunner` or remote dispatch)
pub fn create_spawn_task(
    parent_agent_id: &str,
    parent_thread_id: u128,
    child_thread_id: u128,
    req: &SpawnRequest,
) -> SpawnResult {
    let child_session_key = agent_session_key(&req.child_agent_id, child_thread_id);
    let parent_session_key = agent_session_key(parent_agent_id, parent_thread_id);

    let mut task = Task::for_thread(
        parent_agent_id,
        &req.child_agent_id,
        serde_json::json!({
            "prompt": req.task_prompt,
            "spawn_mode": req.spawn_mode,
        }),
        child_thread_id,
        req.spawn_mode,
    );
    task.session_key = Some(child_session_key.clone());
    task.cleanup = req.cleanup;
    task.announce_on_complete = req.spawn_mode.announces_on_complete();

    SpawnResult {
        child_thread_id,
        child_session_key,
        task,
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Thread agent card registry (in-memory)
// ═══════════════════════════════════════════════════════════════════════════

use dashmap::DashMap;

/// Registry of per-thread agent cards.
///
/// Replaces the single global `AgentCard::new("clawdesk", ...)` with a
/// per-thread-agent card registry. Each thread gets its own card derived
/// from its `ThreadInfo` + optional `ThreadAgentConfig`.
///
/// Supports both `u128` numeric thread IDs (internal) and string-based
/// thread IDs (gateway UUID convention) for lookup.
///
/// Uses `DashMap` for per-shard concurrent access instead of a global
/// `RwLock<HashMap>`, reducing contention in multi-thread scenarios.
pub struct ThreadAgentRegistry {
    /// thread_id (hex or string) → AgentCard
    cards: DashMap<String, AgentCard>,
    /// Gateway base URL for endpoint generation.
    gateway_base_url: String,
}

impl ThreadAgentRegistry {
    pub fn new(gateway_base_url: impl Into<String>) -> Self {
        Self {
            cards: DashMap::new(),
            gateway_base_url: gateway_base_url.into(),
        }
    }

    /// Register or update a thread's agent card from `ThreadInfo`.
    pub fn upsert(
        &self,
        info: &ThreadInfo,
        config: Option<&ThreadAgentConfig>,
    ) {
        let card = thread_agent_card(info, config, &self.gateway_base_url);
        let key = format!("{:032x}", info.thread_id);
        self.cards.insert(key, card);
    }

    /// Register or update a thread's agent card directly, keyed by agent_id.
    pub fn upsert_card(&self, agent_id: &str, card: AgentCard) {
        self.cards.insert(agent_id.to_string(), card);
    }

    /// Remove a thread's agent card by numeric ID.
    pub fn remove(&self, thread_id: u128) {
        let key = format!("{:032x}", thread_id);
        self.cards.remove(&key);
    }

    /// Remove a thread's agent card by string key.
    pub fn remove_by_key(&self, key: &str) {
        self.cards.remove(key);
    }

    /// Get a thread's agent card by numeric ID.
    pub fn get(&self, thread_id: u128) -> Option<AgentCard> {
        let key = format!("{:032x}", thread_id);
        self.cards.get(&key).map(|r| r.value().clone())
    }

    /// Get a thread's agent card by string key (agent_id or thread UUID).
    pub fn get_by_key(&self, key: &str) -> Option<AgentCard> {
        self.cards.get(key).map(|r| r.value().clone())
    }

    /// Get all registered agent cards.
    pub fn all_cards(&self) -> Vec<AgentCard> {
        self.cards.iter().map(|r| r.value().clone()).collect()
    }

    /// Number of registered thread-agents.
    pub fn count(&self) -> usize {
        self.cards.len()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn test_thread_info() -> ThreadInfo {
        ThreadInfo {
            thread_id: 42,
            agent_id: "code-review".to_string(),
            title: "Review PR #123".to_string(),
            model: Some("claude-sonnet-4-20250514".to_string()),
            capabilities: vec!["text-generation".to_string(), "code-execution".to_string()],
            skills: vec![],
            spawn_mode: "standalone".to_string(),
            parent_thread_id: None,
        }
    }

    #[test]
    fn agent_session_key_format() {
        let key = agent_session_key("code-review", 42);
        assert_eq!(key, "agent:code-review:0000000000000000000000000000002a");
    }

    #[test]
    fn parse_agent_session_key_roundtrip() {
        let key = agent_session_key("my-agent", 99);
        let (agent, thread_hex) = parse_agent_session_key(&key).unwrap();
        assert_eq!(agent, "my-agent");
        assert_eq!(u128::from_str_radix(&thread_hex, 16).unwrap(), 99);
    }

    #[test]
    fn parse_invalid_session_key() {
        assert!(parse_agent_session_key("not-agent-format").is_none());
        assert!(parse_agent_session_key("session:123").is_none());
    }

    #[test]
    fn thread_agent_card_defaults() {
        let info = test_thread_info();
        let card = thread_agent_card(&info, None, "http://localhost:18789");

        assert_eq!(card.id, "thread:code-review");
        assert_eq!(card.name, "Review PR #123");
        assert!(card.capabilities.contains(&CapabilityId::TextGeneration));
        assert!(card.capabilities.contains(&CapabilityId::CodeExecution));
        assert!(card.endpoint.url.contains("/a2a"));
    }

    #[test]
    fn thread_agent_card_with_config_override() {
        let info = test_thread_info();
        let config = ThreadAgentConfig {
            name: Some("Custom Agent Name".to_string()),
            description: Some("Overridden desc".to_string()),
            capabilities: vec!["web-search".to_string()],
            max_concurrent_tasks: Some(20),
            ..Default::default()
        };
        let card = thread_agent_card(&info, Some(&config), "http://localhost:18789");

        assert_eq!(card.name, "Custom Agent Name");
        assert_eq!(card.description, "Overridden desc");
        // Config capabilities override thread capabilities
        assert!(card.capabilities.contains(&CapabilityId::WebSearch));
        assert!(!card.capabilities.contains(&CapabilityId::CodeExecution));
        assert_eq!(card.max_concurrent_tasks, Some(20));
    }

    #[test]
    fn thread_agent_card_no_capabilities_defaults_to_text_gen() {
        let mut info = test_thread_info();
        info.capabilities = vec![];
        let card = thread_agent_card(&info, None, "http://localhost:18789");

        assert_eq!(card.capabilities, vec![CapabilityId::TextGeneration]);
    }

    #[test]
    fn create_spawn_task_wiring() {
        let req = SpawnRequest {
            child_agent_id: "summarizer".to_string(),
            title: "Summarize docs".to_string(),
            task_prompt: "Summarize the README".to_string(),
            spawn_mode: SpawnMode::Run,
            cleanup: CleanupPolicy::Keep,
            model: None,
            capabilities: vec![],
            skills: vec![],
        };

        let result = create_spawn_task("parent-agent", 1, 2, &req);
        assert_eq!(result.child_thread_id, 2);
        assert!(result.child_session_key.starts_with("agent:summarizer:"));
        assert_eq!(result.task.executor_id, "summarizer");
        assert_eq!(result.task.requester_id, "parent-agent");
        assert_eq!(result.task.thread_id, Some(2));
        assert_eq!(result.task.spawn_mode, SpawnMode::Run);
        assert!(result.task.announce_on_complete);
    }

    #[test]
    fn spawn_session_mode_no_announce() {
        let req = SpawnRequest {
            child_agent_id: "assistant".to_string(),
            title: "Persistent helper".to_string(),
            task_prompt: "Help with code".to_string(),
            spawn_mode: SpawnMode::Session,
            cleanup: CleanupPolicy::Keep,
            model: None,
            capabilities: vec![],
            skills: vec![],
        };

        let result = create_spawn_task("parent", 1, 2, &req);
        assert_eq!(result.task.spawn_mode, SpawnMode::Session);
        assert!(!result.task.announce_on_complete);
    }

    #[test]
    fn thread_agent_registry_crud() {
        let registry = ThreadAgentRegistry::new("http://localhost:18789");
        let info = test_thread_info();

        // Register
        registry.upsert(&info, None);
        assert_eq!(registry.count(), 1);

        // Get
        let card = registry.get(42).unwrap();
        assert_eq!(card.id, "thread:code-review");

        // All
        assert_eq!(registry.all_cards().len(), 1);

        // Remove
        registry.remove(42);
        assert_eq!(registry.count(), 0);
        assert!(registry.get(42).is_none());
    }

    #[test]
    fn thread_agent_registry_upsert_overwrites() {
        let registry = ThreadAgentRegistry::new("http://localhost:18789");
        let mut info = test_thread_info();
        registry.upsert(&info, None);

        info.title = "Updated Title".to_string();
        registry.upsert(&info, None);

        let card = registry.get(42).unwrap();
        assert_eq!(card.name, "Updated Title");
        assert_eq!(registry.count(), 1);
    }

    #[test]
    fn sub_agent_thread_metadata() {
        let info = ThreadInfo {
            thread_id: 100,
            agent_id: "child-agent".to_string(),
            title: "Subtask: analyze logs".to_string(),
            model: None,
            capabilities: vec![],
            skills: vec![],
            spawn_mode: "run".to_string(),
            parent_thread_id: Some(42),
        };

        let card = thread_agent_card(&info, None, "http://localhost:18789");
        let meta = &card.metadata;
        assert_eq!(meta["spawn_mode"], "run");
        assert!(meta["parent_thread_id"].as_str().is_some());
    }
}
