# Configuration Guide

This guide covers all configuration options for ClawDesk, from provider setup to advanced agent tuning.

## Provider Configuration

### Setting Up Providers

Providers can be configured via environment variables, the Settings UI, or per-request overrides.

#### Anthropic (Claude)

```bash
export ANTHROPIC_API_KEY="sk-ant-api03-..."
```

Available models:
| Model | Context | Best For |
|-------|---------|----------|
| `claude-opus-4-20250514` | 200K | Complex reasoning, analysis |
| `claude-sonnet-4-20250514` | 200K | Balanced quality and speed |
| `claude-haiku-3-20250307` | 200K | Fast, cost-effective |

#### OpenAI

```bash
export OPENAI_API_KEY="sk-..."
export OPENAI_BASE_URL="https://api.openai.com/v1"  # Optional custom endpoint
```

Available models: `gpt-4o`, `gpt-4o-mini`, `gpt-4-turbo`, `o1`, `o1-mini`, `o3-mini`

#### Google Gemini

```bash
export GOOGLE_API_KEY="..."
```

Available models: `gemini-2.0-flash`, `gemini-1.5-pro`, `gemini-1.5-flash`

**Note**: Gemini requires strict user/assistant message alternation. ClawDesk handles this automatically via `ProviderQuirks.require_alternation`.

#### Ollama (Local)

```bash
export OLLAMA_HOST="http://localhost:11434"  # Default
```

No API key needed. Install Ollama and pull models:
```bash
ollama pull llama3.1
ollama pull codellama
ollama pull mistral
```

#### Azure OpenAI

```bash
export AZURE_OPENAI_KEY="..."
export AZURE_OPENAI_ENDPOINT="https://your-resource.openai.azure.com"
export AZURE_OPENAI_DEPLOYMENT="your-deployment-name"
```

#### AWS Bedrock

Uses standard AWS credential chain (`AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_REGION`).

#### Cohere

```bash
export COHERE_API_KEY="..."
```

#### Google Vertex AI

Uses Google Cloud Application Default Credentials.

### Provider Negotiation

`ProviderNegotiator` automatically routes requests based on capabilities:

```rust
let required = ProviderCaps::TEXT_COMPLETION | ProviderCaps::SYSTEM_PROMPT;
let (provider, model) = negotiator.resolve_model("claude-sonnet-4-20250514", required)?;
```

Capabilities checked:
- `TEXT_COMPLETION` — Can generate text
- `SYSTEM_PROMPT` — Supports system prompts
- `TOOL_CALLING` — Supports function calling
- `STREAMING` — Supports streaming responses
- `VISION` — Supports image inputs

### Per-Request Override

The UI allows per-message provider override:
```json
{
  "provider_override": "Anthropic",
  "api_key": "sk-ant-...",
  "base_url": null
}
```

## Agent Configuration

### Creating an Agent

Agents are configured with these properties:

```json
{
  "id": "auto-generated-uuid",
  "name": "My Assistant",
  "model": "claude-sonnet-4-20250514",
  "persona": "You are a helpful coding assistant...",
  "skills": ["code-review", "debugger"],
  "token_budget": 128000
}
```

### Token Budget

The token budget controls the total context window:

```
Total Budget: 128,000 tokens
    ├── Response Reserve: 8,192 tokens (always reserved)
    ├── Identity (persona): up to 2,000 tokens
    ├── Skills: up to 20% (25,600 tokens)
    ├── Memory: up to 4,096 tokens
    ├── Bootstrap: up to 25% (32,000 tokens)
    ├── Runtime context: up to 512 tokens
    ├── Safety instructions: up to 1,024 tokens
    └── Remaining: conversation history
```

### Persona Writing

The persona is the system prompt that defines agent behavior. Tips:

