//! GAP-F: Multi-agent shared state — scoped blackboard for collaboration.
//!
//! Provides a hierarchical key-value store scoped to a pipeline execution,
//! delegation chain, or conversation. Agents within the same scope can
//! read/write structured state, enabling coordination beyond text-passing.
//!
//! ## Scoping model
//!
//! ```text
//! pipeline:abc/           ← pipeline-scoped (auto-cleanup on completion)
//!   researcher/notes      ← agent-namespaced keys
//!   writer/outline
//! delegation:xyz/         ← delegation chain-scoped
//!   parent/goal
//!   child/progress
//! conversation:def/       ← conversation-scoped (persists across turns)
//!   shared_context
//! ```
//!
//! ## Concurrency
//!
//! The backend uses `tokio::sync::RwLock` for safe concurrent access.
//! CAS (compare-and-swap) is provided for optimistic concurrency control.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// Scoped shared state store for multi-agent collaboration.
///
/// A hierarchical KV store scoped to a pipeline execution, delegation chain,
/// or conversation. Supports typed get/set with JSON values, atomic CAS,
/// and namespace prefixing for isolation between concurrent pipelines.
#[derive(Clone)]
pub struct SharedAgentState {
    /// Scope identifier (pipeline_id, delegation_chain_id, or conversation_id).
    scope_id: String,
    /// Underlying storage backend.
    backend: Arc<dyn SharedStateBackend>,
    /// Namespace prefix for this agent within the scope.
    namespace: String,
    /// Optional TTL for auto-cleanup.
    ttl: Option<Duration>,
    /// Creation time for TTL calculation.
    created_at: DateTime<Utc>,
}

impl SharedAgentState {
    /// Create a new shared state scoped to `scope_id`.
    ///
    /// The `namespace` (typically agent_id) prefixes all keys to prevent
    /// collisions within the same scope.
    pub fn new(
        scope_id: impl Into<String>,
        namespace: impl Into<String>,
        backend: Arc<dyn SharedStateBackend>,
    ) -> Self {
        Self {
            scope_id: scope_id.into(),
            backend,
            namespace: namespace.into(),
            ttl: None,
            created_at: Utc::now(),
        }
    }

