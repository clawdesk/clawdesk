# Changelog

## [Unreleased] — 2026-03-10

### Fixed

#### Gateway Agent Pipeline (Critical)
The HTTP gateway and daemon were missing critical pipeline stages that the
Tauri desktop app had, causing gateway agents to behave as generic chatbots.

- **System prompt**: Gateway `send_message()` hardcoded `"You are a helpful
  assistant."` instead of loading agent personas from the registry. Now resolves
  agent by `agent_id` → `"default"` → first available, using the agent's full
  system prompt. (`routes.rs`)

- **Tools**: Gateway CLI created an empty `ToolRegistry::new()` without calling
  `register_builtin_tools()`. Agents had zero tools (no file, shell, web search).
  Both `gateway run` and `daemon` now register all builtin tools. (`main.rs`)

- **Skills**: Gateway never loaded the 52 bundled skills embedded in the binary.
  Now calls `load_bundled_skills()` and merges with user skills from disk.
  Hot-reload (`reload_skills()`) also preserves bundled skills. (`main.rs`,
  `state.rs`)

- **SkillProvider**: Gateway `AgentRunner` had no `SkillProvider` wired, so
  per-turn skill selection (trigger evaluation + knapsack) never ran. Now builds
  `OrchestratorSkillProvider` from the skill registry and wires it into the
  runner builder. (`routes.rs`)

- **Provider/model ordering**: Provider was resolved before the agent registry
  was consulted, causing model mismatches. Restructured to resolve agent first,
  then derive effective model and provider. (`routes.rs`)

- **WebSocket system prompt**: WS handler streamed with `system_prompt: None`.
  Now resolves agent persona from registry. Added `agent_id` field to
  `WsRequest`. (`ws.rs`)

- **Dynamic spawn**: `dynamic_spawn` handler built an ephemeral system prompt
  but passed `String::new()` to `runner.run()`, discarding it. Now passes the
  actual ephemeral prompt. (`commands.rs`)

- **Empty persona fallback**: Agents created without a persona got an empty
  identity section. Now falls back to `DEFAULT_SYSTEM_PROMPT`. (`commands.rs`)

#### Centralized Defaults
- Added `DEFAULT_SYSTEM_PROMPT` constant in `clawdesk_types::session` so all
  crates share the same capable fallback prompt instead of duplicated strings.
- Updated `AgentConfig::default()`, `SessionConfig::default()`,
  `EnrichedBackend::new()`, gateway routes, and WS handler to use it.

#### General Assistant Template
- Expanded `general-assistant.toml` system prompt from 4 generic lines to
  a comprehensive prompt encouraging proactive tool use.
- Enabled full tool capabilities: `shell`, `filesystem_write`, `http_fetch`.

#### Missing Channel Factory Registrations
Signal, Matrix, Microsoft Teams, and Mastodon had full Rust implementations
(300-500 lines each) but were never registered in `ChannelFactory::with_builtins()`.

- **Signal**: Registered with ConfigSchema (phone_number, rpc_endpoint).
  Env vars: `SIGNAL_PHONE_NUMBER`. (`factory.rs`)
- **Matrix**: Registered with ConfigSchema (homeserver_url, user_id, access_token).
  Env vars: `MATRIX_HOMESERVER_URL`, `MATRIX_USER_ID`, `MATRIX_ACCESS_TOKEN`. (`factory.rs`)
- **MS Teams**: Registered with ConfigSchema (app_id, app_secret, tenant_id).
  Env vars: `TEAMS_APP_ID`, `TEAMS_APP_SECRET`, `TEAMS_TENANT_ID`. (`factory.rs`)
- **Mastodon**: Registered with ConfigSchema (instance_url, access_token, username).
  Env vars: `MASTODON_INSTANCE_URL`, `MASTODON_ACCESS_TOKEN`, `MASTODON_USERNAME`. (`factory.rs`)
- All 4 channels now appear in the Tauri Channels UI as "Available" when env
  vars are not set, or "Connected" when configured. (`state.rs`)

#### Gateway Prompt Pipeline Parity
The gateway HTTP path now uses the same PromptBuilder pipeline as Tauri
desktop, instead of sending raw agent persona strings to the LLM.

- **PromptBuilder integration**: Gateway `send_message()` now runs the full
  `PromptBuilder` knapsack pipeline — scored skills, identity section, runtime
  context (datetime, channel, model), and budget-allocated sections. Previously
  it just fetched the static `agent.system_prompt`. (`routes.rs`)

