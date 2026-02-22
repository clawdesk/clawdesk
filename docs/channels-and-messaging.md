# Channels & Messaging

ClawDesk connects to 25+ messaging platforms through a layered channel trait system. Each channel adapter handles platform-specific formatting, threading, media, and rate limiting.

## Channel Architecture

```
┌─────────────────────────────────────────────────────┐
│                   Channel Layer                      │
│                                                      │
│  ┌──────────────────────────────────────────────┐   │
│  │            Channel Trait (Layer 0)            │   │
│  │  id() · meta() · start() · send() · stop()   │   │
│  └──────────────────────────────────────────────┘   │
│       │           │            │           │         │
│       ▼           ▼            ▼           ▼         │
│  ┌─────────┐ ┌──────────┐ ┌──────────┐ ┌────────┐  │
│  │Threaded │ │Streaming │ │Reactions │ │ Group  │  │
│  │(Layer 1)│ │(Layer 1) │ │(Layer 1) │ │Mgmt L1│  │
│  └─────────┘ └──────────┘ └──────────┘ └────────┘  │
│       │           │            │                     │
│       └───────────┴────────────┘                     │
│                   │                                  │
│                   ▼                                  │
│          ┌──────────────┐                            │
│          │ RichChannel  │  (auto-implemented)        │
│          │ = Channel +  │                            │
│          │  Threaded +  │                            │
│          │  Streaming + │                            │
│          │  Reactions   │                            │
│          └──────────────┘                            │
│                                                      │
│  ┌──────────────────────────────────────────────┐   │
│  │           Supporting Infrastructure           │   │
│  │  ChannelDock · ChannelBridge · RateLimit ·    │   │
│  │  ReplyFormatter · Health · InboundAdapter     │   │
│  └──────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────┘
```

## Channel Trait

The base `Channel` trait is the minimum required interface:

```rust
#[async_trait]
pub trait Channel: Send + Sync {
    /// Unique identifier for this channel instance
    fn id(&self) -> &ChannelId;
    
    /// Channel capabilities and metadata
    fn meta(&self) -> &ChannelMeta;
    
    /// Start receiving messages (connect + listen)
    async fn start(&self, sink: Arc<dyn MessageSink>) -> Result<(), ChannelError>;
    
    /// Send a message to the channel
    async fn send(&self, msg: OutboundMessage) -> Result<(), ChannelError>;
    
    /// Stop and disconnect
    async fn stop(&self) -> Result<(), ChannelError>;
}
```

### Optional Capabilities (Layer 1)

| Trait | Methods | Description |
|-------|---------|-------------|
| `Threaded` | `reply_to(msg_id, content)`, `get_thread(msg_id)` | Thread-based replies |
| `Streaming` | `start_stream(msg_id)` → `StreamHandle` | Progressive message updates |
| `Reactions` | `add_reaction(msg_id, emoji)`, `remove_reaction(...)` | Emoji reactions |
| `GroupManagement` | `create_group()`, `add_member()`, `remove_member()` | Group chat management |
| `Directory` | `search(query)`, `list_contacts()` | Contact directory |
| `Pairing` | `init_pairing()`, `accept_pairing()` | Device/account pairing |

### RichChannel (Layer 2)

`RichChannel` is automatically implemented for any type that implements `Channel + Threaded + Streaming + Reactions`. No manual implementation needed.

## Channel Metadata

```rust
pub struct ChannelMeta {
    pub display_name: String,
    pub supports_threading: bool,
    pub supports_streaming: bool,
    pub supports_reactions: bool,
    pub supports_media: bool,
    pub supports_groups: bool,
    pub max_message_length: usize,
}
```

## Supported Channels

