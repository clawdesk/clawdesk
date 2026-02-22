//! Plugin lifecycle hook system with phase-based execution.
//!
//! Hooks allow plugins to intercept and modify behavior at critical lifecycle phases.
//! Uses priority-ordered dispatch with async execution and chain-of-responsibility pattern.
//!
//! ## Phases
//! - `BeforeAgentStart` — Before agent loop begins
//! - `AfterToolCall` — After a tool/skill completes
//! - `BeforeCompaction` — Before context compaction
//! - `AfterCompaction` — After context compaction  
//! - `BeforeLlmCall` — Before sending prompt to LLM
//! - `AfterLlmCall` — After receiving LLM response
//! - `MessageReceive` — When an inbound message arrives
//! - `MessageSend` — Before sending an outbound message
//! - `SessionStart` — When a new session begins
//! - `SessionEnd` — When a session terminates
//! - `Boot` — System bootstrap
//! - `Shutdown` — System shutdown

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// Lifecycle phase at which a hook fires.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Phase {
    Boot,
    SessionStart,
    BeforeAgentStart,
    MessageReceive,
    BeforeLlmCall,
    AfterLlmCall,
    AfterToolCall,
    BeforeCompaction,
    AfterCompaction,
    MessageSend,
    SessionEnd,
    Shutdown,
}

impl fmt::Display for Phase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Phase::Boot => write!(f, "boot"),
            Phase::SessionStart => write!(f, "session_start"),
            Phase::BeforeAgentStart => write!(f, "before_agent_start"),
            Phase::MessageReceive => write!(f, "message_receive"),
            Phase::BeforeLlmCall => write!(f, "before_llm_call"),
            Phase::AfterLlmCall => write!(f, "after_llm_call"),
            Phase::AfterToolCall => write!(f, "after_tool_call"),
            Phase::BeforeCompaction => write!(f, "before_compaction"),
            Phase::AfterCompaction => write!(f, "after_compaction"),
            Phase::MessageSend => write!(f, "message_send"),
            Phase::SessionEnd => write!(f, "session_end"),
            Phase::Shutdown => write!(f, "shutdown"),
        }
    }
}

/// Priority for hook execution (lower = runs first).
pub type Priority = i32;

/// Typed overrides that hooks can set to mutate agent behavior.
///
/// Instead of stuffing mutations into the untyped `data: serde_json::Value`
/// field, hooks use these typed fields so the runner can apply them safely.
/// All fields are `Option` — only populated fields cause an override.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HookOverrides {
    /// Override the model for this run (e.g., switch to a cheaper model
    /// for simple queries, or a more capable model for complex ones).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Text to prepend to the system prompt.
    /// Applied before the existing system prompt content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_prepend: Option<String>,

    /// Text to append to the system prompt.
    /// Applied after the existing system prompt content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt_append: Option<String>,

    /// Additional tool names to inject for this run.
    /// These tools must be registered in the ToolRegistry — the hook only
    /// activates them, it does not define new tools.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inject_tools: Vec<String>,

    /// Tool names to suppress for this run (remove from available tools).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suppress_tools: Vec<String>,

    /// Override the maximum tool rounds for this run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tool_rounds: Option<usize>,

    /// Text to prepend to the response before delivery.
    /// Applied at the MessageSend phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_prepend: Option<String>,

    /// Text to append to the response before delivery.
    /// Applied at the MessageSend phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_append: Option<String>,
}

impl HookOverrides {
    /// Returns `true` if no overrides are set.
    pub fn is_empty(&self) -> bool {
        self.model.is_none()
            && self.system_prompt_prepend.is_none()
            && self.system_prompt_append.is_none()
            && self.inject_tools.is_empty()
            && self.suppress_tools.is_empty()
            && self.max_tool_rounds.is_none()
            && self.response_prepend.is_none()
            && self.response_append.is_none()
    }