- **Hook manager**: Added `hook_manager: Arc<HookManager>` field to
  `GatewayState`. Wired into AgentRunner via `.with_hook_manager()` so gateway
  agents fire plugin lifecycle hooks (before-agent-start, before-tool-call,
  etc.). (`state.rs`, `routes.rs`)

- **Channel awareness**: Gateway prompt now includes available channel list
  from the channel registry, enabling cross-channel message routing awareness
  in the system prompt. (`routes.rs`)

### Added

- **41 Google Workspace skills**: Copied all GWS CLI skills into bundled
  skills directory (`openclaw-skills/gws-*`). Includes: gws-drive, gws-gmail,
  gws-gmail-send, gws-gmail-triage, gws-gmail-reply, gws-calendar,
  gws-calendar-agenda, gws-calendar-insert, gws-sheets, gws-sheets-read,
  gws-sheets-append, gws-docs, gws-docs-write, gws-slides, gws-tasks,
  gws-people, gws-chat, gws-chat-send, gws-workflow (6 workflow variants),
  gws-events, gws-forms, gws-keep, gws-meet, gws-classroom,
  gws-admin-reports, gws-modelarmor (4 variants), and gws-shared.
  All embedded via `include_dir!` — no separate installation needed.

- **Google Workspace integration**: Added `google-workspace` entry to the
  Extensions registry (26th integration). Shows in Extensions UI with
  Configure + OAuth buttons. Supports 3 auth methods: `gws` (automated via
  gws CLI), `manual` (Google Cloud Console client ID/secret), or `token`
  (direct access token). OAuth uses the same PKCE flow as other integrations.

- **OAuth template resolution fix**: The `start_extension_oauth` command now
  resolves `${KEY}` templates from user-saved config AND env vars before
  building the auth URL. Previously, clicking "OAuth" on google-calendar
  or google-drive did nothing because `client_id="${GCAL_CLIENT_ID}"` was
  passed literally. Now returns a clear error if the key isn't configured.
  (`commands_extensions.rs`)

- **Skill budget increase**: Gateway prompt `skills_cap` increased from 4096
  to 6144 tokens to accommodate 90+ bundled skills (52 original + 41 GWS).
  The PromptBuilder's knapsack algorithm automatically selects the best
  skills per-turn based on value density. (`routes.rs`)

- **GWS binary co-distribution**: Added `scripts/build-gws.sh` to build the
  Google Workspace CLI (`gws`) from source and ship it in `tools/bundled/gws`.
  The original `gws` crate (Apache-2.0, Google LLC / Justin Poehnelt) is
  built unmodified from its source at `../cli`. License file copied alongside
  binary as `GWS-LICENSE`. ClawDesk agents can call `gws` at runtime for
  Drive, Gmail, Calendar, Sheets, and 17 other Google Workspace APIs.

- **Architecture diagrams**: SVG flow diagrams in `docs/diagrams/`:
  - `clawdesk-tauri-flow.svg` — Full Tauri desktop request→response lineage
    including all 4 entry points, OAuth/PKCE auth, credential vault, profile
    rotation, 17 channels, 52 skills, 15+ tools, MCP bridge, multi-agent
    orchestration, cross-channel routing, 7 persistence stores
  - `clawdesk-gateway-flow.svg` — Gateway HTTP request→response lineage
  - `openclaw-flow.svg` — OpenClaw request→response lineage
  - `clawdesk-vs-openclaw-comparison.svg` — Side-by-side architecture comparison

## [Unreleased] — 2026-02-22

### Added

#### Thread-as-Agent A2A Architecture
Every chat thread is now an A2A-capable agent with task delegation support.

- **`clawdesk-acp` — `thread_agent.rs`** (~540 lines): Core bridge module.
  - Agent-scoped session keys (`agent:{id}:{thread_hex}` format) with
    `agent_session_key()` / `parse_agent_session_key()` roundtrip.
  - `ThreadAgentConfig` — per-thread agent overrides (name, model, capabilities,
    limits).
  - `ThreadInfo` — decoupled thread view to avoid circular crate deps.
  - `thread_agent_card()` — generates per-thread `AgentCard` with capability
    string→enum mapping and metadata.
  - `SpawnRequest` / `SpawnResult` — sub-agent thread spawning primitives.
  - `create_spawn_task()` — wires A2A `Task` with thread bindings and session keys.
  - `ThreadAgentRegistry` — `RwLock<HashMap>` registry with `upsert`/`upsert_card`/
    `get`/`get_by_key`/`remove`/`all_cards`/`count` supporting both `u128` and
    string-based thread IDs.
  - 12 unit tests covering key format, roundtrip, card generation, config overrides,
    spawn task wiring, and registry CRUD.

