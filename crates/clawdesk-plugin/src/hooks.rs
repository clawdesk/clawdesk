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

/// Context passed to hooks — mutable data the hook can inspect/modify.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookContext {
    pub phase: Phase,
    pub session_id: Option<String>,
    pub agent_id: Option<String>,
    pub data: serde_json::Value,
    /// If set to true by a hook, the chain is short-circuited.
    pub cancelled: bool,
}

impl HookContext {
    pub fn new(phase: Phase) -> Self {
        Self {
            phase,
            session_id: None,
            agent_id: None,
            data: serde_json::Value::Null,
            cancelled: false,
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
}