    /// Create with a TTL — entries expire after the given duration.
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = Some(ttl);
        self
    }

    /// Create a child view with a different namespace (for sub-agent delegation).
    ///
    /// The child shares the same scope and backend but writes under its own
    /// namespace. It can also read the parent's keys via `get_namespaced()`.
    pub fn child(&self, child_namespace: impl Into<String>) -> Self {
        Self {
            scope_id: self.scope_id.clone(),
            backend: self.backend.clone(),
            namespace: child_namespace.into(),
            ttl: self.ttl,
            created_at: self.created_at,
        }
    }

    /// Scope identifier.
    pub fn scope_id(&self) -> &str {
        &self.scope_id
    }

    /// Current namespace.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Set a value under this agent's namespace.
    pub async fn set(&self, key: &str, value: Value) -> Result<(), SharedStateError> {
        let full_key = self.full_key(key);
        let entry = StateEntry {
            value,
            written_by: self.namespace.clone(),
            written_at: Utc::now(),
            version: 0, // Backend manages versioning
        };
        self.backend.set(&self.scope_id, &full_key, entry).await
    }

    /// Get a value from this agent's namespace.
    pub async fn get(&self, key: &str) -> Result<Option<Value>, SharedStateError> {
        let full_key = self.full_key(key);
        self.backend
            .get(&self.scope_id, &full_key)
            .await
            .map(|opt| opt.map(|e| e.value))
    }

    /// Get a value from another agent's namespace within the same scope.
    pub async fn get_from(&self, namespace: &str, key: &str) -> Result<Option<Value>, SharedStateError> {
        let full_key = format!("{}/{}", namespace, key);
        self.backend
            .get(&self.scope_id, &full_key)
            .await
            .map(|opt| opt.map(|e| e.value))
    }

    /// Get the full state entry (with metadata) from this agent's namespace.
    pub async fn get_entry(&self, key: &str) -> Result<Option<StateEntry>, SharedStateError> {
        let full_key = self.full_key(key);
        self.backend.get(&self.scope_id, &full_key).await
    }

    /// Compare-and-swap: only set if the current value matches `expected`.
    ///
    /// Returns `true` if the swap succeeded, `false` if the current value
    /// didn't match (someone else wrote first).
    pub async fn cas(
        &self,
        key: &str,
        expected: Option<&Value>,
        new_value: Value,
    ) -> Result<bool, SharedStateError> {
        let full_key = self.full_key(key);
        let entry = StateEntry {
            value: new_value,
            written_by: self.namespace.clone(),
            written_at: Utc::now(),
            version: 0,
        };
        self.backend
            .cas(&self.scope_id, &full_key, expected, entry)
            .await
    }

    /// Delete a key from this agent's namespace.
    pub async fn delete(&self, key: &str) -> Result<bool, SharedStateError> {
        let full_key = self.full_key(key);
        self.backend.delete(&self.scope_id, &full_key).await
    }

    /// List all keys in this agent's namespace.
    pub async fn list_keys(&self) -> Result<Vec<String>, SharedStateError> {
        let prefix = format!("{}/", self.namespace);
        let keys = self.backend.list_keys(&self.scope_id, &prefix).await?;
        // Strip the namespace prefix
        Ok(keys
            .into_iter()
            .map(|k| k.strip_prefix(&prefix).unwrap_or(&k).to_string())
            .collect())
    }

    /// List all keys across all namespaces in this scope (global view).
    pub async fn list_all_keys(&self) -> Result<Vec<String>, SharedStateError> {
        self.backend.list_keys(&self.scope_id, "").await
    }

    /// Get a snapshot of all state in this scope — useful for context injection.
    pub async fn snapshot(&self) -> Result<HashMap<String, Value>, SharedStateError> {
        self.backend.snapshot(&self.scope_id).await
    }

    /// Delete the entire scope (cleanup after pipeline completion).
    pub async fn cleanup(&self) -> Result<(), SharedStateError> {
        self.backend.delete_scope(&self.scope_id).await
    }

    /// Format scope state for LLM context injection.
    ///
    /// Returns an XML-formatted summary of key-value pairs, bounded by
    /// `max_tokens` to prevent shared state from consuming the entire
    /// context window. Entries are included in insertion order until the
    /// budget is exhausted. Each value is per-entry truncated at 500 chars.
    pub async fn format_for_context_bounded(
        &self,
        max_tokens: usize,
    ) -> Result<String, SharedStateError> {
        let snap = self.snapshot().await?;
        if snap.is_empty() {
            return Ok(String::new());
        }

        let header = "<shared_state>\n";
        let footer = "</shared_state>\n";
        // Reserve tokens for header + footer (~10 tokens)
        let budget = max_tokens.saturating_sub(10);
        let mut used_tokens: usize = 0;

        let mut out = String::from(header);
        for (key, value) in &snap {
            let val_str = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            // Per-entry truncation at 500 chars (char-safe, not byte-based)
            let truncated = if val_str.chars().count() > 500 {
                let s: String = val_str.chars().take(500).collect();
                format!("{}... (truncated)", s)
            } else {
                val_str
            };
            let entry = format!("  <entry key=\"{}\">{}</entry>\n", key, truncated);
            let entry_tokens = clawdesk_types::tokenizer::estimate_tokens(&entry);
            if used_tokens + entry_tokens > budget {
                // Budget exhausted — add summary of omitted entries
                let remaining = snap.len().saturating_sub(out.matches("<entry").count());
                if remaining > 0 {
                    out.push_str(&format!(
                        "  <!-- {} more entries omitted (token budget) -->\n",
                        remaining
                    ));
                }
                break;
            }
            used_tokens += entry_tokens;
            out.push_str(&entry);
        }
        out.push_str(footer);
        Ok(out)
    }

    /// Format scope state for LLM context injection.
    ///
    /// Returns an XML-formatted summary of all key-value pairs in the scope.
    /// **Unbounded** — prefer `format_for_context_bounded()` for production use.
    pub async fn format_for_context(&self) -> Result<String, SharedStateError> {
        let snap = self.snapshot().await?;
        if snap.is_empty() {
            return Ok(String::new());
        }

        let mut out = String::from("<shared_state>\n");
        for (key, value) in &snap {
            let val_str = match value {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            // Per-entry truncation at 500 chars
            let truncated = if val_str.chars().count() > 500 {
                let s: String = val_str.chars().take(500).collect();
                format!("{}... (truncated)", s)
            } else {
                val_str
            };
            out.push_str(&format!("  <entry key=\"{}\">{}</entry>\n", key, truncated));
        }
        out.push_str("</shared_state>\n");
        Ok(out)
    }

    /// Whether this state has expired past TTL.
    pub fn is_expired(&self) -> bool {
        match self.ttl {
            Some(ttl) => {
                let deadline = self.created_at + chrono::Duration::from_std(ttl).unwrap_or_default();
                Utc::now() > deadline
            }
            None => false,
        }
    }

    fn full_key(&self, key: &str) -> String {
        format!("{}/{}", self.namespace, key)
    }
}