```markdown
You are an expert Rust developer working on the ClawDesk project.

## Core Behaviors
- Always explain your reasoning before providing code
- Prefer safe Rust patterns (no unwrap in production code)
- Write comprehensive tests for all new functionality

## Communication Style
- Be concise and direct
- Use code examples to illustrate points
- Format responses in markdown

## Constraints
- Never modify files outside the workspace
- Ask for clarification when requirements are ambiguous
- Limit responses to 500 words unless explicitly asked for more
```

### PromptBuilder Budget Allocation

`PromptBuilder` controls how the prompt budget is allocated:

| Section | Cap | Priority | Description |
|---------|-----|----------|-------------|
| Identity | 2,000 | Highest | Agent persona |
| Safety | 1,024 | High | Safety instructions |
| Skills | 4,096 | Medium | Activated skill fragments |
| Memory | 4,096 | Medium | Recalled memory fragments |
| Runtime | 512 | Low | Date/time, channel, model info |
| History | 2,000 floor | — | Minimum conversation retained |

## Context Guard

The context guard prevents context window overflow:

```rust
ContextGuardConfig {
    context_limit: 128_000,
    trigger_threshold: 0.80,        // Start compacting at 80%
    response_reserve: 8_192,
    circuit_breaker_threshold: 3,   // Open breaker after 3 failures
    circuit_breaker_cooldown: 60s,
    adaptive_thresholds: true,      // Adjust based on usage patterns
    force_truncate_retain_share: 0.50,
}
```

### Compaction Behavior

| Fill Level | Action |
|-----------|--------|
| < 80% | No action |
| 80-95% | Tiered compaction (drop metadata → summarize → truncate) |
| 95-100% | Budget-based force truncation (keep newest 50%) |
| After 3 failures | Circuit breaker opens — emergency truncation |

## Failover Configuration

Configure multi-stage failover for resilience:

```rust
FailoverConfig {
    profiles: vec![
        FailoverProfile { provider: "anthropic", model: "claude-sonnet-4-20250514" },
        FailoverProfile { provider: "openai", model: "gpt-4o" },
        FailoverProfile { provider: "ollama", model: "llama3.1" },
    ],
    max_retries_per_stage: 2,
    backoff_base_ms: 1000,
    backoff_max_ms: 30000,
    thinking_levels: vec!["high", "medium", "low"],
}
```

Failover progression:
```
Profile 1 (Anthropic) → Profile 2 (OpenAI) → Profile 3 (Ollama)
    each with: attempt → backoff → retry
    then: reduce thinking level
    finally: exhausted → error
```

## Tool Policy

Control which tools agents can use:

```rust
ToolPolicy {
    allowed_tools: vec![],          // Empty = allow all
    denied_tools: vec!["shell_exec"],
    require_approval: vec!["file_write", "web_fetch"],
    max_concurrent: 5,
    timeout_ms: 30_000,
}
```

## Workspace Configuration

Set up workspace confinement for file tools:

```json
{
  "workspace_path": "/Users/me/projects/my-project"
}
```

### Bootstrap Config

Control which project files are auto-discovered:

```rust
BootstrapConfig {
    enabled: true,
    patterns: vec![
        "CLAUDE.md", "AGENTS.md",
        "README.md", "README.rst",
        "Cargo.toml", "package.json",
        "pyproject.toml", "go.mod",
    ],
    max_file_size: 50_000,       // Skip files > 50KB
    max_total_chars: 100_000,    // Total bootstrap limit
    max_files: 20,               // Max files to include
}
```

## Memory Configuration

```rust
MemoryConfig {
    // Embedding provider
    embedding_provider: "openai",
    embedding_model: "text-embedding-3-small",
    
    // Chunking
    chunk_size: 512,
    chunk_overlap: 64,
    
    // Search
    search_strategy: SearchStrategy::Hybrid(0.7),
    max_results: 10,
    
    // Diversity
    mmr_lambda: 0.5,
    
    // Recency
    temporal_decay_half_life: Duration::from_secs(7 * 24 * 3600),
    
    // Caching
    cache_enabled: true,
    
    // Tiered provider
    tiered_config: TieredConfig {
        failure_threshold: 3,
        recovery_interval: Duration::from_secs(300),
    },
}
```

