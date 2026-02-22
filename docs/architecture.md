# Architecture Overview

ClawDesk is a 27-crate Rust workspace following hexagonal (ports-and-adapters) architecture. Every component communicates through trait-defined ports, enabling independent testing and swappable implementations.

## Design Principles

1. **Privacy-first** — All data stays local. SochDB runs embedded, no cloud sync required.
2. **Zero-trust** — Every message is scanned, every tool call gated, every action audited.
3. **Offline-capable** — Works with local models (Ollama) when disconnected.
4. **Composable** — Skills, plugins, channels, and providers are all pluggable.
5. **Observable** — Full OpenTelemetry tracing, structured audit logs, real-time event streaming.

## Crate Dependency Graph

The crate DAG has a critical path depth of 6. Dependencies flow strictly upward — no cycles.

```
                         ┌─────────────┐
                         │clawdesk-types│  ← Zero-dep leaf: shared types
                         └──────┬──────┘
                                │
                    ┌───────────┼───────────┐
                    ▼           ▼           ▼
             ┌──────────┐ ┌─────────┐ ┌──────────┐
             │  storage  │ │  domain │ │  sochdb  │
             │ (traits)  │ │ (pure)  │ │ (impl)   │
             └──────────┘ └─────────┘ └──────────┘
                    │           │           │
          ┌────────┴───┐       │     ┌─────┴──────┐
          ▼            ▼       ▼     ▼            ▼
    ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐
    │ channel  │ │providers │ │  memory  │ │ security │
    │ (traits) │ │ (8 LLMs) │ │(embed/BM)│ │(scan/acl)│
    └──────────┘ └──────────┘ └──────────┘ └──────────┘
          │           │           │           │
          ▼           ▼           ▼           ▼
    ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌──────────┐
    │ channels │ │  agents  │ │  skills  │ │  plugin  │
    │ (25+ im) │ │ (runner) │ │(registry)│ │ (hooks)  │
    └──────────┘ └──────────┘ └──────────┘ └──────────┘
          │           │           │           │
          └───────────┴─────┬─────┴───────────┘
                            ▼
                    ┌──────────────┐
                    │   gateway    │  ← HTTP/WS API server
                    │ (Axum 0.7)  │
                    └──────┬──────┘
                           │
              ┌────────────┼────────────┐
              ▼            ▼            ▼
        ┌──────────┐ ┌──────────┐ ┌──────────┐
        │  tauri   │ │   cli    │ │   tui    │
        │(desktop) │ │(command) │ │(terminal)│
        └──────────┘ └──────────┘ └──────────┘
```

## Crate Reference

### Core Layer

| Crate | Purpose |
|-------|---------|
| `clawdesk-types` | Shared types: errors, messages, channels, sessions, config, tokenizer |
| `clawdesk-storage` | Storage trait definitions (ports): `SessionStore`, `ConversationStore`, `VectorStore`, `GraphStore` |
| `clawdesk-domain` | Pure business logic — no I/O. Context guard, compaction, routing, prompt building, rate limiting |
| `clawdesk-sochdb` | SochDB embedded ACID database adapter. WAL + buffered commit, MVCC, HNSW vector search |

### Agent Engine

| Crate | Purpose |
|-------|---------|
| `clawdesk-agents` | Agent execution engine: `AgentRunner`, multi-agent pipelines, failover, tool orchestration, context window management |
| `clawdesk-providers` | LLM provider abstraction + 8 implementations: Anthropic, OpenAI, Google Gemini, Ollama, Azure, Bedrock, Cohere, Vertex |
| `clawdesk-skills` | Composable skill system: registry, trigger evaluation, token-budgeted selection, env injection, verification |
| `clawdesk-plugin` | Plugin lifecycle: hooks, sandbox, dependency resolution, SDK for plugin authors |

### Memory & Knowledge

| Crate | Purpose |
|-------|---------|
| `clawdesk-memory` | Embeddings, BM25, hybrid search (RRF), batch pipeline, temporal decay, MMR diversity |

### Communication