/// A single entry in the shared state store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateEntry {
    pub value: Value,
    pub written_by: String,
    pub written_at: DateTime<Utc>,
    pub version: u64,
}

/// Errors from shared state operations.
#[derive(Debug, thiserror::Error)]
pub enum SharedStateError {
    #[error("scope not found: {0}")]
    ScopeNotFound(String),
    #[error("key not found: {0}")]
    KeyNotFound(String),
    #[error("serialization error: {0}")]
    Serde(String),
    #[error("backend error: {0}")]
    Backend(String),
    #[error("scope expired")]
    Expired,
}

/// Backend trait for shared state storage.
///
/// Implementations can be in-memory (for tests/ephemeral pipelines) or
/// backed by SochDB (for durable cross-restart state).
#[async_trait]
pub trait SharedStateBackend: Send + Sync + 'static {
    /// Get a state entry by scope and key.
    async fn get(&self, scope: &str, key: &str) -> Result<Option<StateEntry>, SharedStateError>;

    /// Set a state entry.
    async fn set(&self, scope: &str, key: &str, entry: StateEntry) -> Result<(), SharedStateError>;

    /// Compare-and-swap: set only if current value matches `expected`.
    async fn cas(
        &self,
        scope: &str,
        key: &str,
        expected: Option<&Value>,
        new_entry: StateEntry,
    ) -> Result<bool, SharedStateError>;

    /// Delete a key from a scope.
    async fn delete(&self, scope: &str, key: &str) -> Result<bool, SharedStateError>;

    /// List keys in a scope matching a prefix.
    async fn list_keys(&self, scope: &str, prefix: &str) -> Result<Vec<String>, SharedStateError>;

    /// Get all key-value pairs in a scope.
    async fn snapshot(&self, scope: &str) -> Result<HashMap<String, Value>, SharedStateError>;

    /// Delete an entire scope.
    async fn delete_scope(&self, scope: &str) -> Result<(), SharedStateError>;
}

// ---------------------------------------------------------------------------
// In-memory backend (for tests and ephemeral pipelines)
// ---------------------------------------------------------------------------

/// In-memory shared state backend.
///
/// Keys are stored as `{scope}::{key}` in a flat HashMap.
/// Thread-safe via `tokio::sync::RwLock`.
pub struct InMemorySharedState {
    store: RwLock<HashMap<String, StateEntry>>,
}

impl InMemorySharedState {
    pub fn new() -> Self {
        Self {
            store: RwLock::new(HashMap::new()),
        }
    }

    fn scoped_key(scope: &str, key: &str) -> String {
        format!("{}::{}", scope, key)
    }
}

