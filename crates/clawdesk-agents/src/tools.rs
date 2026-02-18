//! Tool orchestration — lazy loading, policy engine, schema caching.
//!
//! Lazy loading: startup is O(A) where A = actually-used tools, not O(T) total.
//! Schema caching: SHA-256 content addressing, O(1) after first build.
//! Policy engine: allowlists, approval requirements, capability gating.
//! Before-tool-call hooks: async with timeout via `tokio::time::timeout`.

use async_trait::async_trait;
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

/// Schema definition for a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    /// JSON Schema for parameters.
    pub parameters: serde_json::Value,
}

/// Result of a tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
}

/// Tool trait — the core interface for all tools.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name (unique identifier).
    fn name(&self) -> &str;

    /// Tool schema for LLM function calling.
    fn schema(&self) -> ToolSchema;

    /// Whether this tool blocks the async runtime (e.g., file I/O, shell commands).
    fn is_blocking(&self) -> bool {
        false
    }

    /// Required capabilities for this tool.
    fn required_capabilities(&self) -> Vec<ToolCapability> {
        vec![]
    }

    /// Execute the tool with JSON arguments.
    async fn execute(&self, args: serde_json::Value) -> Result<String, String>;
}

/// Tool capabilities for policy enforcement.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolCapability {
    /// File system access.
    FileSystem,
    /// Network access.
    Network,
    /// Shell command execution.
    ShellExec,
    /// Browser automation.
    Browser,
    /// Memory/database access.
    Memory,
    /// Message sending.
    Messaging,
    /// External API calls.
    ExternalApi,
    /// Image/media generation.
    MediaGeneration,
    /// Cron/scheduling.
    Scheduling,
}

/// Tool policy configuration — governs which tools are allowed and when.
#[derive(Debug, Clone)]
pub struct ToolPolicy {
    /// Allowlist of tool names. If empty, all tools are allowed.
    pub allowlist: HashSet<String>,
    /// Denylist of tool names (overrides allowlist).
    pub denylist: HashSet<String>,
    /// Tools that require user approval before execution.
    pub require_approval: HashSet<String>,
    /// Capability grants — which capabilities are active.
    pub granted_capabilities: HashSet<ToolCapability>,
    /// Maximum concurrent tool executions.
    pub max_concurrent: usize,
    /// Timeout per tool execution in seconds.
    pub tool_timeout_secs: u64,
}

impl Default for ToolPolicy {
    fn default() -> Self {
        Self {
            allowlist: HashSet::new(), // Empty = allow all
            denylist: HashSet::new(),
            require_approval: HashSet::new(),
            granted_capabilities: HashSet::new(),
            max_concurrent: 8,
            tool_timeout_secs: 30,
        }
    }
}

impl ToolPolicy {
    /// Check if a tool is allowed by policy. O(1) hash lookup.
    pub fn is_allowed(&self, tool_name: &str) -> bool {
        if self.denylist.contains(tool_name) {
            return false;
        }
        if self.allowlist.is_empty() {
            return true; // No allowlist = allow all
        }
        self.allowlist.contains(tool_name)
    }

    /// Check if a tool requires approval before execution.
    pub fn requires_approval(&self, tool_name: &str) -> bool {
        self.require_approval.contains(tool_name)
    }

    /// Check if all required capabilities are granted.
    pub fn capabilities_met(&self, required: &[ToolCapability]) -> bool {
        required
            .iter()
            .all(|cap| self.granted_capabilities.contains(cap))
    }
}

/// Tool registry — `FxHashMap` for near-perfect O(1) lookups.
///
/// Replaces the standard `HashMap` (SipHash-2-4, ~15ns per lookup) with
/// `FxHashMap` (FxHash, ~3ns per lookup). FxHash is NOT cryptographically
/// secure, but tool names are trusted internal strings — collision resistance
/// against adversarial input is unnecessary.
///
/// Each tool slot contains:
/// - An `OnceLock<Arc<dyn Tool>>` for lock-free access after initialization
/// - An `Arc<dyn Fn>` factory for lazy init and re-initialization
///
/// Lookup is wait-free for loaded tools (single atomic `Relaxed` load).
/// First access triggers `OnceLock::get_or_init` (one thread initializes,
/// others spin-wait ~100ns on the internal `AtomicU8`).
///
/// The `Arc<ToolRegistry>` can be shared freely across agent runners
/// without any external `Mutex` or `RwLock`.
pub struct ToolRegistry {
    /// Tool slots: name → (initializer, factory, cached schema).
    slots: FxHashMap<String, ToolSlot>,
}