| Crate | Purpose |
|-------|---------|
| `clawdesk-channel` | Channel trait hierarchy: `Channel` → `Threaded` + `Streaming` + `Reactions` → `RichChannel` |
| `clawdesk-channels` | 25+ channel implementations: Slack, Discord, Telegram, WhatsApp, Signal, Matrix, IRC, etc. |
| `clawdesk-bus` | Event-sourced reactive bus with weighted fair queuing and backpressure |
| `clawdesk-autoreply` | Auto-reply engine: classify → route → enrich → execute → format → deliver |
| `clawdesk-threads` | Namespaced chat-thread persistence on SochDB |
| `clawdesk-acp` | Agent-to-Agent Communication Protocol (A2A). Agent Cards, task FSM, capability discovery |

### Security

| Crate | Purpose |
|-------|---------|
| `clawdesk-security` | Audit logging (hash-chained), content scanning (Aho-Corasick + regex), ACL, OAuth2 + PKCE, execution approval, credential vault, TLS cert pinning |

### Networking

| Crate | Purpose |
|-------|---------|
| `clawdesk-gateway` | Axum HTTP/WS gateway: REST API, OpenAI-compatible API, admin routes, WebSocket streaming |
| `clawdesk-tunnel` | WireGuard-based P2P encrypted networking. Userspace — no root required. NAT traversal via STUN |
| `clawdesk-discovery` | mDNS service advertisement + SPAKE2 password-authenticated pairing |

### Infrastructure

| Crate | Purpose |
|-------|---------|
| `clawdesk-runtime` | Durable crash-recoverable agent execution. Checkpoint + resume, activity journal, dead-letter queue |
| `clawdesk-infra` | Backup, clipboard, daemon management, dispatch queue, git-sync, idle detection, notifications, TLS |
| `clawdesk-media` | Media pipeline: audio transcription, image analysis, document parsing, TTS, link previews |
| `clawdesk-browser` | Browser automation via Chrome DevTools Protocol (CDP): navigate, click, type, screenshot |
| `clawdesk-cron` | Cron scheduling with overlap prevention, heartbeat monitoring, proactive orchestration |
| `clawdesk-adapters` | External service adapter trait with OAuth2 lifecycle, rate limiting, circuit breaking |

### Observability

| Crate | Purpose |
|-------|---------|
| `clawdesk-observability` | OpenTelemetry tracing + metrics following GenAI semantic conventions. OTLP export |
| `clawdesk-telemetry` | Metrics, tracing, and logging initialization. Health status tracking |

### Frontends

| Crate | Purpose |
|-------|---------|
| `clawdesk-tauri` | Tauri 2.0 desktop application shell: ~138 IPC commands, AppState, system tray |
| `clawdesk-cli` | Command-line interface: `gateway run`, `message send`, `config`, `doctor` |
| `clawdesk-tui` | Terminal UI for interactive chat with streaming and model picking |
| `ui` | React + TypeScript + Vite frontend: 7 pages, Tailwind CSS, Tauri IPC bindings |

## Data Flow: Message Processing

When a user sends a message, the following pipeline executes:

```
User Input (UI)
    │
    ▼
┌─── IPC: send_message ──────────────────────────────────┐
│                                                         │
│  1. Security Scan         CascadeScanner                │
│     └─ Aho-Corasick → Regex → verdict (pass/flag)      │
│                                                         │
│  2. Agent Resolution      agents + model override       │
│     └─ Find agent by ID, apply model override if set    │
│                                                         │
│  3. Session Management    sessions hot cache + SochDB   │
│     └─ Create/load session, persist user message        │
│                                                         │
│  4. Provider Resolution   ProviderNegotiator            │
│     └─ Capability-based routing to best provider        │
│                                                         │
│  5. History Assembly      tool history merge + sort     │
│     └─ Session messages + tool messages by timestamp    │
│                                                         │
│  6. Context Guard         ContextGuard + compaction     │
│     └─ Check αC threshold → compact if needed           │
│                                                         │
│  7. Skill Selection       TriggerEvaluator + knapsack   │
│     └─ Score skills → budget-constrained packing        │
│                                                         │
│  8. Prompt Assembly       PromptBuilder                 │
│     └─ Identity + skills + memory + runtime context     │
│                                                         │
│  9. Memory Recall         MemoryManager → hybrid search │
│     └─ Inject memory pre-user-message for recency bias  │
│                                                         │
│ 10. Semantic Cache Check  SochDB SemanticCache          │
│     └─ Short-circuit if cache hit                       │
│                                                         │
│ 11. Session Lane          SessionLaneManager::acquire() │
│     └─ Serialize concurrent runs per session            │
│                                                         │
│ 12. Agent Runner          AgentRunner::run()            │
│     └─ Hook dispatch → bootstrap → LLM loop → chunking │
│                                                         │
│ 13. Response Persistence  session + SochDB + cache      │
│     └─ Store assistant message, cache response          │
│                                                         │
│ 14. Event Streaming       broadcast channel → UI        │
│     └─ Real-time token streaming to frontend            │
│                                                         │
└─────────────────────────────────────────────────────────┘
```