    /// Merge another set of overrides into this one.
    /// Later overrides win for scalar fields; lists are concatenated.
    pub fn merge(&mut self, other: &HookOverrides) {
        if other.model.is_some() {
            self.model = other.model.clone();
        }
        if other.system_prompt_prepend.is_some() {
            self.system_prompt_prepend = other.system_prompt_prepend.clone();
        }
        if other.system_prompt_append.is_some() {
            self.system_prompt_append = other.system_prompt_append.clone();
        }
        self.inject_tools.extend(other.inject_tools.iter().cloned());
        self.suppress_tools.extend(other.suppress_tools.iter().cloned());
        if other.max_tool_rounds.is_some() {
            self.max_tool_rounds = other.max_tool_rounds;
        }
        if other.response_prepend.is_some() {
            self.response_prepend = other.response_prepend.clone();
        }
        if other.response_append.is_some() {
            self.response_append = other.response_append.clone();
        }
    }
}

/// Context passed to hooks — mutable data the hook can inspect/modify.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookContext {
    pub phase: Phase,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub data: serde_json::Value,
    /// If set to true by a hook, the chain is short-circuited.
    pub cancelled: bool,
    /// GAP-7: Typed overrides that hooks can set to mutate agent behavior.
    /// The runner inspects this after dispatch and applies any set fields.
    /// Multiple hooks in a chain can contribute overrides — later hooks win
    /// for scalar fields, lists are concatenated.
    #[serde(default)]
    pub overrides: HookOverrides,
}

impl HookContext {
    pub fn new(phase: Phase) -> Self {
        Self {
            phase,
            session_id: None,
            agent_id: None,
            data: serde_json::Value::Null,
            cancelled: false,
            overrides: HookOverrides::default(),
        }
    }

    pub fn with_session(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = Some(session_id.into());
        self
    }

    pub fn with_agent(mut self, agent_id: impl Into<String>) -> Self {
        self.agent_id = Some(agent_id.into());
        self
    }

    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = data;
        self
    }

    /// Set a model override on this context.
    pub fn override_model(mut self, model: impl Into<String>) -> Self {
        self.overrides.model = Some(model.into());
        self
    }

    /// Append text to the system prompt via overrides.
    pub fn append_to_prompt(mut self, text: impl Into<String>) -> Self {
        self.overrides.system_prompt_append = Some(text.into());
        self
    }

    /// Prepend text to the system prompt via overrides.
    pub fn prepend_to_prompt(mut self, text: impl Into<String>) -> Self {
        self.overrides.system_prompt_prepend = Some(text.into());
        self
    }

    /// Inject additional tools by name.
    pub fn inject_tools(mut self, tools: Vec<String>) -> Self {
        self.overrides.inject_tools.extend(tools);
        self
    }

    /// Suppress tools by name.
    pub fn suppress_tools(mut self, tools: Vec<String>) -> Self {
        self.overrides.suppress_tools.extend(tools);
        self
    }
}

/// Result of a hook execution.
#[derive(Debug, Clone)]
pub enum HookResult {
    /// Continue to next hook in chain.
    Continue(HookContext),
    /// Short-circuit: skip remaining hooks.
    ShortCircuit(HookContext),
    /// Error: log and continue chain.
    Error(String),
}

/// Trait for hook implementations.
#[async_trait]
pub trait Hook: Send + Sync {
    /// Unique name for this hook (e.g. "session-memory", "command-logger").
    fn name(&self) -> &str;

    /// Phases this hook is registered for.
    fn phases(&self) -> Vec<Phase>;

    /// Priority (lower = runs first). Default: 100.
    fn priority(&self) -> Priority {
        100
    }

    /// Execute the hook. May modify the context.
    async fn execute(&self, ctx: HookContext) -> HookResult;
}

/// Registration entry in the hook manager.
struct HookEntry {
    priority: Priority,
    hook: Arc<dyn Hook>,
}

/// Manages hook registration and phase-based dispatch.
///
/// Hooks are stored in a BTreeMap keyed by (Phase, Priority) for
/// O(log n) registration and O(k) dispatch where k = hooks at a phase.
pub struct HookManager {
    hooks: RwLock<BTreeMap<Phase, Vec<HookEntry>>>,
}

impl HookManager {
    pub fn new() -> Self {
        Self {
            hooks: RwLock::new(BTreeMap::new()),
        }
    }