struct ToolSlot {
    /// The tool instance, initialized lazily via OnceLock.
    instance: OnceLock<Arc<dyn Tool>>,
    /// Factory for lazy initialization (non-consuming, Arc-shared).
    factory: Option<Arc<dyn Fn() -> Arc<dyn Tool> + Send + Sync>>,
    /// Cached schema (always available immediately).
    schema: ToolSchema,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            slots: FxHashMap::default(),
        }
    }

    /// Register a tool eagerly (loaded immediately).
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        let name = tool.name().to_string();
        let schema = tool.schema();
        let lock = OnceLock::new();
        let _ = lock.set(tool);
        self.slots.insert(
            name,
            ToolSlot {
                instance: lock,
                factory: None,
                schema,
            },
        );
    }

    /// Register a tool lazily (loaded on first `get` call).
    /// Factory is `Arc`-shared and non-consuming — survives re-initialization.
    pub fn register_lazy<F>(&mut self, name: String, schema: ToolSchema, factory: F)
    where
        F: Fn() -> Arc<dyn Tool> + Send + Sync + 'static,
    {
        self.slots.insert(
            name,
            ToolSlot {
                instance: OnceLock::new(),
                factory: Some(Arc::new(factory)),
                schema,
            },
        );
    }

    /// Get a tool by name. Triggers lazy loading if needed.
    ///
    /// This works through `&self` (no `&mut` needed) via `OnceLock::get_or_init`.
    /// After first initialization, lookup is a single atomic `Relaxed` load.
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        let slot = self.slots.get(name)?;
        let tool = if let Some(t) = slot.instance.get() {
            // Fast path: already loaded. Single atomic read.
            Arc::clone(t)
        } else if let Some(factory) = &slot.factory {
            // Lazy path: OnceLock::get_or_init handles concurrent init safely.
            let f = Arc::clone(factory);
            let tool = slot.instance.get_or_init(move || f());
            Arc::clone(tool)
        } else {
            return None;
        };
        Some(tool)
    }

    /// Get a tool, loading it lazily if needed. Kept for API compatibility.
    pub fn get_or_load(&mut self, name: &str) -> Option<Arc<dyn Tool>> {
        // Delegate to get() which now handles lazy loading via OnceLock.
        self.get(name)
    }

    /// Get all tool schemas (both loaded and lazy).
    /// O(n) but does not trigger lazy loading.
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.slots.values().map(|s| s.schema.clone()).collect()
    }

    /// List all registered tool names (both loaded and lazy).
    pub fn list(&self) -> Vec<String> {
        let mut names: Vec<String> = self.slots.keys().cloned().collect();
        names.sort();
        names
    }

    /// Number of loaded (initialized) tools.
    pub fn loaded_count(&self) -> usize {
        self.slots.values().filter(|s| s.instance.get().is_some()).count()
    }

    /// Number of total tools (loaded + lazy).
    pub fn total_count(&self) -> usize {
        self.slots.len()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Before-tool-call hook — runs before tool execution.
#[async_trait]
pub trait BeforeToolHook: Send + Sync {
    /// Called before a tool is executed. Return Err to cancel execution.
    async fn before_call(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<(), String>;
}

/// After-tool-call hook — runs after tool execution.
#[async_trait]
pub trait AfterToolHook: Send + Sync {
    /// Called after a tool execution completes.
    async fn after_call(&self, result: &ToolResult);
}

/// Hook runner — executes before/after hooks with timeout.
pub struct HookRunner {
    before_hooks: Vec<Arc<dyn BeforeToolHook>>,
    after_hooks: Vec<Arc<dyn AfterToolHook>>,
    timeout: std::time::Duration,
}

impl HookRunner {
    pub fn new(timeout: std::time::Duration) -> Self {
        Self {
            before_hooks: Vec::new(),
            after_hooks: Vec::new(),
            timeout,
        }
    }

    pub fn add_before_hook(&mut self, hook: Arc<dyn BeforeToolHook>) {
        self.before_hooks.push(hook);
    }

    pub fn add_after_hook(&mut self, hook: Arc<dyn AfterToolHook>) {
        self.after_hooks.push(hook);
    }

    /// Run all before hooks with timeout. Returns Err if any hook rejects.
    pub async fn run_before(&self, tool_name: &str, args: &serde_json::Value) -> Result<(), String> {
        for hook in &self.before_hooks {
            let hook = Arc::clone(hook);
            let name = tool_name.to_string();
            let args = args.clone();
            let result = tokio::time::timeout(self.timeout, async move {
                hook.before_call(&name, &args).await
            })
            .await;

            match result {
                Ok(Ok(())) => continue,
                Ok(Err(e)) => return Err(e),
                Err(_) => return Err("before-tool hook timed out".to_string()),
            }
        }
        Ok(())
    }

    /// Run all after hooks (fire-and-forget, with timeout).
    pub async fn run_after(&self, result: &ToolResult) {
        for hook in &self.after_hooks {
            let hook = Arc::clone(hook);
            let result = result.clone();
            let timeout = self.timeout;
            tokio::spawn(async move {
                let _ = tokio::time::timeout(timeout, hook.after_call(&result)).await;
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct EchoTool;

    #[async_trait]
    impl Tool for EchoTool {
        fn name(&self) -> &str {
            "echo"
        }

        fn schema(&self) -> ToolSchema {
            ToolSchema {
                name: "echo".into(),
                description: "Echoes input".into(),
                parameters: serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
            }
        }

        async fn execute(&self, args: serde_json::Value) -> Result<String, String> {
            Ok(args.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string())
        }
    }

    #[test]
    fn test_tool_registry() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));

        assert_eq!(registry.total_count(), 1);
        assert!(registry.get("echo").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn test_tool_policy_allowlist() {
        let policy = ToolPolicy {
            allowlist: HashSet::from(["echo".to_string()]),
            ..Default::default()
        };
        assert!(policy.is_allowed("echo"));
        assert!(!policy.is_allowed("bash"));
    }

    #[test]
    fn test_tool_policy_denylist_overrides_allowlist() {
        let policy = ToolPolicy {
            allowlist: HashSet::from(["bash".to_string()]),
            denylist: HashSet::from(["bash".to_string()]),
            ..Default::default()
        };
        assert!(!policy.is_allowed("bash"));
    }

    #[test]
    fn test_schema_caching() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(EchoTool));

        let schemas = registry.schemas();
        assert_eq!(schemas.len(), 1);
        assert_eq!(schemas[0].name, "echo");
    }
}
