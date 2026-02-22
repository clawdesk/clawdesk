# API Reference

ClawDesk exposes APIs through two interfaces: **Tauri IPC** (desktop app) and **HTTP/WebSocket Gateway** (server mode).

## Tauri IPC Commands

IPC commands are invoked from the React frontend via `@tauri-apps/api`:

```typescript
import { invoke } from '@tauri-apps/api/core';

const response = await invoke('send_message', {
  agentId: 'agent-uuid',
  request: {
    content: 'Hello, world!',
    chatId: 'chat-uuid',
  }
});
```

### Core Agent Commands

#### `send_message`

Send a message to an agent and receive a response.

| Parameter | Type | Required | Description |
|-----------|------|----------|-------------|
| `agentId` | `string` | Yes | Agent UUID |
| `request.content` | `string` | Yes | Message text |
| `request.chatId` | `string` | No | Existing chat ID (creates new if omitted) |
| `request.providerOverride` | `string` | No | Provider name override |
| `request.apiKey` | `string` | No | API key for provider override |
| `request.baseUrl` | `string` | No | Custom base URL |

**Returns**: `SendMessageResponse`
```typescript
{
  message: ChatMessage,     // Assistant's response
  trace: TraceEntry[],      // Execution trace
  chatId: string,           // Chat session ID
  chatTitle?: string,       // Auto-generated title (new chats only)
}
```

#### `create_agent`

Create a new agent.

| Parameter | Type | Description |
|-----------|------|-------------|
| `name` | `string` | Display name |
| `model` | `string` | Model identifier |
| `persona` | `string` | System prompt / persona |
| `skills` | `string[]` | Skill IDs to activate |
| `tokenBudget` | `number` | Context window size |

#### `list_agents`

List all configured agents.

**Returns**: `Agent[]`

#### `delete_agent`

Delete an agent by ID.

| Parameter | Type | Description |
|-----------|------|-------------|
| `agentId` | `string` | Agent UUID |

#### `clone_agent`

Create a copy of an agent.

| Parameter | Type | Description |
|-----------|------|-------------|
| `agentId` | `string` | Source agent UUID |

### Session Commands

#### `list_sessions`

List all chat sessions.

**Returns**: `ChatSession[]`
```typescript
{
  id: string,
  agentId: string,
  title: string,
  messages: ChatMessage[],
  createdAt: string,
  updatedAt: string,
}
```

#### `get_session_messages` / `get_chat_messages`

Get messages for a specific chat session.

| Parameter | Type | Description |
|-----------|------|-------------|
| `chatId` | `string` | Chat session ID |

#### `create_chat`

Create a new chat session for an agent.

#### `delete_chat`

Delete a chat session.

#### `update_chat_title`

Update the title of a chat session.

#### `export_session_markdown` / `export_session_json`

Export a session in markdown or JSON format.

### Skill Commands

#### `list_skills`

List all registered skills with activation status.

**Returns**: `SkillInfo[]`
```typescript
{
  id: string,
  displayName: string,
  description: string,
  active: bool,
  trustLevel: string,
  triggerKeywords: string[],
  tokenCost: number,
}
```

#### `activate_skill` / `deactivate_skill`

Toggle skill activation by ID.

#### `register_skill`

Register a new skill from a manifest path.

| Parameter | Type | Description |
|-----------|------|-------------|
| `manifestPath` | `string` | Path to skill manifest.toml |

#### `validate_skill`

Validate a skill manifest without registering.

#### `get_skill_detail`

Get detailed information about a specific skill.

### Memory Commands

#### `remember_memory`

Store a new memory.

| Parameter | Type | Description |
|-----------|------|-------------|
| `content` | `string` | Memory content |
| `metadata` | `object` | Optional metadata (source, tags) |

#### `remember_batch`

Store multiple memories at once.

| Parameter | Type | Description |
|-----------|------|-------------|
| `entries` | `{content, metadata}[]` | Array of memory entries |

#### `recall_memories`

Search memories by query.

| Parameter | Type | Description |
|-----------|------|-------------|
| `query` | `string` | Search query |
| `limit` | `number` | Max results (default: 10) |

**Returns**: `MemoryResult[]`
```typescript
{
  content: string,
  score: number,
  metadata: { source?: string, tags?: string[] },
}
```