## Storage Architecture

All persistent state flows through **SochDB**, an embedded ACID database.

```
              SochDB (single database file)
    ┌─────────────────────────────────────────────┐
    │                                             │
    │  Sessions     agents/{id}/sessions/{sid}    │
    │  Messages     agents/{id}/messages/{mid}    │
    │  Tools        tools/{chat_id}/{timestamp}   │
    │  Threads      threads/{id}                  │
    │  Skills       skills/{id}                   │
    │  Configs      config/{key}                  │
    │  Checkpoints  runtime:checkpoints/{id}      │
    │  Agent Reg    a2a:agents/{id}               │
    │                                             │
    │  ── Vector Store ──                         │
    │  HNSW index for embedding similarity search │
    │                                             │
    │  ── Semantic Cache ──                       │
    │  Query → response cache with TTL            │
    │                                             │
    │  ── Knowledge Graph ──                      │
    │  Nodes + edges for entity relationships     │
    │                                             │
    │  ── Temporal Graph ──                       │
    │  Time-scoped fact assertions                │
    │                                             │
    │  ── Trace Store ──                          │
    │  OTEL-compatible span storage               │
    │                                             │
    │  ── Policy Engine ──                        │
    │  Rate limits, access policies               │
    │                                             │
    └─────────────────────────────────────────────┘
```

### Write Modes

| Mode | Guarantee | Use Case |
|------|-----------|----------|
| `put()` | Group-commit (batched) | Normal writes, high throughput |
| `put_durable()` | Immediate commit + fsync | User messages before LLM call |
| `put_batch()` | Multi-key atomic commit | Bulk imports, migrations |

### Durability

- **Write-Ahead Log (WAL)** ensures crash recovery
- **Periodic checkpoints** every 30 seconds
- **Explicit sync** after user message persistence (before LLM call)
- **On-exit checkpoint** with fsync on application close

## Concurrency Model

ClawDesk uses Tokio for async execution with several concurrency patterns:

| Pattern | Where | Purpose |
|---------|-------|---------|
| `SessionLaneManager` | commands.rs | One agent run per session at a time |
| `CancellationToken` | AgentRunner | Cooperative cancellation of agent loops |
| `broadcast::channel` | Event streaming | Fan-out agent events to UI + trace collector |
| `JoinSet` | Tool execution | Bounded parallel tool calls with semaphore |
| `RwLock` | AppState fields | Concurrent reads, exclusive writes |
| `parking_lot::Mutex` | SochDB | Low-latency serialized DB access |
| `Arc` + `AtomicU64` | Cost tracking | Lock-free token/cost counters |

## Build Configuration

```toml
# Release profile (Cargo.toml)
[profile.release]
lto = "fat"            # Full link-time optimization
codegen-units = 1      # Maximum optimization
strip = true           # Strip debug symbols
panic = "abort"        # No unwinding overhead
```

Key external dependencies:
- **Tauri 2.0** — Desktop application framework
- **Axum 0.7** — HTTP/WebSocket server
- **Tokio** — Async runtime (full features)
- **SochDB** — Embedded ACID vector database
- **OpenTelemetry** — Distributed tracing
- **ed25519-dalek** + **sha2** — Cryptographic primitives
- **aho-corasick** — Fast multi-pattern text search
- **parking_lot** — Low-latency synchronization primitives