- **`clawdesk-acp` — `session_router.rs`**: Added 4 thread-affinity methods:
  - `bind_thread_to_agent()` — creates agent-scoped session key + affinity entry.
  - `unbind_thread()` — removes all affinity entries for a thread.
  - `route_for_thread()` — convenience wrapper for thread-aware routing.
  - `register_thread_agent()` — registers card in directory + binds affinity.

- **`clawdesk-acp` — `task.rs`**: Enriched `Task` with thread context:
  `thread_id`, `session_key`, `spawn_mode`, `cleanup`, `announce_on_complete`.
  Added `Task::for_thread()` constructor.

- **`clawdesk-threads` — `types.rs`**: Enriched `ThreadMeta` with `spawn_mode`
  (standalone/run/session), `parent_thread_id`, `capabilities`, `skills`.

- **`clawdesk-gateway` — `subagent_manager.rs`**: Enriched `SubAgentEntry` with
  full lifecycle tracking: `thread_id`, `child_session_key`,
  `requester_session_key`, `task_prompt`, `spawn_mode`, `cleanup`, `outcome`,
  `AnnounceState` enum (Pending/Delivered/Failed/Suppressed).

- **`clawdesk-gateway` — `routes.rs`**: Thread-agent API endpoints:
  - `send_message` now auto-registers each thread as an A2A agent on first
    message. Response includes `agent_id`.
  - `GET /api/v1/thread-agents` — list all registered thread agents.
  - `POST /api/v1/thread-agents/:thread_id/delegate` — delegate a task from
    one thread-agent to another via A2A.

- **`clawdesk-gateway` — `state.rs`**: Added `ThreadAgentRegistry` to
  `GatewayState` for per-thread agent card storage.

### Performance

#### O(1) Rolling Hash for Streaming Integrity (`delta_stream.rs`)
- Replaced FNV-1a full-rehash (`fnv1a_hash(self.assembled.as_bytes())`) with a
  composable polynomial rolling hash mod Mersenne prime (2⁶¹ − 1).
- `DeltaEncoder::push()` now computes H(S ‖ C) = H(S) · p^|C| + H(C) mod M,
  processing only the incoming chunk bytes — O(|chunk|) per delta instead of
  O(|assembled|).
- `DeltaDecoder` uses the rolling hash on the common append path; falls back to
  full rehash only on rare insert/replace operations.
- Eliminates the O(N²) algorithmic trap where streaming N deltas forced
  1 + 2 + … + N bytes of hashing.

#### Wait-Free Task Partitioning via Sharded Map (`server.rs`)
- Replaced `RwLock<FxHashMap<String, Task>>` with `DashMap<String, Task>`
  (internally sharded, each shard independently locked).
- Operations on different tasks no longer contend — eliminates MESI cache-line
  bouncing across cores on the RwLock atomic counter.
- Updated all 4 handler methods (`send_task`, `get_task`, `cancel_task`,
  `provide_input`) and both constructors.
- Added `dashmap = "5.5"` to workspace and `clawdesk-acp` Cargo.toml.

### Added

#### SochDB MemoryBackend Trait & Implementation
- **`clawdesk-storage` — `MemoryBackend` trait** (827 lines): Defined the full
  capability contract for SochDB-backed memory with 25+ trait methods and 20+
  supporting types covering:
  - **Atomic Writes**: `write_atomic`, `recover_atomic_writes`
  - **Knowledge Graph**: `graph_neighbors`, `graph_add_node`, `graph_add_edge`,
    `graph_reachable_memory_ids`
  - **Temporal Graph**: `temporal_add_edge`, `temporal_invalidate_edge`,
    `temporal_edges_at`
  - **Policy Engine**: `policy_check_content`, `policy_check_access`
  - **Trace Store**: `trace_start_span`, `trace_end_span`
  - **Batch Writes (A7)**: `batch_insert_embeddings`
  - **Memory Schema (A4)**: Episodes (`create_episode`, `get_episode`,
    `search_episodes`), Events (`append_event`, `get_timeline`), Entities
    (`upsert_entity`, `get_entity`, `search_entities`, `get_entity_facts`)
  - **Context Assembly (A1)**: `context_query` — token-budgeted context builder
    with truncation strategies (TailDrop, HeadDrop, Proportional, Strict) and
    output formats (Markdown, JSON, Text, Soch)
  - **Task Queue (A8)**: `enqueue_task`, `enqueue_delayed_task`, `claim_task`,
    `ack_task`, `nack_task`, `queue_stats`
  - **Cost Model (A9)**: `search_with_budget`
  - **Filter Pushdown (A12)**: `search_with_filters`
  - **Multi-Vector (A11)**: `insert_multi_vector`, `search_multi_vector`
  - **Path Query (A6)**: `path_query`
  - **SQL / AST Query (A15)**: `sql_query`
  - **Predefined Views (A5)**: `list_views`, `query_view`
  - All methods have default no-op implementations so non-SochDB backends
    compile without changes.