    /// Register a hook for all of its declared phases.
    pub async fn register(&self, hook: Arc<dyn Hook>) {
        let phases = hook.phases();
        let priority = hook.priority();
        let mut hooks = self.hooks.write().await;

        for phase in phases {
            let entries = hooks.entry(phase).or_default();
            entries.push(HookEntry {
                priority,
                hook: Arc::clone(&hook),
            });
            // Keep sorted by priority (stable sort preserves registration order for equal priorities)
            entries.sort_by_key(|e| e.priority);
            info!(
                hook = hook.name(),
                phase = %phase,
                priority,
                "hook registered"
            );
        }
    }

    /// Unregister a hook by name from all phases.
    pub async fn unregister(&self, hook_name: &str) {
        let mut hooks = self.hooks.write().await;
        for entries in hooks.values_mut() {
            entries.retain(|e| e.hook.name() != hook_name);
        }
        info!(hook = hook_name, "hook unregistered");
    }

    /// Dispatch hooks for a phase. Executes in priority order.
    /// Returns the (possibly modified) context after all hooks run.
    ///
    /// Chain of responsibility: hooks run in order until one short-circuits or all complete.
    pub async fn dispatch(&self, mut ctx: HookContext) -> HookContext {
        let hooks = self.hooks.read().await;
        let entries = match hooks.get(&ctx.phase) {
            Some(e) => e,
            None => return ctx,
        };

        for entry in entries {
            if ctx.cancelled {
                debug!(
                    phase = %ctx.phase,
                    hook = entry.hook.name(),
                    "hook chain cancelled, skipping remaining"
                );
                break;
            }

            match entry.hook.execute(ctx.clone()).await {
                HookResult::Continue(updated) => {
                    ctx = updated;
                }
                HookResult::ShortCircuit(updated) => {
                    ctx = updated;
                    ctx.cancelled = true;
                    debug!(
                        phase = %ctx.phase,
                        hook = entry.hook.name(),
                        "hook short-circuited chain"
                    );
                    break;
                }
                HookResult::Error(err) => {
                    warn!(
                        phase = %ctx.phase,
                        hook = entry.hook.name(),
                        error = %err,
                        "hook error, continuing chain"
                    );
                }
            }
        }

        ctx
    }

    /// List all registered hooks with their phases and priorities.
    pub async fn list_hooks(&self) -> Vec<(String, Vec<Phase>, Priority)> {
        let hooks = self.hooks.read().await;
        let mut seen = std::collections::HashMap::<String, (Vec<Phase>, Priority)>::new();

        for (phase, entries) in hooks.iter() {
            for entry in entries {
                let name = entry.hook.name().to_string();
                let e = seen.entry(name).or_insert_with(|| (vec![], entry.priority));
                e.0.push(*phase);
            }
        }

        seen.into_iter()
            .map(|(name, (phases, priority))| (name, phases, priority))
            .collect()
    }

    /// Count hooks registered at a specific phase.
    pub async fn hook_count(&self, phase: Phase) -> usize {
        let hooks = self.hooks.read().await;
        hooks.get(&phase).map(|e| e.len()).unwrap_or(0)
    }
}

impl Default for HookManager {
    fn default() -> Self {
        Self::new()
    }
}

// ── Built-in Hooks ────────────────────────────────────────

/// A logging hook that records all lifecycle events.
pub struct CommandLoggerHook;

#[async_trait]
impl Hook for CommandLoggerHook {
    fn name(&self) -> &str {
        "command-logger"
    }

    fn phases(&self) -> Vec<Phase> {
        vec![
            Phase::MessageReceive,
            Phase::MessageSend,
            Phase::AfterToolCall,
            Phase::SessionStart,
            Phase::SessionEnd,
        ]
    }

    fn priority(&self) -> Priority {
        0 // Runs first — logging should always fire
    }

    async fn execute(&self, ctx: HookContext) -> HookResult {
        info!(
            phase = %ctx.phase,
            session = ctx.session_id.as_deref().unwrap_or("-"),
            agent = ctx.agent_id.as_deref().unwrap_or("-"),
            "lifecycle event"
        );
        HookResult::Continue(ctx)
    }
}

