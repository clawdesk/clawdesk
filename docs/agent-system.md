# Agent System

The agent system is the core execution engine of ClawDesk. It handles everything from receiving a user message to streaming the response back to the UI.

## AgentRunner

`AgentRunner` is the central type that orchestrates agent execution. It lives in `crates/clawdesk-agents/src/runner.rs`.

### Construction

```rust
let runner = AgentRunner::new(
    provider,           // Arc<dyn Provider> — LLM backend
    tool_registry,      // Arc<ToolRegistry> — available tools
    config,             // AgentConfig — model, limits, etc.
    cancel_token,       // CancellationToken — cooperative cancellation
)
.with_events(event_tx)                  // broadcast::Sender<AgentEvent>
.with_approval_gate(gate)              // human-in-the-loop tool approval
.with_context_guard(guard)             // pre-compacted context guard
.with_hook_manager(hooks)              // plugin hook dispatch
.with_channel_context(channel_ctx)     // channel-aware formatting
.with_skill_provider(skill_provider)   // per-turn skill selection
.with_profile_rotator(rotator)         // API key rotation
.with_sandbox_gate(sandbox);           // sandbox enforcement
```

### AgentConfig

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `model` | `String` | `claude-sonnet-4-20250514` | Model identifier |
| `system_prompt` | `String` | `"You are a helpful assistant."` | Base system prompt |
| `max_tool_rounds` | `usize` | `25` | Max tool call rounds before forced stop |
| `context_limit` | `usize` | `128_000` | Total context window budget (tokens) |
| `response_reserve` | `usize` | `8_192` | Reserved tokens for response generation |
| `provider_quirks` | `ProviderQuirks` | `default` | Provider-specific behavior (alternation, tool placement) |
| `workspace_path` | `Option<String>` | `None` | Workspace directory for file tools |
| `failover` | `Option<FailoverConfig>` | `None` | Multi-stage failover configuration |
| `bootstrap` | `Option<BootstrapConfig>` | `None` | Bootstrap file discovery config |

## Execution Pipeline

When `runner.run(history, system_prompt)` is called, the following stages execute in order:

### Stage 1: Hook Dispatch

```
MessageReceive hook → BeforeAgentStart hook
```

Plugins can inspect or cancel the run. Hooks can set typed overrides via `HookOverrides`:
- **Model override** — Switch to a different model
- **System prompt prepend/append** — Inject additional instructions
- **Tool injection/suppression** — Activate or deactivate tools
- **Max tool rounds override** — Adjust iteration limits

### Stage 2: History Sanitization

Provider-specific quirks are applied:
- **Google Gemini** — Requires strict user/assistant alternation
- **Tool placement** — Some providers require tool calls only after user turns

### Stage 3: Bootstrap Context

If `workspace_path` is configured, ClawDesk discovers project files:
- `CLAUDE.md`, `AGENTS.md` — Project instructions
- `README.md`, `Cargo.toml`, `package.json` — Project metadata
- `tsconfig.json`, `.eslintrc` — Configuration files

Bootstrap content is budget-limited to **25% of context_limit** to leave room for conversation.

### Stage 4: Channel Context Injection

If a `ChannelContext` is set, channel-specific formatting instructions are appended to the system prompt:

```
## Channel Context
You are responding in channel: slack
Capabilities: threading=true, streaming=true, reactions=true, media=true
Maximum message length: 4000 characters
Markup format: mrkdwn
```

### Stage 5: Skill Selection

The `SkillProvider` selects relevant skills for this turn:

1. **Trigger Evaluation** — Each skill's triggers are checked against the user message
2. **Relevance Scoring** — Skills are scored by keyword/pattern match relevance
3. **Token Budget** — Skills are packed into a budget of **20% of context_limit** using a greedy knapsack algorithm
4. **Prompt Injection** — Selected skill prompt fragments are appended to the system prompt

### Stage 6: Execute Loop

The core LLM interaction loop:

```
┌──────────────────────────────────────────┐
│                                          │
│  Assemble messages (system + history)    │
│          │                               │
│          ▼                               │
│  Call LLM (stream or complete)           │
│          │                               │
│          ▼                               │
│  Check finish reason                     │
│     │           │          │             │
│  ToolUse      Stop     MaxTokens        │
│     │           │          │             │
│     ▼           ▼          ▼             │
│  Execute     Return    Compaction        │
│  Tools       response  + retry           │
│     │                                    │
│     ▼                                    │
│  Append tool results to history          │
│     │                                    │
│     ▼                                    │
│  Check round count < max_tool_rounds     │
│     │          │                         │
│   Yes          No                        │
│     │          │                         │
│  Continue   Return                       │
│  loop       response                     │
│                                          │
└──────────────────────────────────────────┘
```

### Stage 7: Response Chunking

If a `ChannelContext` is set, the response is split into delivery-ready segments using intelligent boundary detection:

