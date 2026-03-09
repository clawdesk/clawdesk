# Getting Started

This guide walks you through building, running, and configuring ClawDesk for the first time.

## Prerequisites

- **Rust** 1.75+ with `cargo`
- **Node.js** 18+ with `pnpm`
- **Tauri CLI** — `cargo install tauri-cli`
- **macOS** (primary target), Linux, or Windows

### Optional

- **Ollama** — For local model inference
- An API key for at least one LLM provider (Anthropic, OpenAI, Google, etc.)

## Building

### Desktop App (Tauri)

```bash
cd clawdesk

# Install frontend dependencies
cd crates/ui && pnpm install && cd ../..

# Build and run in development mode
./run-tauri.sh

# Or manually:
cargo tauri dev
```

### Gateway Server (Standalone)

```bash
cargo run -p clawdesk-cli -- gateway run
# Default: http://127.0.0.1:18789
```

### CLI

```bash
cargo run -p clawdesk-cli -- --help
```

### tmux Desktop

For the full Tauri-like experience in the terminal with 10 screens (Dashboard, Chat, Agents, Skills, etc.):

```bash
# First-time: guided onboarding + auto-launch
clawdesk tmux setup

# Quick launch the desktop layout (10 windows)
clawdesk tmux launch

# Or a simpler preset
clawdesk tmux launch --layout workspace   # 4-pane dev layout
clawdesk tmux launch --layout chat         # Focused chat
```

Navigate between screens with `Ctrl-B + 0..9`. See the full [tmux Desktop Guide](tmux-workspace.md) for details.

### Tests

```bash
# Run all tests
cargo test --workspace

# Run tests for a specific crate
cargo test -p clawdesk-agents
cargo test -p clawdesk-security
cargo test -p clawdesk-memory
```

## First Run

### 1. Set Up a Provider

On first launch, ClawDesk needs at least one LLM provider configured. Open **Settings** from the sidebar and configure a provider:

| Provider | What You Need |
|----------|---------------|
| **Anthropic** | API key from console.anthropic.com |
| **OpenAI** | API key from platform.openai.com |
| **Google Gemini** | API key from aistudio.google.com |
| **Ollama** | Install Ollama locally, no key needed |
| **Azure OpenAI** | Endpoint URL + API key |
| **AWS Bedrock** | AWS credentials configured |
| **Cohere** | API key from dashboard.cohere.com |

You can configure the provider directly in the Settings page, or set environment variables:

```bash
export ANTHROPIC_API_KEY="sk-ant-..."
export OPENAI_API_KEY="sk-..."
export GOOGLE_API_KEY="..."
```

### 2. Create an Agent

Navigate to the **Overview** page and click **Create Agent**. Configure:

- **Name** — Display name for the agent
- **Model** — Select from available models (e.g., `claude-sonnet-4-20250514`)
- **Persona** — System prompt defining the agent's behavior
- **Skills** — Select which skills to activate (see [Skills & Plugins](skills-and-plugins.md))
- **Token Budget** — Maximum context window size (default: 128,000)

### 3. Start Chatting

Click on your agent to open a chat. Type a message and press Enter. You'll see:

- **Real-time streaming** — Tokens appear as they're generated
- **Tool usage indicators** — When the agent uses tools, you'll see trace entries
- **Skill activation** — Active skills are shown in the message metadata

### 4. Explore Features

- **Trace Viewer** — Click the trace icon on any message to see the full execution trace
- **Skills Page** — Browse, activate, and manage skills
- **Automations** — Set up cron-triggered agent tasks
- **Canvas** — Create structured documents with AI assistance
- **Memory** — The agent remembers past conversations via the memory system
- **tmux Workspace** — Launch `clawdesk tmux setup` for a multi-pane terminal development environment
- **Terminal UI** — Run `clawdesk tui` for a ratatui-based dashboard with Vim keybindings
- **Security Audit** — Run `clawdesk security audit --deep` to scan for security issues
- **Shell Completions** — Generate with `clawdesk completions bash` (or zsh, fish, powershell)

## Directory Structure

ClawDesk stores its data in the following locations:

| Location | Content |
|----------|---------|
| `~/.clawdesk/` | Main data directory |
| `~/.clawdesk/sochdb/` | SochDB database files |
| `~/.clawdesk/skills/` | User-installed skills |
| `~/.clawdesk/plugins/` | User-installed plugins |
| `~/.clawdesk/backups/` | Encrypted backups |
| `~/.clawdesk/logs/` | Rotating log files |

## Environment Variables

| Variable | Purpose | Default |
|----------|---------|---------|
| `CLAWDESK_DATA_DIR` | Override data directory | `~/.clawdesk` |
| `CLAWDESK_GATEWAY_PORT` | Gateway server port | `18789` |
| `CLAWDESK_GATEWAY_HOST` | Gateway bind address | `127.0.0.1` |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | OpenTelemetry collector endpoint | disabled |
| `CLAWDESK_LOG_LEVEL` | Log verbosity | `info` |
| `ANTHROPIC_API_KEY` | Anthropic API key | — |
| `OPENAI_API_KEY` | OpenAI API key | — |
| `GOOGLE_API_KEY` | Google API key | — |
| `OLLAMA_HOST` | Ollama server URL | `http://localhost:11434` |

## Verifying Your Setup

Run the built-in diagnostics:

```bash
cargo run -p clawdesk-cli -- doctor
```

This checks:
- Rust toolchain version
- Database connectivity and integrity
- Provider API key validity
- Network connectivity
- Disk space availability

## Next Steps

- [Agent System](agent-system.md) — Learn how the agent runner works
- [Skills & Plugins](skills-and-plugins.md) — Extend agent capabilities
- [Channels & Messaging](channels-and-messaging.md) — Connect to Slack, Discord, Telegram, etc.
- [Security & Safety](security-and-safety.md) — Understand the security model
- [Configuration Guide](configuration.md) — Advanced configuration options