#### `forget_memory`

Delete a specific memory.

#### `get_memory_stats`

Get memory system statistics.

**Returns**: `MemoryStats`
```typescript
{
  totalMemories: number,
  totalChunks: number,
  indexSize: number,
  cacheHitRate: number,
}
```

### Security Commands

#### `get_security_status`

Get overall security posture.

#### `start_oauth_flow`

Begin an OAuth2 authorization flow.

| Parameter | Type | Description |
|-----------|------|-------------|
| `provider` | `string` | OAuth provider name |
| `scopes` | `string[]` | Requested scopes |

#### `list_auth_profiles`

List all authentication profiles.

#### `generate_scoped_token`

Create a capability-scoped token.

| Parameter | Type | Description |
|-----------|------|-------------|
| `scope` | `string` | Token scope (chat, admin, tools, read) |
| `ttlHours` | `number` | Token lifetime |

#### `add_acl_rule` / `check_permission` / `revoke_acl`

Manage access control rules.

#### `create_approval_request` / `approve_request` / `deny_request`

Human-in-the-loop execution approval.

### Observability Commands

#### `get_metrics`

Get system metrics.

**Returns**: `SystemMetrics`
```typescript
{
  totalMessages: number,
  totalTokens: number,
  totalCostUsd: number,
  avgResponseMs: number,
  activeAgents: number,
  uptime: number,
}
```

#### `get_agent_trace`

Get the execution trace for an agent run.

**Returns**: `AgentTrace`

### Configuration Commands

#### `get_config`

Get current configuration.

#### `list_models`

List available models across all configured providers.

#### `test_llm_connection`

Test connectivity to a specific provider.

| Parameter | Type | Description |
|-----------|------|-------------|
| `provider` | `string` | Provider name |
| `apiKey` | `string` | API key to test |

### Thread Commands

#### `create_thread` / `get_thread` / `delete_thread`

Thread CRUD operations.

#### `append_thread_message` / `get_thread_messages`

Message management within threads.

#### `thread_stats`

Get thread statistics (message count, size, activity).

### Canvas Commands

#### `create_canvas` / `get_canvas` / `update_canvas` / `delete_canvas`

Canvas CRUD operations.

#### `add_canvas_block` / `update_canvas_block` / `delete_canvas_block`

Block-level canvas editing.

#### `export_canvas_markdown`

Export canvas as markdown document.

### SochDB Advanced Commands

#### Semantic Cache

| Command | Description |
|---------|-------------|
| `semantic_cache_lookup` | Query the cache for similar queries |
| `semantic_cache_store` | Store a query-response pair |
| `semantic_cache_invalidate` | Invalidate cache entries |

#### Tracing

| Command | Description |
|---------|-------------|
| `start_trace_run` | Begin a trace run |
| `end_trace_run` | End a trace run |
| `start_trace_span` | Begin a span within a run |
| `end_trace_span` | End a span |
| `get_trace_metrics` | Get trace statistics |

#### Knowledge Graph

| Command | Description |
|---------|-------------|
| `add_graph_node` | Add a node with type and properties |
| `add_graph_edge` | Create a relationship between nodes |
| `find_graph_path` | Find paths between nodes |
| `get_graph_subgraph` | Extract a subgraph |
| `query_graph` | Run a graph query |

#### Checkpoints

| Command | Description |
|---------|-------------|
| `create_checkpoint` | Create a state checkpoint |
| `save_checkpoint` | Persist checkpoint data |
| `load_checkpoint` | Restore from checkpoint |
| `list_checkpoints` | List available checkpoints |
| `delete_checkpoint` | Remove a checkpoint |

---

## Gateway HTTP API

The gateway serves a REST API at `http://127.0.0.1:18789`.

### Public API (`/api/v1/`)

#### `POST /api/v1/chat`

Send a message.

```json
// Request
{
  "agent_id": "agent-uuid",
  "content": "Hello!",
  "chat_id": "optional-chat-id"
}

// Response
{
  "message": { "role": "assistant", "content": "Hi there!" },
  "chat_id": "chat-uuid",
  "trace": [...]
}
```

#### `GET /api/v1/agents`

List all agents.

#### `GET /api/v1/models`

List available models.

#### `GET /api/v1/health`

Health check.