/// A hook that enforces session memory limits.
pub struct SessionMemoryHook {
    max_messages: usize,
}

impl SessionMemoryHook {
    pub fn new(max_messages: usize) -> Self {
        Self { max_messages }
    }
}

#[async_trait]
impl Hook for SessionMemoryHook {
    fn name(&self) -> &str {
        "session-memory"
    }

    fn phases(&self) -> Vec<Phase> {
        vec![Phase::MessageReceive, Phase::BeforeCompaction]
    }

    fn priority(&self) -> Priority {
        50
    }

    async fn execute(&self, mut ctx: HookContext) -> HookResult {
        if ctx.phase == Phase::BeforeCompaction {
            ctx.data["max_messages"] = serde_json::json!(self.max_messages);
        }
        HookResult::Continue(ctx)
    }
}

// ── Tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    struct TestHook {
        name: String,
        phases: Vec<Phase>,
        priority: Priority,
        short_circuit: bool,
    }

    #[async_trait]
    impl Hook for TestHook {
        fn name(&self) -> &str {
            &self.name
        }
        fn phases(&self) -> Vec<Phase> {
            self.phases.clone()
        }
        fn priority(&self) -> Priority {
            self.priority
        }
        async fn execute(&self, mut ctx: HookContext) -> HookResult {
            let count = ctx.data.get("count").and_then(|v| v.as_u64()).unwrap_or(0);
            ctx.data["count"] = serde_json::json!(count + 1);
            ctx.data["last_hook"] = serde_json::json!(self.name);
            if self.short_circuit {
                HookResult::ShortCircuit(ctx)
            } else {
                HookResult::Continue(ctx)
            }
        }
    }

    #[tokio::test]
    async fn test_hook_registration_and_dispatch() {
        let mgr = HookManager::new();

        mgr.register(Arc::new(TestHook {
            name: "a".into(),
            phases: vec![Phase::MessageReceive],
            priority: 10,
            short_circuit: false,
        }))
        .await;

        mgr.register(Arc::new(TestHook {
            name: "b".into(),
            phases: vec![Phase::MessageReceive],
            priority: 20,
            short_circuit: false,
        }))
        .await;

        let ctx = HookContext::new(Phase::MessageReceive);
        let result = mgr.dispatch(ctx).await;

        assert_eq!(result.data["count"], 2);
        assert_eq!(result.data["last_hook"], "b"); // b runs second
    }

    #[tokio::test]
    async fn test_short_circuit() {
        let mgr = HookManager::new();

        mgr.register(Arc::new(TestHook {
            name: "blocker".into(),
            phases: vec![Phase::MessageReceive],
            priority: 1,
            short_circuit: true,
        }))
        .await;

        mgr.register(Arc::new(TestHook {
            name: "never_runs".into(),
            phases: vec![Phase::MessageReceive],
            priority: 100,
            short_circuit: false,
        }))
        .await;

        let ctx = HookContext::new(Phase::MessageReceive);
        let result = mgr.dispatch(ctx).await;

        assert_eq!(result.data["count"], 1);
        assert_eq!(result.data["last_hook"], "blocker");
        assert!(result.cancelled);
    }

    #[tokio::test]
    async fn test_unregister() {
        let mgr = HookManager::new();

        mgr.register(Arc::new(TestHook {
            name: "temp".into(),
            phases: vec![Phase::Boot],
            priority: 10,
            short_circuit: false,
        }))
        .await;

        assert_eq!(mgr.hook_count(Phase::Boot).await, 1);
        mgr.unregister("temp").await;
        assert_eq!(mgr.hook_count(Phase::Boot).await, 0);
    }

    #[tokio::test]
    async fn test_empty_phase_dispatch() {
        let mgr = HookManager::new();
        let ctx = HookContext::new(Phase::Shutdown);
        let result = mgr.dispatch(ctx).await;
        assert!(!result.cancelled);
    }

    #[tokio::test]
    async fn test_priority_ordering() {
        let mgr = HookManager::new();

        // Register in reverse priority order
        mgr.register(Arc::new(TestHook {
            name: "high".into(),
            phases: vec![Phase::Boot],
            priority: 100,
            short_circuit: false,
        }))
        .await;

        mgr.register(Arc::new(TestHook {
            name: "low".into(),
            phases: vec![Phase::Boot],
            priority: 1,
            short_circuit: false,
        }))
        .await;

        let ctx = HookContext::new(Phase::Boot);
        let result = mgr.dispatch(ctx).await;
        // "low" runs first (priority 1), "high" runs second (priority 100)
        assert_eq!(result.data["last_hook"], "high");
    }

    #[tokio::test]
    async fn test_builtin_command_logger() {
        let mgr = HookManager::new();
        let logger = Arc::new(CommandLoggerHook);
        mgr.register(logger).await;

        assert_eq!(mgr.hook_count(Phase::MessageReceive).await, 1);
        assert_eq!(mgr.hook_count(Phase::MessageSend).await, 1);
        assert_eq!(mgr.hook_count(Phase::Boot).await, 0);
    }

    // ── HookOverrides tests ──────────────────────────────────

    #[test]
    fn test_hook_overrides_default_is_empty() {
        let overrides = HookOverrides::default();
        assert!(overrides.is_empty());
    }

    #[test]
    fn test_hook_overrides_merge_scalar_last_wins() {
        let mut a = HookOverrides {
            model: Some("claude-haiku".into()),
            max_tool_rounds: Some(10),
            ..Default::default()
        };
        let b = HookOverrides {
            model: Some("claude-opus".into()),
            system_prompt_append: Some("Be concise.".into()),
            ..Default::default()
        };
        a.merge(&b);
        assert_eq!(a.model.as_deref(), Some("claude-opus"));
        assert_eq!(a.max_tool_rounds, Some(10)); // not overridden
        assert_eq!(a.system_prompt_append.as_deref(), Some("Be concise."));
    }

    #[test]
    fn test_hook_overrides_merge_lists_concatenate() {
        let mut a = HookOverrides {
            inject_tools: vec!["web_search".into()],
            suppress_tools: vec!["file_write".into()],
            ..Default::default()
        };
        let b = HookOverrides {
            inject_tools: vec!["calculator".into()],
            suppress_tools: vec!["shell_exec".into()],
            ..Default::default()
        };
        a.merge(&b);
        assert_eq!(a.inject_tools, vec!["web_search", "calculator"]);
        assert_eq!(a.suppress_tools, vec!["file_write", "shell_exec"]);
    }

    #[test]
    fn test_hook_context_override_helpers() {
        let ctx = HookContext::new(Phase::BeforeAgentStart)
            .override_model("claude-opus")
            .append_to_prompt("Be helpful.")
            .inject_tools(vec!["web_search".into()])
            .suppress_tools(vec!["shell".into()]);

        assert_eq!(ctx.overrides.model.as_deref(), Some("claude-opus"));
        assert_eq!(ctx.overrides.system_prompt_append.as_deref(), Some("Be helpful."));
        assert_eq!(ctx.overrides.inject_tools, vec!["web_search"]);
        assert_eq!(ctx.overrides.suppress_tools, vec!["shell"]);
        assert!(!ctx.overrides.is_empty());
    }

    /// A hook that sets typed overrides (model + prompt append).
    struct OverrideHook;

    #[async_trait]
    impl Hook for OverrideHook {
        fn name(&self) -> &str {
            "override-hook"
        }
        fn phases(&self) -> Vec<Phase> {
            vec![Phase::BeforeAgentStart]
        }
        async fn execute(&self, ctx: HookContext) -> HookResult {
            let ctx = ctx
                .override_model("claude-haiku")
                .append_to_prompt("Keep responses under 100 words.");
            HookResult::Continue(ctx)
        }
    }

    #[tokio::test]
    async fn test_hook_overrides_propagate_through_dispatch() {
        let mgr = HookManager::new();
        mgr.register(Arc::new(OverrideHook)).await;

        let ctx = HookContext::new(Phase::BeforeAgentStart);
        let result = mgr.dispatch(ctx).await;

        assert_eq!(result.overrides.model.as_deref(), Some("claude-haiku"));
        assert_eq!(
            result.overrides.system_prompt_append.as_deref(),
            Some("Keep responses under 100 words.")
        );
    }
}