1. **Paragraph breaks** (double newline) — preferred
2. **Sentence boundaries** (`. `) — fallback
3. **Line breaks** (`\n`) — next fallback
4. **Word boundaries** (space) — last resort
5. **Hard split** — if all else fails

Each `ResponseSegment` carries:
- `content` — The text chunk
- `part` / `total_parts` — Part numbering
- `media_urls` — Attached media (first segment only)
- `reply_to_id` — Thread parent message ID
- `is_error` — Error flag for special rendering
- `audio_as_voice` — Voice message hint

## Tool System

Tools extend agent capabilities with executable functions.

### Tool Trait

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn schema(&self) -> ToolSchema;
    async fn execute(&self, args: serde_json::Value) -> Result<String, String>;
}
```

### ToolRegistry

Tools are registered in a `ToolRegistry`:

```rust
let mut registry = ToolRegistry::new();
registry.register(Arc::new(MyTool::new()));
```

### ToolPolicy

Tool execution is governed by `ToolPolicy`:

| Field | Type | Description |
|-------|------|-------------|
| `allowed_tools` | `Vec<String>` | Allowlist (empty = allow all) |
| `denied_tools` | `Vec<String>` | Denylist |
| `require_approval` | `Vec<String>` | Tools requiring human approval |
| `max_concurrent` | `usize` | Maximum parallel tool calls |
| `timeout_ms` | `u64` | Per-tool timeout |

### Built-in Tools

| Tool | Description |
|------|-------------|
| `message_send` | Send a message to a channel recipient |

The `message_send` tool has full schema validation, delegating to a callback for actual delivery. `MessagingToolTracker` deduplicates sends and records them on `AgentResponse.messaging_sends`.

### Parallel Execution

Tool calls are executed concurrently using Tokio's `JoinSet` with a `Semaphore` for bound concurrency. Results are collected and re-sorted into invocation order for deterministic LLM context.

### Human Approval Gate

The `ApprovalGate` trait enables human-in-the-loop approval for sensitive tools:

```rust
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    async fn request_approval(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Result<bool, String>;
}
```

When a tool in `ToolPolicy.require_approval` is called, the gate prompts the user in the UI before execution proceeds.

## Context Window Management

ClawDesk aggressively manages the context window to prevent overflow.

### Context Guard

`ContextGuard` (from `clawdesk-domain`) monitors token usage and triggers compaction:

| Threshold | Action |
|-----------|--------|
| < 80% of context_limit | `Ok` — no action |
| 80-95% | `Compact` — tiered compaction |
| 95-100% | `ForceTruncate` — budget-based tail retention |
| 3+ compaction failures | `CircuitBroken` — emergency truncation |

### Compaction Levels

1. **DropMetadata** — Truncate long tool results to 500 characters
2. **SummarizeOld** — Keep recent half, summarize older messages via LLM
3. **Truncate** — Keep only the 10 most recent messages

### Predictive Compaction

Inside the execute loop, the runner performs predictive compaction:
- Before each LLM call, estimate if the next response might overflow
- If estimated usage > threshold, compact proactively
- On `ContextLengthExceeded` error, apply emergency compaction and retry

### Tool Result Truncation

Large tool results are truncated to prevent context bloat:
- Results > 5,000 characters are trimmed
- Truncation indicator `...[truncated]` is appended
- Tool results from earlier rounds are progressively shortened

## Failover System

The failover system provides multi-stage error recovery.

### FailoverController

A deterministic finite automaton (DFA) that manages retry strategy:

```
                ┌──────────┐
                │   Init   │
                └────┬─────┘
                     │ first call
                     ▼
              ┌──────────────┐
         ┌────│  TryProfile  │────┐
         │    └──────────────┘    │
    success         │          exhaust
         │     next profile       │
         ▼          │             ▼
    ┌─────────┐     │      ┌───────────┐
    │ Success │     │      │ TryModel  │
    └─────────┘     │      └─────┬─────┘
                    │            │
                    │       next model
                    │            │
                    │            ▼
                    │     ┌────────────────┐
                    │     │ TryThinkLevel  │
                    │     └───────┬────────┘
                    │             │
                    │        exhaust all
                    │             │
                    │             ▼
                    │      ┌───────────┐
                    └──────│ Exhausted │
                           └───────────┘