```json
{
  "status": "healthy",
  "version": "0.1.0",
  "uptime_seconds": 3600,
  "components": {
    "database": "ok",
    "providers": "ok",
    "memory": "ok"
  }
}
```

### OpenAI-Compatible API

ClawDesk exposes an OpenAI-compatible API for drop-in replacement:

#### `POST /v1/chat/completions`

```json
// Request (OpenAI format)
{
  "model": "claude-sonnet-4-20250514",
  "messages": [
    { "role": "system", "content": "You are helpful." },
    { "role": "user", "content": "Hello!" }
  ],
  "stream": true,
  "temperature": 0.7
}

// Response (streaming SSE)
data: {"choices":[{"delta":{"content":"Hi"},"index":0}]}
data: {"choices":[{"delta":{"content":" there!"},"index":0}]}
data: [DONE]
```

#### `GET /v1/models`

List models in OpenAI format.

```json
{
  "data": [
    { "id": "claude-sonnet-4-20250514", "object": "model" },
    { "id": "gpt-4o", "object": "model" }
  ]
}
```

### Responses API

#### `POST /v1/responses`

OpenAI Responses API format.

### WebSocket API

#### `ws://127.0.0.1:18789/ws`

Real-time bidirectional communication:

```json
// Client → Server
{ "type": "subscribe", "agent_id": "agent-uuid" }
{ "type": "message", "agent_id": "agent-uuid", "content": "Hello" }
{ "type": "cancel" }

// Server → Client
{ "type": "stream_chunk", "text": "Hi", "done": false }
{ "type": "stream_chunk", "text": " there!", "done": true }
{ "type": "tool_start", "name": "web_search", "args": "..." }
{ "type": "tool_end", "name": "web_search", "success": true }
{ "type": "error", "message": "..." }
```

### Admin Routes

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/api/v1/admin/plugins` | GET | List plugins |
| `/api/v1/admin/plugins/{id}/reload` | POST | Reload a plugin |
| `/api/v1/admin/cron` | GET/POST | List/create cron jobs |
| `/api/v1/admin/cron/{id}/trigger` | POST | Manually trigger a job |
| `/api/v1/admin/skills` | GET | List skills |
| `/api/v1/admin/channels` | GET | List channels |
| `/api/v1/admin/metrics` | GET | System metrics |

### Agent-to-Agent (A2A) Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/.well-known/agent.json` | GET | Agent Card (capability advertisement) |
| `/api/v1/a2a/tasks` | POST | Submit a task to an agent |
| `/api/v1/a2a/tasks/{id}` | GET | Get task status |
| `/api/v1/a2a/tasks/{id}/cancel` | POST | Cancel a task |

### Rate Limiting

The gateway enforces rate limits:
- Default: 10 requests/second with burst of 50
- Configurable per route
- Returns `429 Too Many Requests` with `Retry-After` header

### CORS

CORS is configured to allow the Tauri WebView origin:
- `http://localhost:1420` (dev)
- `tauri://localhost` (production)

## Event Types

Events emitted via broadcasting and available through WebSocket:

```typescript
// Tauri event: "agent-event"
interface AgentEventPayload {
  agent_id: string;
  event: {
    type: string;  // One of the event types below
    // ... event-specific fields
  };
}

// Tauri event: "incoming:message" 
interface IncomingMessage {
  agent_id: string;
  chat_id: string;
  preview: string;
  timestamp: string;
  cache_hit?: boolean;
}

// Tauri event: "system:alert"
interface SystemAlert {
  level: "info" | "warn" | "error";
  title: string;
  message: string;
}
```

## TypeScript Types

The frontend uses typed interfaces matching Rust Serialize structs. Key types in `src/types.ts`:

```typescript
interface Agent {
  id: string;
  name: string;
  model: string;
  persona: string;
  skills: string[];
  tokenBudget: number;
}

interface ChatMessage {
  id: string;
  role: "user" | "assistant" | "system";
  content: string;
  timestamp: string;
  metadata?: ChatMessageMeta;
}

interface ChatMessageMeta {
  skillsActivated: string[];
  tokenCost: number;
  costUsd: number;
  model: string;
  durationMs: number;
  identityVerified: boolean;
  toolsUsed: ToolUsageSummary[];
  compaction?: CompactionInfo;
}
```