| Channel | Threading | Streaming | Reactions | Media | Max Length |
|---------|-----------|-----------|-----------|-------|-----------|
| **Slack** | ✅ | ✅ | ✅ | ✅ | 4,000 |
| **Discord** | ✅ | ✅ | ✅ | ✅ | 2,000 |
| **Telegram** | ✅ | ✅ | ✅ | ✅ | 4,096 |
| **WhatsApp** | ✅ | ❌ | ✅ | ✅ | 4,096 |
| **Signal** | ❌ | ❌ | ✅ | ✅ | 4,096 |
| **Matrix** | ✅ | ❌ | ✅ | ✅ | 65,536 |
| **iMessage** | ❌ | ❌ | ✅ | ✅ | 20,000 |
| **MS Teams** | ✅ | ✅ | ✅ | ✅ | 28,000 |
| **Google Chat** | ✅ | ❌ | ❌ | ✅ | 4,096 |
| **IRC** | ❌ | ❌ | ❌ | ❌ | 512 |
| **Email** | ✅ | ❌ | ❌ | ✅ | 100,000 |
| **Nostr** | ✅ | ❌ | ✅ | ✅ | 65,535 |
| **Twitch** | ❌ | ❌ | ❌ | ❌ | 500 |
| **Line** | ✅ | ❌ | ✅ | ✅ | 5,000 |
| **Feishu/Lark** | ✅ | ❌ | ✅ | ✅ | 4,096 |
| **Mattermost** | ✅ | ❌ | ✅ | ✅ | 16,383 |
| **Nextcloud Talk** | ✅ | ❌ | ✅ | ✅ | 32,000 |
| **Zalo** | ❌ | ❌ | ✅ | ✅ | 2,000 |
| **Webchat** | ✅ | ✅ | ✅ | ✅ | 100,000 |
| **Internal** | ✅ | ✅ | ✅ | ✅ | 100,000 |

## Channel Dock

`ChannelDock` is a registry mapping channel IDs to their configuration:

```rust
let dock = ChannelDock::with_all_defaults(); // Pre-populated with 23+ channels

// Look up channel capabilities
let entry = dock.get("slack")?;
println!("Max length: {}", entry.capabilities.max_message_length);
println!("Threading: {}", entry.capabilities.supports_threading);

// Convert to runner context for prompt injection
let ctx = dock.to_runner_context("slack")?;
// → ChannelContext injected into AgentRunner system prompt
```

### DockEntry

```rust
pub struct DockEntry {
    pub channel_id: ChannelId,
    pub capabilities: ChannelCapabilities,
    pub markup_format: MarkupFormat,  // Markdown, HTML, mrkdwn, plain
    pub is_active: bool,
}
```

## Message Flow

### Inbound (Receiving)

```
Platform API (webhook/websocket/polling)
    │
    ▼
Channel Adapter (e.g., SlackChannel)
    │
    ▼
InboundAdapter (normalization)
    │  - Extract sender identity
    │  - Normalize message format
    │  - Attach media references
    │  - Map to InboundMessage
    │
    ▼
MessageSink callback
    │
    ▼
Event Bus (clawdesk-bus)
    │
    ▼
Auto-Reply Pipeline (clawdesk-autoreply)
    │  1. TriggerClassifier — should agent respond?
    │  2. EchoSuppressor — prevent echo loops
    │  3. MessageRouter — which agent handles this?
    │  4. Agent execution
    │  5. ResponseFormatter — format for channel
    │
    ▼
Channel.send(outbound_message)
```

### Outbound (Sending)

```
AgentResponse
    │
    ▼
ResponseSegment chunking (by channel max_length)
    │  - Paragraph boundary splitting
    │  - Media attachment on first segment
    │  - Thread reply_to on first segment
    │  - Error flag propagation
    │
    ▼
ReplyFormatter
    │  - Apply channel-specific markup
    │  - Deduplicate against messaging_sends
    │
    ▼
Rate Limiter (token bucket per channel)
    │
    ▼
Channel.send()
```

## Auto-Reply Engine

`clawdesk-autoreply` provides the full auto-reply pipeline:

### Pipeline Stages

| Stage | Component | Purpose |
|-------|-----------|---------|
| 1. Classify | `TriggerClassifier` | Determine if the message needs a response |
| 2. Debounce | `Debouncer` | Prevent rapid-fire responses |
| 3. Echo Suppress | `EchoSuppressor` | Prevent the bot from replying to itself |
| 4. Route | `MessageRouter` | Select the appropriate agent |
| 5. Execute | Agent pipeline | Run the agent with the message |
| 6. Format | `ResponseFormatter` | Format response for the channel |
| 7. Chunk | `Chunker` | Split long responses for channel limits |
| 8. Deliver | Channel adapter | Send via the channel |

### Echo Suppression

`EchoSuppressor` prevents infinite loops when the bot's own messages trigger responses:
- Tracks recently sent message IDs
- Checks sender identity against bot identity
- Configurable suppression window