## Gateway Configuration

```rust
GatewayConfig {
    host: "127.0.0.1",     // Bind address
    port: 18789,            // Port
    cors_origins: vec!["http://localhost:1420"],
    max_request_size: 10_485_760,  // 10MB
    rate_limit: RateLimitConfig {
        requests_per_second: 10,
        burst_size: 50,
    },
}
```

## Observability Configuration

### OpenTelemetry

Enable OTEL tracing by setting the endpoint:

```bash
export OTEL_EXPORTER_OTLP_ENDPOINT="http://localhost:4317"
export OTEL_SERVICE_NAME="clawdesk"
export OTEL_TRACES_SAMPLER="always_on"      # or "traceidratio" with ratio
export OTEL_TRACES_SAMPLER_ARG="1.0"
```

ClawDesk follows the **GenAI semantic conventions** (v1.36) for LLM call tracing:
- `gen_ai.system` — Provider name
- `gen_ai.request.model` — Model requested
- `gen_ai.response.model` — Model used
- `gen_ai.usage.input_tokens` — Input token count
- `gen_ai.usage.output_tokens` — Output token count
- `gen_ai.request.temperature` — Temperature setting

### Logging

```bash
export CLAWDESK_LOG_LEVEL="info"  # trace, debug, info, warn, error
export RUST_LOG="clawdesk_agents=debug,clawdesk_providers=info"
```

Logs are JSON-formatted for structured parsing. Log rotation is managed by `RotatingFileWriter`.

## Cron Configuration

Set up scheduled agent tasks:

```rust
CronJob {
    name: "daily-digest",
    schedule: "0 9 * * *",      // Every day at 9 AM
    agent_id: "digest-agent",
    message: "Generate today's digest from recent conversations",
    enabled: true,
    overlap_policy: OverlapPolicy::Skip,  // Skip if previous run still active
}
```

## Tunnel Configuration

```rust
TunnelConfig {
    listen_port: 51820,          // UDP port for WireGuard
    private_key: auto_generated,
    nat_strategy: NatStrategy::Auto,  // STUN + UDP hole-punching
    peers: vec![],               // Added via pairing flow
}
```

## Notification Configuration

```rust
NotificationConfig {
    enabled: true,
    sound: true,
    badge: true,
    categories: vec![
        NotificationCategory::DirectMessage,
        NotificationCategory::Mention,
        NotificationCategory::SecurityAlert,
    ],
}
```

## Backup Configuration

```rust
BackupConfig {
    enabled: true,
    schedule: "0 2 * * *",      // Daily at 2 AM
    retention_days: 30,
    encrypt: true,
    backup_dir: "~/.clawdesk/backups",
    include_embeddings: false,   // Embeddings can be regenerated
}
```

## Environment Variable Reference

| Variable | Purpose | Default |
|----------|---------|---------|
| `CLAWDESK_DATA_DIR` | Data directory | `~/.clawdesk` |
| `CLAWDESK_GATEWAY_PORT` | Gateway port | `18789` |
| `CLAWDESK_GATEWAY_HOST` | Gateway host | `127.0.0.1` |
| `CLAWDESK_LOG_LEVEL` | Log level | `info` |
| `ANTHROPIC_API_KEY` | Anthropic key | — |
| `OPENAI_API_KEY` | OpenAI key | — |
| `GOOGLE_API_KEY` | Google key | — |
| `COHERE_API_KEY` | Cohere key | — |
| `OLLAMA_HOST` | Ollama URL | `http://localhost:11434` |
| `AZURE_OPENAI_KEY` | Azure key | — |
| `AZURE_OPENAI_ENDPOINT` | Azure endpoint | — |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | OTEL endpoint | disabled |
| `OTEL_SERVICE_NAME` | OTEL service name | `clawdesk` |
| `RUST_LOG` | Per-crate log levels | — |