impl Default for InMemorySharedState {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SharedStateBackend for InMemorySharedState {
    async fn get(&self, scope: &str, key: &str) -> Result<Option<StateEntry>, SharedStateError> {
        let store = self.store.read().await;
        Ok(store.get(&Self::scoped_key(scope, key)).cloned())
    }

    async fn set(&self, scope: &str, key: &str, mut entry: StateEntry) -> Result<(), SharedStateError> {
        let mut store = self.store.write().await;
        let sk = Self::scoped_key(scope, key);
        // Increment version
        if let Some(existing) = store.get(&sk) {
            entry.version = existing.version + 1;
        }
        store.insert(sk, entry);
        Ok(())
    }

    async fn cas(
        &self,
        scope: &str,
        key: &str,
        expected: Option<&Value>,
        mut new_entry: StateEntry,
    ) -> Result<bool, SharedStateError> {
        let mut store = self.store.write().await;
        let sk = Self::scoped_key(scope, key);
        let current = store.get(&sk);

        match (expected, current) {
            (None, None) => {
                // Key doesn't exist, expected None
                store.insert(sk, new_entry);
                Ok(true)
            }
            (Some(exp), Some(cur)) if &cur.value == exp => {
                new_entry.version = cur.version + 1;
                store.insert(sk, new_entry);
                Ok(true)
            }
            _ => Ok(false), // CAS failed
        }
    }

    async fn delete(&self, scope: &str, key: &str) -> Result<bool, SharedStateError> {
        let mut store = self.store.write().await;
        Ok(store.remove(&Self::scoped_key(scope, key)).is_some())
    }

    async fn list_keys(&self, scope: &str, prefix: &str) -> Result<Vec<String>, SharedStateError> {
        let store = self.store.read().await;
        let scope_prefix = format!("{}::", scope);
        let full_prefix = format!("{}{}", scope_prefix, prefix);
        Ok(store
            .keys()
            .filter(|k| k.starts_with(&full_prefix))
            .map(|k| k.strip_prefix(&scope_prefix).unwrap_or(k).to_string())
            .collect())
    }

    async fn snapshot(&self, scope: &str) -> Result<HashMap<String, Value>, SharedStateError> {
        let store = self.store.read().await;
        let scope_prefix = format!("{}::", scope);
        Ok(store
            .iter()
            .filter(|(k, _)| k.starts_with(&scope_prefix))
            .map(|(k, v)| {
                let short_key = k.strip_prefix(&scope_prefix).unwrap_or(k).to_string();
                (short_key, v.value.clone())
            })
            .collect())
    }