- **`clawdesk-sochdb` — `SochMemoryBackend`** (1106 lines): Full implementation
  of the `MemoryBackend` trait using SochDB's embedded modules:
  - Atomic writes via `AtomicMemoryWriter<SochConn>`
  - Episodes/Events/Entities stored as JSON in SochDB's KV layer with prefix
    scans for search
  - Context query implemented as a pure-Rust token-budgeted assembler
  - Task queue via `sochdb::queue::PriorityQueue` (in-memory, lock-free)
  - Path query and SQL query implemented over `SochConn::scan()` with prefix
    matching and basic SELECT parsing
  - Predefined views via `sochdb_core::predefined_views`
  - Type-safe `SochValue` ↔ `serde_json::Value` conversion helpers

#### MemoryManager Integration
- **`clawdesk-memory` — `MemoryManager`**: Added 20 delegate methods exposing
  all new `MemoryBackend` capabilities through the manager:
  - `batch_insert_embeddings`, `create_episode`, `get_episode`,
    `search_episodes`, `append_event`, `get_timeline`, `upsert_entity`,
    `get_entity`, `search_entities`, `get_entity_facts`, `build_context`,
    `enqueue_task`, `enqueue_delayed_task`, `claim_task`, `ack_task`,
    `nack_task`, `queue_stats`, `path_query`, `sql_query`, `list_views`,
    `query_view`
  - Re-exported all new types from `clawdesk-storage` for downstream consumers.

#### Tauri Commands
- **`clawdesk-tauri` — `commands_memory.rs`**: Added 14 new Tauri IPC commands:
  - `create_episode`, `get_episode`, `search_episodes` — Episode CRUD
  - `append_event`, `get_timeline` — Event timeline management
  - `upsert_entity`, `get_entity`, `search_entities`, `get_entity_facts` —
    Entity graph operations
  - `build_context` — Token-budgeted LLM context assembly
  - `enqueue_task`, `claim_task`, `ack_task` / `nack_task`, `queue_stats` —
    Background task queue
  - `list_views`, `query_view` — Predefined view queries
  - All commands registered in the Tauri invoke handler.

### Fixed
- **`builtin_tools.rs` — String truncation panic**: Fixed two byte-slicing sites
  (`HttpFetchTool` response body and `FileReadTool` content) that panicked when
  `max_response_bytes` / `max_bytes` fell inside a multi-byte UTF-8 character.
  Now walks backward to find a valid char boundary before slicing.
- **`ChatPage.tsx` — Invalid DOM nesting**: Changed outer `<button>` wrapping
  thread sidebar items to `<div role="button">` to fix React warning about
  `<button>` nested inside `<button>` (delete button inside clickable row).
- **`conversation.rs` — Messages lost on restart**: `append_message()` used
  non-durable `put()` (no commit), so individual messages written to the
  `sessions/` keyspace were discarded during WAL recovery. Now uses
  `put_durable()` for immediate commit. `append_messages()` batch variant
  switched from individual `put()` calls to `put_batch()` for a single
  atomic commit.
- **`commands.rs` — Deleted chats reappearing on restart**: `delete_chat()`
  called `soch_store.delete()` without committing the transaction, so the
  deletion was lost on WAL recovery and the chat silently reappeared. Now
  calls `commit()` after delete and also cleans up the associated
  `tool_history/` key.
- **`lib.rs` — WAL backup file accumulation**: Old `wal.log.backup.*` and
  `wal.log.corrupt.*` files from retry-quarantine cycles were never cleaned
  up, leading to unbounded disk usage (~80 MB observed). After a successful
  SochDB open + self-test, these files are now automatically removed.