```

### Profile Rotation

`ProfileRotator` manages multiple API key profiles with weighted selection:
- **Weighted random** — Higher-weight profiles are selected more often
- **Failure recording** — Failed profiles get cooldown periods
- **Success tracking** — Successful profiles get weight boosts
- **Failure classification** — Rate limit, auth error, billing, server error, timeout

### Error Classification

Errors are classified for appropriate recovery:

| Error Type | Retryable | Recovery |
|------------|-----------|----------|
| Rate limit | Yes | Exponential backoff |
| Auth failure | Yes | Rotate to next profile |
| Billing | No | Rotate + notify user |
| Server error (5xx) | Yes | Backoff + retry |
| Context length exceeded | Yes | Compact + retry |
| Timeout | Yes | Retry with backoff |
| Other | No | Propagate immediately |

## Multi-Agent Pipelines

ClawDesk supports composing multiple agents into DAG pipelines.

### Pipeline Structure

```rust
let pipeline = PipelineBuilder::new("analysis-pipeline")
    .add_step("summarizer", PipelineStep::Agent { 
        agent_config: summarizer_config,
        timeout_ms: 30_000,
    })
    .add_step("critic", PipelineStep::Agent {
        agent_config: critic_config,
        timeout_ms: 30_000,
    })
    .add_step("final", PipelineStep::Agent {
        agent_config: final_config,
        timeout_ms: 60_000,
    })
    .add_dependency("critic", "summarizer")
    .add_dependency("final", "critic")
    .build()?;
```

### Step Types

| Type | Description |
|------|-------------|
| `Agent` | Single agent execution with config + timeout |
| `Parallel` | Execute multiple steps concurrently |
| `Router` | Dynamic routing based on input content |
| `Gate` | Human approval checkpoint |
| `Transform` | Data transformation between steps |

### Merge Strategies

When parallel steps converge, their outputs are merged:

| Strategy | Description |
|----------|-------------|
| `Concat` | Concatenate all outputs |
| `Structured` | Structured JSON merge |
| `FirstSuccess` | Use the first successful result |
| `Best` | Select the best result by quality score |
| `Council` | LLM-based evaluation of all outputs |

### Pipeline Router

`PipelineRouter` uses a LinUCB bandit algorithm for per-step model selection:
- Learns which models work best for different types of input
- Supports model pinning for deterministic behavior
- Records routing history for feedback-driven improvement

### Pipeline Backend

`RunnerBackend` bridges pipelines to the agent runner:
- Creates a fresh `AgentRunner` per pipeline step
- Configuration per agent via `PipelineAgentConfig`
- Shared provider, tool registry, and cancellation token

## Event System

The agent emits events throughout execution for monitoring and streaming.

### AgentEvent Variants

| Event | Data | Purpose |
|-------|------|---------|
| `RoundStart` | `round: usize` | Tool loop round started |
| `Response` | `content, finish_reason` | LLM response received |
| `ToolStart` | `name, args` | Tool execution beginning |
| `ToolEnd` | `name, success, duration_ms` | Tool execution complete |
| `Compaction` | `level, tokens_before, tokens_after` | Context compaction occurred |
| `StreamChunk` | `text, done` | Streaming token chunk |
| `Done` | `total_rounds` | Agent run complete |
| `Error` | `error: String` | Error occurred |
| `PromptAssembled` | `total_tokens, skills, memory, budget` | Prompt assembly details |
| `SkillDecision` | `skill_id, included, reason, cost` | Skill selection decision |
| `ContextGuardAction` | `action, token_count, threshold` | Context guard intervened |
| `FallbackTriggered` | `from_model, to_model, reason` | Model fallback occurred |
| `IdentityVerified` | `hash_match, version` | Identity contract verified |

Events are broadcast via `tokio::sync::broadcast` and consumed by:
1. **UI streaming** — Real-time token display
2. **Trace collector** — Full execution trace for debugging
3. **Audit logger** — Security-relevant events logged

## Session Management

### Session Lanes

`SessionLaneManager` ensures only one agent run per session at a time:

```rust
let _guard = state.session_lanes.acquire(&session_key).await?;
// guard is held until dropped — next run waits
```

Features:
- Per-session mutex with RAII guard (auto-release on drop)
- Watchdog timeout (300s) prevents deadlocks from crashed runs
- Async-aware (uses `tokio::sync::Mutex` internally)

### Transactional Lanes

`TransactionalLaneManager` adds write buffering:
- All writes within a transaction are buffered
- Committed atomically on success
- Rolled back on failure

## Durable Execution

The `clawdesk-runtime` crate provides crash-recoverable agent execution:

### How It Works

1. Before each side-effect, the activity is journaled to SochDB
2. On crash recovery, the journal is replayed
3. Completed activities are skipped (idempotency)
4. Failed activities are retried or moved to dead-letter queue

### Key Types

| Type | Purpose |
|------|---------|
| `DurableAgentRunner` | Crash-safe wrapper around `AgentRunner` |
| `ActivityJournal` | WAL for side-effects |
| `CheckpointStore` | Agent state snapshots |
| `RecoveryManager` | Crash recovery orchestration |
| `DeadLetterQueue` | Failed activity storage |
| `LeaseManager` | Distributed lease management |

## Workspace Confinement

When `workspace_path` is set, `WorkspaceGuard` confines all file operations:

- Path canonicalization prevents symlink escapes
- All file tool paths are validated against the workspace root
- Operations outside the workspace are rejected
- Workspace context files are discovered and injected into the system prompt