    async fn delete_scope(&self, scope: &str) -> Result<(), SharedStateError> {
        let mut store = self.store.write().await;
        let scope_prefix = format!("{}::", scope);
        store.retain(|k, _| !k.starts_with(&scope_prefix));
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Convenience: scope factory
// ---------------------------------------------------------------------------

/// Factory for creating scoped shared state instances.
///
/// Manages lifecycle: creates scopes on demand, tracks active scopes,
/// and provides cleanup for expired scopes.
pub struct SharedStateManager {
    backend: Arc<dyn SharedStateBackend>,
    active_scopes: RwLock<HashMap<String, DateTime<Utc>>>,
}

impl SharedStateManager {
    pub fn new(backend: Arc<dyn SharedStateBackend>) -> Self {
        Self {
            backend,
            active_scopes: RwLock::new(HashMap::new()),
        }
    }

    /// Create a new scoped state for a pipeline.
    pub async fn pipeline_scope(&self, pipeline_id: &str, agent_id: &str) -> SharedAgentState {
        let scope = format!("pipeline:{}", pipeline_id);
        self.active_scopes
            .write()
            .await
            .insert(scope.clone(), Utc::now());
        SharedAgentState::new(scope, agent_id, self.backend.clone())
            .with_ttl(Duration::from_secs(3600)) // 1 hour default TTL
    }

    /// Create a new scoped state for a delegation chain.
    pub async fn delegation_scope(&self, chain_id: &str, agent_id: &str) -> SharedAgentState {
        let scope = format!("delegation:{}", chain_id);
        self.active_scopes
            .write()
            .await
            .insert(scope.clone(), Utc::now());
        SharedAgentState::new(scope, agent_id, self.backend.clone())
            .with_ttl(Duration::from_secs(1800)) // 30 min default TTL
    }

    /// Create a new scoped state for a conversation (persistent, no TTL).
    pub async fn conversation_scope(&self, conversation_id: &str, agent_id: &str) -> SharedAgentState {
        let scope = format!("conversation:{}", conversation_id);
        self.active_scopes
            .write()
            .await
            .insert(scope.clone(), Utc::now());
        SharedAgentState::new(scope, agent_id, self.backend.clone())
    }

    /// Cleanup a specific scope.
    pub async fn cleanup_scope(&self, scope_id: &str) -> Result<(), SharedStateError> {
        self.backend.delete_scope(scope_id).await?;
        self.active_scopes.write().await.remove(scope_id);
        Ok(())
    }

    /// List active scope IDs.
    pub async fn active_scopes(&self) -> Vec<String> {
        self.active_scopes
            .read()
            .await
            .keys()
            .cloned()
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_state(scope: &str, ns: &str) -> SharedAgentState {
        let backend = Arc::new(InMemorySharedState::new());
        SharedAgentState::new(scope, ns, backend)
    }

    fn make_shared_backend() -> Arc<InMemorySharedState> {
        Arc::new(InMemorySharedState::new())
    }

    #[tokio::test]
    async fn test_basic_set_get() {
        let state = make_state("pipeline:1", "agent_a");
        state.set("notes", json!("hello")).await.unwrap();
        let val = state.get("notes").await.unwrap().unwrap();
        assert_eq!(val, json!("hello"));
    }

    #[tokio::test]
    async fn test_get_missing_key() {
        let state = make_state("pipeline:1", "agent_a");
        let val = state.get("nonexistent").await.unwrap();
        assert!(val.is_none());
    }

    #[tokio::test]
    async fn test_cross_namespace_read() {
        let backend = make_shared_backend();
        let agent_a = SharedAgentState::new("pipeline:1", "agent_a", backend.clone());
        let agent_b = SharedAgentState::new("pipeline:1", "agent_b", backend);

        agent_a.set("notes", json!({"key": "value"})).await.unwrap();

        // Agent B can read Agent A's key
        let val = agent_b.get_from("agent_a", "notes").await.unwrap().unwrap();
        assert_eq!(val, json!({"key": "value"}));
    }

    #[tokio::test]
    async fn test_scope_isolation() {
        let backend = make_shared_backend();
        let scope_1 = SharedAgentState::new("pipeline:1", "agent", backend.clone());
        let scope_2 = SharedAgentState::new("pipeline:2", "agent", backend);

        scope_1.set("key", json!("scope1")).await.unwrap();
        scope_2.set("key", json!("scope2")).await.unwrap();

        assert_eq!(scope_1.get("key").await.unwrap().unwrap(), json!("scope1"));
        assert_eq!(scope_2.get("key").await.unwrap().unwrap(), json!("scope2"));
    }

    #[tokio::test]
    async fn test_cas_success() {
        let state = make_state("scope", "agent");
        state.set("counter", json!(1)).await.unwrap();

        let ok = state.cas("counter", Some(&json!(1)), json!(2)).await.unwrap();
        assert!(ok);
        assert_eq!(state.get("counter").await.unwrap().unwrap(), json!(2));
    }

    #[tokio::test]
    async fn test_cas_failure() {
        let state = make_state("scope", "agent");
        state.set("counter", json!(1)).await.unwrap();

        let ok = state.cas("counter", Some(&json!(99)), json!(2)).await.unwrap();
        assert!(!ok);
        // Value unchanged
        assert_eq!(state.get("counter").await.unwrap().unwrap(), json!(1));
    }

    #[tokio::test]
    async fn test_cas_create_new() {
        let state = make_state("scope", "agent");

        let ok = state.cas("new_key", None, json!("created")).await.unwrap();
        assert!(ok);
        assert_eq!(
            state.get("new_key").await.unwrap().unwrap(),
            json!("created")
        );
    }

    #[tokio::test]
    async fn test_delete() {
        let state = make_state("scope", "agent");
        state.set("key", json!("val")).await.unwrap();
        assert!(state.delete("key").await.unwrap());
        assert!(state.get("key").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_list_keys() {
        let state = make_state("scope", "agent");
        state.set("alpha", json!(1)).await.unwrap();
        state.set("beta", json!(2)).await.unwrap();
        state.set("gamma", json!(3)).await.unwrap();

        let mut keys = state.list_keys().await.unwrap();
        keys.sort();
        assert_eq!(keys, vec!["alpha", "beta", "gamma"]);
    }

    #[tokio::test]
    async fn test_snapshot() {
        let backend = make_shared_backend();
        let agent_a = SharedAgentState::new("scope", "a", backend.clone());
        let agent_b = SharedAgentState::new("scope", "b", backend);

        agent_a.set("x", json!(1)).await.unwrap();
        agent_b.set("y", json!(2)).await.unwrap();

        let snap = agent_a.snapshot().await.unwrap();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap.get("a/x").unwrap(), &json!(1));
        assert_eq!(snap.get("b/y").unwrap(), &json!(2));
    }

    #[tokio::test]
    async fn test_cleanup_scope() {
        let state = make_state("ephemeral", "agent");
        state.set("temp", json!("data")).await.unwrap();
        state.cleanup().await.unwrap();
        assert!(state.get("temp").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_child_view() {
        let backend = make_shared_backend();
        let parent = SharedAgentState::new("pipeline:1", "parent", backend);

        parent.set("goal", json!("analyze data")).await.unwrap();

        let child = parent.child("child_agent");
        child.set("progress", json!("50%")).await.unwrap();

        // Child can read parent's state
        let goal = child.get_from("parent", "goal").await.unwrap().unwrap();
        assert_eq!(goal, json!("analyze data"));

        // Parent can read child's state
        let progress = parent
            .get_from("child_agent", "progress")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(progress, json!("50%"));
    }

    #[tokio::test]
    async fn test_format_for_context() {
        let state = make_state("scope", "agent");
        state.set("notes", json!("important finding")).await.unwrap();
        let ctx = state.format_for_context().await.unwrap();
        assert!(ctx.contains("<shared_state>"));
        assert!(ctx.contains("important finding"));
    }

    #[tokio::test]
    async fn test_format_for_context_empty() {
        let state = make_state("scope", "agent");
        let ctx = state.format_for_context().await.unwrap();
        assert!(ctx.is_empty());
    }

    #[tokio::test]
    async fn test_shared_state_manager() {
        let backend = make_shared_backend();
        let mgr = SharedStateManager::new(backend);

        let state = mgr.pipeline_scope("pipe-1", "agent_a").await;
        state.set("result", json!(42)).await.unwrap();

        let scopes = mgr.active_scopes().await;
        assert_eq!(scopes, vec!["pipeline:pipe-1"]);

        mgr.cleanup_scope("pipeline:pipe-1").await.unwrap();
        assert!(state.get("result").await.unwrap().is_none());
        assert!(mgr.active_scopes().await.is_empty());
    }

    #[tokio::test]
    async fn test_versioning() {
        let state = make_state("scope", "agent");
        state.set("key", json!(1)).await.unwrap();
        let entry1 = state.get_entry("key").await.unwrap().unwrap();
        assert_eq!(entry1.version, 0);

        state.set("key", json!(2)).await.unwrap();
        let entry2 = state.get_entry("key").await.unwrap().unwrap();
        assert_eq!(entry2.version, 1);
    }

    #[tokio::test]
    async fn test_expired() {
        let state = make_state("scope", "agent")
            .with_ttl(Duration::from_secs(0));
        assert!(state.is_expired());
    }
}