### Response Formatting

`ResponseFormatter` adapts output for each channel's markup format:

| Format | Channels | Syntax |
|--------|----------|--------|
| **Markdown** | Matrix, Webchat | Standard `**bold**`, `*italic*`, `` `code` `` |
| **mrkdwn** | Slack | `*bold*`, `_italic_`, `` `code` `` |
| **HTML** | Email, Teams | `<b>bold</b>`, `<i>italic</i>` |
| **Plain** | IRC, Twitch, SMS | No formatting, text only |

## Rate Limiting

Each channel has a token-bucket rate limiter:

```rust
pub struct RateLimiter {
    tokens: AtomicU64,
    max_tokens: u64,
    refill_rate: f64,    // tokens per second
    last_refill: Instant,
}
```

Default limits vary by platform:
- **Slack**: 1 message/second
- **Discord**: 5 messages/5 seconds
- **Telegram**: 30 messages/second (global), 1/second per chat
- **IRC**: 1 message/2 seconds

## Channel Health

`ChannelHealth` monitors channel liveness:

| Metric | Description |
|--------|-------------|
| `latency_ms` | Average send latency |
| `error_rate` | Recent error percentage |
| `last_heartbeat` | Last successful communication |
| `status` | `Healthy`, `Degraded`, or `Down` |

## Channel Bridge

`ChannelBridge` connects the agent's `message_send` tool to actual channel delivery:
- Maps tool call `to` + `channel` to a `ChannelId`
- Resolves the channel adapter
- Sends via `channel.send()`
- Records the send in `MessagingToolTracker`

## Threading

For channels that support threading, ClawDesk maintains thread context:

```rust
pub struct ThreadStore {
    // SochDB-backed thread persistence
    // Schema: threads/{id}, msgs/{thread_id}/{timestamp}/{msg_id}
}
```

### Thread Operations

| Command | Description |
|---------|-------------|
| `create_thread` | Create a new thread |
| `get_thread` | Get thread metadata |
| `append_message` | Add a message to a thread |
| `get_messages` | Get messages in a thread |
| `get_message_range` | Get messages in a time range |
| `delete_thread` | Delete a thread and all messages |
| `thread_stats` | Get thread statistics |

## Channel Registration

Channels are registered at startup in `AppState`:

```rust
let mut registry = ChannelRegistry::new();

// Register built-in webchat (always available)
registry.register(webchat_channel)?;

// Register platform channels based on config
if let Some(slack_config) = config.slack {
    registry.register(SlackChannel::new(slack_config))?;
}
```

### Channel Commands

| Command | Description |
|---------|-------------|
| `list_channels` | List all registered channels with status |
| `get_channel_status` | Get detailed channel status + health |

## Event Bus Integration

`clawdesk-bus` provides the reactive event bus connecting channels to agents:

### Priority Classes

| Priority | Weight | Use Case |
|----------|--------|----------|
| `Urgent` | 8 | Direct messages, mentions |
| `Standard` | 4 | Regular channel messages |
| `Batch` | 1 | Background indexing, digests |

### Features

- **Weighted Fair Queuing** — Higher-priority messages are processed first
- **Backpressure** — Bounded channels prevent memory exhaustion
- **Crash Recovery** — Events are journaled to SochDB for replay
- **Topic Subscriptions** — Components subscribe to specific event types

## Markdown Cross-Platform

`clawdesk-channels` includes a `markdown` module for cross-platform formatting:

```rust
// Convert standard markdown to platform-specific format
let slack_text = markdown::convert(text, MarkupFormat::Mrkdwn);
let html_text = markdown::convert(text, MarkupFormat::Html);
let plain_text = markdown::convert(text, MarkupFormat::Plain);
```

Handles:
- Bold, italic, strikethrough
- Code blocks and inline code
- Links and images
- Lists (ordered and unordered)
- Block quotes
- Headers (flattened for platforms without support)

## Retry Policy

Each channel adapter has a configurable retry policy:

```rust
pub struct RetryConfig {
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub backoff_factor: f64,
    pub jitter: bool,
}
```

Platform-specific defaults (`telegram_retry_config()`, `discord_retry_config()`, etc.) are pre-configured with appropriate rate-limit awareness.
