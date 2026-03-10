<h1 align="center">ClawDesk - Agent2OS</h1>

<p align="center">
  <strong>The private Agent2OS for real work. Chat with any AI model, connect your favorite messaging apps, and run agents across your desktop, terminal, cloud VM, Raspberry Pi, or anywhere Rust runs.</strong>
</p>

<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/badge/License-MIT-blue.svg" alt="MIT License" /></a>
  <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/Rust-1.75+-orange.svg?logo=rust" alt="Rust 1.75+" /></a>
  <a href="https://tauri.app/"><img src="https://img.shields.io/badge/Tauri-2.0-24C8D8.svg?logo=tauri&logoColor=white" alt="Tauri 2.0" /></a>
  <a href="https://github.com/sochdb/sochdb"><img src="https://img.shields.io/badge/Powered%20by-SochDB-6C3483.svg" alt="Powered by SochDB" /></a>
</p>

<p align="center">
  <a href="#quick-start">Quick Start</a> ·
  <a href="#architecture">Architecture</a> ·
  <a href="#tmux-desktop">tmux Desktop</a> ·
  <a href="#security-model">Security</a> ·
  <a href="#crate-structure">Crates</a> ·
  <a href="#development">Development</a> ·
  <a href="#contributing">Contributing</a>
</p>

> **⚠️ Active Development — Things Will Break**
>
> ClawDesk is under active development and is not yet stable. APIs, CLI commands, and data formats may change without notice. Expect rough edges, missing features, and occasional breakage. We're currently focused on testing and hardening the core runtime.
>
> **Contributors and testers are welcome!** If you hit a bug, please open an issue. If you want to help, check the open issues or submit a PR.

---

ClawDesk - Agent2OS is a personal AI workspace you can run on your own devices. It is built for people who want one place to chat with AI, organize work, automate repetitive tasks, and keep useful assistants available across desktop, terminal, and server environments.

Instead of switching between browser tabs, hosted tools, scripts, and disconnected bots, ClawDesk gives you a single home for AI-powered work. You can use it as a desktop app, keep it running in the background, connect it to the tools and channels you already use, or run it headless on a remote machine.

With ClawDesk, you can:

- **Talk to AI in a way that fits your workflow** from desktop, terminal, or command line
- **Automate busywork** such as repeated prompts, routines, checks, and operational tasks
- **Connect your messaging and work tools** so agents can meet you where work already happens
- **Keep more control over privacy and setup** by running it on your own machine or server
- **Use one system across laptop, cloud VM, Raspberry Pi, and Docker** instead of learning separate tools for each place
- **Run always-on assistants** for background jobs, monitoring, and long-running workflows

Runs on **macOS, Linux, Windows, cloud VMs (AWS/GCP/Azure/DO), Raspberry Pi, and any machine with Rust**.

In short: **ClawDesk is Agent2OS** — a private place to run AI assistants for real work, on your computer or infrastructure, in a way that stays close to your tools, your data, and your workflow.

Inspired by [OpenClaw](https://github.com/openclaw/openclaw), ClawDesk takes the same broad idea of a personal, always-available AI system and pushes it toward a native, local-first experience with fewer moving parts and stronger control over where it runs.

> **Project status:** ClawDesk is in active development. Expect rapid changes, rough edges, incomplete documentation in some areas, and occasional regressions as major features land. If you hit bugs, missing docs, confusing behavior, or platform-specific issues, please report them. Contributors, testers, and feedback from real-world usage are actively wanted.

## Goal

> **Simplify. Reduce friction. Ship one binary.**

ClawDesk exists to make powerful AI tooling feel more like a real product and less like a pile of parts. The goal is simple: give one system that can be easy enough for everyday use, but flexible enough for automation, remote machines, and serious long-running work.

- **Start from a desktop app when you want something simple**
- **Drop to the terminal when you want more control**
- **Run it in the background when you need always-on behavior**
- **Use the same core system across personal devices, servers, and edge hardware**
- **Avoid getting locked into a single model, a single interface, or a hosted service**

## Why ClawDesk

- **It stays close to how you already work.** Desktop when you want visuals, terminal when you want speed, background services when you want automation.
- **It keeps you in control.** You choose where it runs, which models it uses, and how your data flows.
- **It is built for more than chat.** ClawDesk is meant for ongoing tasks, workflows, tools, channels, and assistants that keep working after one prompt.
- **It scales from personal use to serious setups.** Start on a laptop, then move the same system to a server, VM, Docker host, or Raspberry Pi.
- **It does not trap you in one surface.** Use the GUI, CLI, tmux workspace, TUI, gateway, or daemon depending on the job.

## Features

ClawDesk is designed to be useful to both everyday users and technical operators. You can ignore the deeper internals at first and simply think of it as one system for AI chat, automation, connected tools, and long-running assistants.

| | |
|---|---|
| **Local-First Runtime** | Agents execute on your hardware — laptop, server, cloud VM, Raspberry Pi. No cloud dependency required. |
| **Security Hardened** | CascadeScanner (Aho-Corasick + Regex), SHA-256 audit chain, scoped tokens, identity contracts, plugin sandbox. |
| **Multi-Model Support** | Claude (Haiku/Sonnet/Opus), OpenAI, Gemini, Ollama, Azure, Bedrock, Cohere, Vertex — 8 providers via a single trait. |
| **25+ Channel Adapters** | Telegram, Discord, Slack, WhatsApp, Signal, Matrix, Email, IRC, Teams, iMessage, Mastodon, Nostr, Twitch, and more. |
| **Skill Registry** | 15+ built-in skills with hot-loading, trigger evaluation, token-budgeted knapsack selection, and per-skill ACLs. |
| **tmux Desktop** | Full 10-window terminal layout mirroring the Tauri desktop app — Dashboard, Chat, Agents, Skills, Security, and more. Navigate with `Ctrl-B + 0..9`. |
| **Terminal UI (TUI)** | Ratatui-based interactive dashboard with Vim keybindings, 10 screens, 4 themes, model picker, and 30fps event loop. |
| **Agent Pipelines** | Declarative TOML pipelines with DAG execution, gates, parallel branches, checkpointing, and dead-letter queue. |
| **Memory System** | Hybrid search (Vector + BM25 + RRF) with temporal decay, MMR deduplication, and batch ingestion pipeline. |
| **Browser Automation** | Chrome DevTools Protocol integration — navigate, click, type, screenshot, DOM intelligence. |
| **RAG Pipeline** | Document ingestion (PDF, text), semantic chunking, vector search retrieval. |
| **MCP Support** | Model Context Protocol client/server over JSON-RPC 2.0 with stdio and SSE transports. |
| **Local Models** | Hardware detection (CUDA/Metal/CPU), model database, llama-server lifecycle management. |
| **A2A Protocol** | Agent-to-Agent communication with capability discovery, task FSM, and bipartite routing. |
| **WireGuard Tunnel** | Peer-to-peer encrypted networking with invite-based device pairing. NAT traversal via STUN. |
| **Full Observability** | OpenTelemetry tracing + metrics, cost tracking, token budgeting, and tamper-evident audit logs. |
| **Self-Update** | Atomic binary self-update from GitHub with SHA-256 verification and rollback support. |
| **Encrypted Backups** | AES-256-GCM encrypted config backups with Argon2 key derivation. |
| **Desktop UI** | Tauri 2.0 + React frontend with 138+ IPC commands, system tray, markdown rendering, drag-drop. |

## Runs Anywhere

ClawDesk is not just a desktop app — it's a **multi-interface AI runtime** that deploys anywhere:

| Interface | Command | Best For |
|-----------|---------|----------|
| **Desktop App** (Tauri 2.0) | `cargo tauri dev` | Everyday GUI use on macOS/Linux/Windows |
| **tmux Workspace** (10 screens) | `clawdesk tmux launch` | Terminal power users, SSH, cloud VMs |
| **TUI Dashboard** (ratatui) | `clawdesk tui` | Interactive terminal with Vim keybindings |
| **CLI** (40+ commands) | `clawdesk agent msg "hello"` | Scripts, automation, cron jobs |
| **Gateway Server** (Axum) | `clawdesk gateway run` | REST API, webhook integrations |
| **Daemon** (systemd/launchd) | `clawdesk daemon run` | Always-on background service |
| **Docker** | `docker-compose up` | Containerized headless deployment |

### Supported Platforms

| Platform | Desktop | tmux/TUI/CLI | Gateway/Daemon | Docker |
|----------|---------|-------------|----------------|--------|
| **macOS** (Intel & Apple Silicon) | ✅ | ✅ | ✅ | ✅ |
| **Linux** (x86_64) | ✅ | ✅ | ✅ | ✅ |
| **Linux** (ARM64 / Raspberry Pi) | — | ✅ | ✅ | ✅ |
| **Windows** (10+) | ✅ | ✅ | ✅ | ✅ |
| **Cloud VMs** (AWS/GCP/Azure/DO) | — | ✅ | ✅ | ✅ |
| **Headless servers** | — | ✅ | ✅ | ✅ |

> **No display required.** The tmux workspace, TUI, CLI, gateway server, and daemon all run without a GUI — perfect for cloud VMs, Raspberry Pi, headless servers, and SSH sessions.

## Powered by SochDB

All persistent storage — agent state, sessions, audit logs, skill configs, and vector embeddings — is handled by [**SochDB**](https://github.com/sochdb/sochdb), an embedded ACID-compliant database written in Rust. SochDB provides:

- **Embedded & zero-config** — no external database process, no connection strings.
- **ACID transactions** — crash-safe writes for audit chains and session state.
- **Vector search** — built-in cosine similarity for memory/recall without external vector DBs.
- **Single-file storage** — one portable database file per workspace.

## Channels

Channels are messaging platform integrations that let your agent send and receive messages across different surfaces. ClawDesk implements 25+ channel adapters as Rust traits:

| Channel | Status | Description |
|---------|--------|-------------|
| **Telegram** | Supported | Bot API with group/DM routing |
| **Discord** | Supported | Bot with slash commands, DM pairing, guild routing |
| **Slack** | Supported | Bolt-equivalent with app/bot token auth |
| **WhatsApp** | Supported | Baileys-equivalent bridge |
| **Signal** | Supported | signal-cli bridge |
| **Matrix** | Supported | Matrix SDK client |
| **Email** | Supported | IMAP/SMTP with MIME parsing |
| **IRC** | Supported | Standard IRC client with SASL auth |
| **Microsoft Teams** | Supported | Bot Framework adapter |
| **iMessage** | Supported | AppleScript bridge (macOS) |
| **Mastodon** | Supported | ActivityPub-compatible API |
| **Nostr** | Supported | NIP-01 relay client |
| **Twitch** | Supported | IRC-based chat integration |
| **Line** | Supported | Messaging API |
| **Lark** | Supported | Open API |
| **Mattermost** | Supported | WebSocket + REST API |
| **Nextcloud Talk** | Supported | Signaling API |
| **Zalo** | Supported | Official API |
| **WebChat** | Built-in | Browser UI served from the gateway |
| **Internal** | Built-in | In-process test channel |
| **Google Chat** | Planned | Chat API integration |

Each channel implements the `Channel` trait (`clawdesk-channel` crate), providing a uniform interface for message routing, group handling, allowlists, and DM pairing — regardless of the underlying platform.

## Skills

Skills are modular capabilities that extend what an agent can do. ClawDesk's skill system is ported from [OpenClaw's skill platform](https://github.com/anthropics/openclaw) and reimplemented in Rust:

- **15 bundled skills** — ship with the app (file ops, shell, browser, memory, cron, etc.).
- **Hot-loading** — activate/deactivate skills at runtime without restarting.
- **Per-skill ACLs** — each skill declares required permissions; the security scanner enforces them.
- **Workspace skills** — drop a `SKILL.md` into your workspace to add custom skills.
- **Managed skills** — install community skill packs from registries.

## Architecture

```
┌──────────────────────────────────────────────────────────────┐
│                    ClawDesk Runtime                          │
├──────────────┬───────────────────────────────────────────────┤
│  React UI    │              Tauri IPC Bridge                 │
│  (TypeScript)│  138+ commands · typed invoke() wrappers      │
├──────────────┴───────────────────────────────────────────────┤
│                      Rust Backend                            │
│  ┌─────────────┬──────────────┬──────────────┬────────────┐  │
│  │ Skill       │ Provider     │ Security     │ Agent      │  │
│  │ Registry    │ Registry     │ Scanner      │ Runtime    │  │
│  ├─────────────┼──────────────┼──────────────┼────────────┤  │
│  │ Tool        │ Audit        │ Identity     │ Tunnel     │  │
│  │ Registry    │ Logger       │ Contracts    │ (WireGuard)│  │
│  └─────────────┴──────────────┴──────────────┴────────────┘  │
│                      SochDB Storage                          │
└──────────────────────────────────────────────────────────────┘
```

### How it works

The React frontend communicates with the Rust backend through **138+ typed IPC commands** over the Tauri bridge. The backend manages agent lifecycle, security scanning, skill orchestration, and model routing — all in-process, no sidecar daemons.

## Crate Structure

<details>
<summary><strong>All 40+ crates</strong> (click to expand)</summary>

### Core Layer
| Crate | Purpose |
|-------|---------|
| `clawdesk-types` | Shared types: errors, messages, sessions, config, tokenizer |
| `clawdesk-storage` | Storage trait ports: SessionStore, ConversationStore, VectorStore, GraphStore |
| `clawdesk-domain` | Pure business logic — context guard, compaction, routing, prompt building |
| `clawdesk-sochdb` | SochDB embedded ACID database: WAL, MVCC, HNSW vector search, checkpointing |

### Agent Engine
| Crate | Purpose |
|-------|---------|
| `clawdesk-agents` | Agent execution engine: AgentRunner, pipelines, failover, tool orchestration |
| `clawdesk-providers` | 8 LLM providers: Anthropic, OpenAI, Gemini, Ollama, Azure, Bedrock, Cohere, Vertex |
| `clawdesk-runtime` | Durable execution: checkpoints, activity journal, dead-letter queue, lease management |
| `clawdesk-skills` | Skill system: registry, trigger evaluation, token-budgeted knapsack, env injection |
| `clawdesk-plugin` | Plugin lifecycle: hooks, sandbox, dependency resolution, capability enforcement |

### Memory & Knowledge
| Crate | Purpose |
|-------|---------|
| `clawdesk-memory` | Embeddings, BM25, hybrid search (RRF), batch pipeline, temporal decay, MMR |
| `clawdesk-rag` | Document ingestion, PDF/text extraction, semantic chunking, vector retrieval |

### Communication
| Crate | Purpose |
|-------|---------|
| `clawdesk-channel` | Channel trait hierarchy: Channel → Threaded + Streaming + Reactions |
| `clawdesk-channels` | 25+ implementations: Slack, Discord, Telegram, WhatsApp, Signal, Matrix, IRC, etc. |
| `clawdesk-bus` | Event-sourced reactive bus with weighted fair queuing |
| `clawdesk-autoreply` | Auto-reply pipeline: classify → route → enrich → execute → format → deliver |
| `clawdesk-threads` | Namespaced chat-thread persistence on SochDB |
| `clawdesk-acp` | Agent-to-Agent protocol: agent cards, task FSM, capability discovery |

### Security
| Crate | Purpose |
|-------|---------|
| `clawdesk-security` | Audit logging (hash-chained), CascadeScanner, ACL, OAuth2 + PKCE, credential vault |
| `clawdesk-sandbox` | Multi-modal isolation: Docker, subprocess, workspace confinement |

### Networking & Discovery
| Crate | Purpose |
|-------|---------|
| `clawdesk-gateway` | Axum HTTP/WS: REST + OpenAI-compatible + admin + A2A routes |
| `clawdesk-tunnel` | WireGuard P2P encrypted networking, NAT traversal via STUN |
| `clawdesk-discovery` | mDNS service advertisement + SPAKE2 password-authenticated pairing |
| `clawdesk-mcp` | Model Context Protocol: JSON-RPC 2.0 client/server, stdio & SSE |

### Advanced Engine
| Crate | Purpose |
|-------|---------|
| `clawdesk-consensus` | Byzantine PBFT for multi-agent voting |
| `clawdesk-planner` | Dynamic task graph (DTGG) with HEFT scheduling |
| `clawdesk-canvas` | Canvas host + A2UI protocol for agent-generated UI |
| `clawdesk-browser` | Browser automation via Chrome DevTools Protocol |
| `clawdesk-media` | Audio transcription, image analysis, TTS, link previews |
| `clawdesk-local-models` | Local LLM management: hardware detection, llama-server lifecycle |
| `clawdesk-simd` | SIMD kernels: cosine similarity (AVX2/NEON), dot product |

### Infrastructure
| Crate | Purpose |
|-------|---------|
| `clawdesk-infra` | Backup (AES-256), clipboard, daemon, dispatch queue, git-sync, TLS |
| `clawdesk-cron` | Cron scheduling with overlap prevention and heartbeat monitoring |
| `clawdesk-daemon` | Platform-native service management (launchd/systemd/Windows) |
| `clawdesk-adapters` | External service adapter: OAuth lifecycle, rate limiting, circuit breaker |
| `clawdesk-extensions` | Integration registry, credential vault, health monitoring |
| `clawdesk-migrate` | Migration from OpenClaw (YAML agents, SQLite sessions) |

### Observability
| Crate | Purpose |
|-------|---------|
| `clawdesk-observability` | OpenTelemetry tracing + metrics, GenAI semantic conventions |
| `clawdesk-telemetry` | TracerProvider, MeterProvider, structured logging, OTLP export |

### Frontends
| Crate | Purpose |
|-------|---------|
| `clawdesk-tauri` | Tauri 2.0 desktop: 138+ IPC commands, AppState, system tray |
| `clawdesk-cli` | CLI: 40+ commands, tmux desktop (10-window), onboarding, agent REPL |
| `clawdesk-tui` | Ratatui TUI: 10 screens, Vim keys, 4 themes, session multiplexing |
| `ui` | React + TypeScript + Vite frontend, Tailwind CSS |

### Testing
| Crate | Purpose |
|-------|---------|
| `clawdesk-bench` | Benchmark harness: provider latency, throughput, cost tracking |
| `clawdesk-test` | YAML test cases with deterministic replay |

</details>

## Prerequisites

<details>
<summary><strong>macOS</strong></summary>

1. **Xcode Command Line Tools:** `xcode-select --install`
2. **Rust 1.75+:** `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
3. **Node.js 20+** and **pnpm:** `brew install node && npm install -g pnpm`
4. **Tauri CLI:** `cargo install tauri-cli`

</details>

<details>
<summary><strong>Linux</strong></summary>

1. **System packages (Debian/Ubuntu):**
   ```bash
   sudo apt install build-essential pkg-config libssl-dev libgtk-3-dev webkit2gtk-4.1
   ```
2. **Rust 1.75+:** `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
3. **Node.js 20+** and **pnpm:** install via your package manager or [nodejs.org](https://nodejs.org)
4. **Tauri CLI:** `cargo install tauri-cli`

</details>

<details>
<summary><strong>Windows</strong></summary>

1. **Visual Studio Build Tools** with the "Desktop development with C++" workload.
2. **Rust 1.75+:** [rustup.rs](https://rustup.rs)
3. **Node.js 20+** and **pnpm:** [nodejs.org](https://nodejs.org) + `npm install -g pnpm`
4. **Tauri CLI:** `cargo install tauri-cli`

</details>

## Quick Start

```bash
# Clone the repository
git clone https://github.com/clawdesk/clawdesk.git
cd clawdesk

# Install UI dependencies
cd crates/ui && pnpm install && cd ../..

# Run in development mode
cargo tauri dev

# Build for production
cargo tauri build
```

### CLI Quick Start

Prebuilt CLI binaries are currently published for macOS and Linux. Windows CLI binaries are coming soon.

```bash
# Build the CLI
cargo build -p clawdesk-cli

# Interactive first-time setup
clawdesk init

# tmux workspace (multi-pane terminal experience)
clawdesk tmux setup              # Guided onboarding + auto-launch
clawdesk tmux launch             # 4-pane workspace layout
clawdesk tmux launch -l chat     # Focused chat layout
clawdesk tmux launch -l monitor  # Ops monitoring layout

# Start the gateway
clawdesk gateway run

# Chat with an agent
clawdesk agent run
clawdesk agent msg "hello"

# Run diagnostics
clawdesk doctor
```

`cargo tauri dev` and `cargo tauri build` automatically prepare the bundled `llama-server` sidecar used by Local Models. A machine-level `llama-server` install is not required.

If you need to skip the auto-download in offline CI, set `CLAWDESK_SKIP_LLAMA_SERVER_DOWNLOAD=1` and provide the sidecar files under [crates/clawdesk-tauri/binaries](/Users/sushanth/llamabot/clawdesk/crates/clawdesk-tauri/binaries) yourself.

## CLI

ClawDesk ships a full-featured CLI with over 40 commands. Build it with `cargo build -p clawdesk-cli`.

<details>
<summary><strong>Full command tree</strong> (click to expand)</summary>

```
clawdesk
├── gateway run              Start the HTTP gateway server
├── message send <text>      Send a message to the agent
├── channels status          Show channel connectivity
├── plugins {list, reload, info}
├── cron {list, create, trigger, delete}
├── config {set, get, backup, restore}
├── agent
│   ├── message <text>       Send a one-shot message
│   ├── run                  Interactive REPL session (Claude Code equivalent)
│   ├── add <id>             Add agent from TOML or wizard
│   ├── validate             Validate all agent definitions
│   ├── list                 List agents with routing table
│   ├── apply                Hot-reload agent definitions
│   └── export <id>          Export agent to TOML
├── skill
│   ├── list / info          Browse installed skills
│   ├── search <query>       Search the skill registry
│   ├── install <name>       Install a skill pack
│   ├── uninstall <name>     Remove a skill
│   ├── create <name>        Scaffold a new skill
│   ├── lint / test          Validate skill definitions
│   ├── audit / check        Security audit for skills
│   └── publish              Publish to registry
├── tmux
│   ├── setup                Guided onboarding + tmux launch
│   ├── launch               Launch tmux session (default: desktop — 10 screens)
│   ├── list                 List active ClawDesk sessions
│   ├── attach <session>     Attach to a session
│   ├── kill <session>       Kill a session
│   ├── layouts              Show available layout presets
│   └── keys                 Show tmux key bindings cheat sheet
├── tui                      Launch the ratatui terminal UI
├── login                    Authenticate with provider APIs
├── doctor                   Run diagnostics
├── init                     Interactive first-time setup wizard
├── completions <shell>      Generate shell completions
├── security audit           Run security audit (8 checks)
├── daemon
│   ├── run / install / uninstall
│   ├── start / stop / restart
│   ├── status               PID, uptime, health
│   └── logs                 Tail daemon logs
└── update
    ├── check                Check for newer version
    ├── apply                Download and install update
    └── rollback             Rollback to previous version
```

</details>

## tmux Desktop

ClawDesk includes a built-in tmux session manager that mirrors the **Tauri desktop app** in the terminal. The default **desktop** layout creates 10 tmux windows — one for each screen in the Tauri app.

```bash
# First-time: guided onboarding → provider setup → layout selection → auto-launch
clawdesk tmux setup

# Quick launch the full desktop experience (10 screens)
clawdesk tmux launch

# Quick-start presets
clawdesk tmux launch --layout workspace    # 4-pane dev layout
clawdesk tmux launch --layout monitor      # 3-pane ops dashboard
clawdesk tmux launch --layout chat         # 2-pane focused chat
```

### Desktop Layout — 10 Screens

Navigate with `Ctrl-B + 0..9`, just like clicking sidebar items in the Tauri app:

| Key | Screen | Content |
|-----|--------|---------|
| `Ctrl-B + 0` | Dashboard | System health, providers, agent list, daemon status |
| `Ctrl-B + 1` | Chat | Agent REPL (interactive conversation) |
| `Ctrl-B + 2` | Sessions | Session list and detail/export |
| `Ctrl-B + 3` | Agents | Agent registry, management, team mode |
| `Ctrl-B + 4` | Channels | 25+ channel status and configuration |
| `Ctrl-B + 5` | Memory | Hybrid search stats (HNSW, BM25, RRF) |
| `Ctrl-B + 6` | Skills | 15+ skill registry, install, lint, audit |
| `Ctrl-B + 7` | Settings | Config viewer, 8-provider setup guide |
| `Ctrl-B + 8` | Logs | Live gateway output + daemon logs |
| `Ctrl-B + 9` | Security | Security audit report + 7-layer overview |

### Session Management

```bash
clawdesk tmux list                 # List active sessions
clawdesk tmux attach clawdesk      # Re-attach to a detached session
clawdesk tmux kill clawdesk        # Clean up
clawdesk tmux keys                 # Show key bindings cheat sheet
clawdesk tmux layouts              # Show all layout options
```

Mouse support is enabled by default. `Ctrl-B + z` zooms any pane to full screen.

See the full [tmux Desktop Guide](docs/tmux-workspace.md) for details.

## Terminal UI (TUI)

The ratatui-based TUI provides a full interactive dashboard in the terminal with Vim keybindings:

```bash
clawdesk tui                       # Launch with dark theme
clawdesk tui --theme light         # Light theme
clawdesk tui --theme high-contrast # High contrast
```

**Screens:** Dashboard, Chat, Sessions, Agents, Channels, Memory, Skills, Settings, Logs, Security

**Controls:** `j`/`k` scroll, `i` insert mode, `Tab` cycle screens, `Ctrl+1-9` switch sessions, `q` quit

## Security Model

ClawDesk implements defense-in-depth. Every layer enforces its own boundaries.

| # | Layer | How |
|---|-------|-----|
| 1 | **CascadeScanner** | Two-phase content scanning — Aho-Corasick (fast pass) + Regex (deep pass) — detects secrets, PII, and prompt injection. |
| 2 | **SHA-256 Audit Chain** | Every action (message send/receive, agent creation, skill activation, config change) is logged to a tamper-evident hash chain. |
| 3 | **Identity Contracts** | Each agent has a hash-locked persona. `IdentityContract` verifies persona integrity on every message to prevent drift. |
| 4 | **Scoped Tokens** | Capability-separated tokens (chat, admin, tools) — no single god-token. |
| 5 | **Rate Limiting** | Lock-free `ShardedRateLimiter` with 256KB fixed memory, zero heap allocation per request. |
| 6 | **Network Isolation** | Gateway binds to `127.0.0.1` only. External access requires WireGuard tunnel with invite-based pairing. |

## Cost Tracking

ClawDesk tracks token costs in real-time with per-model pricing:

| Model | Input (per 1M tokens) | Output (per 1M tokens) |
|-------|----------------------|------------------------|
| Claude Haiku 4.5 | $0.25 | $1.25 |
| Claude Sonnet 4.5 | $3.00 | $15.00 |
| Claude Opus 4.6 | $15.00 | $75.00 |
| Local (Ollama) | Free | Free |

## Development

### Running Tests

```bash
# Run all tests
cargo test --workspace

# Run tests for a specific crate
cargo test -p clawdesk-security
cargo test -p clawdesk-skills
cargo test -p clawdesk-agents

# Run with output
cargo test --workspace -- --nocapture

# Lint
cargo clippy --workspace
```

### Project Layout

```
clawdesk/
├── Cargo.toml              # Workspace root
├── crates/
│   ├── clawdesk-tauri/     # Tauri app (commands.rs, state.rs, lib.rs)
│   ├── clawdesk-agents/    # Agent runtime
│   ├── clawdesk-providers/ # LLM providers
│   ├── clawdesk-skills/    # Skill system
│   ├── clawdesk-security/  # Security (scanner, audit, identity)
│   ├── clawdesk-tunnel/    # WireGuard tunnel
│   ├── clawdesk-gateway/   # HTTP gateway
│   ├── clawdesk-types/     # Shared types
│   ├── ui/                 # React frontend
│   └── ...                 # 18 more crates
├── README.md
└── LICENSE
```

<details>
<summary><strong>Tauri IPC Commands</strong> (138+ commands)</summary>

The frontend communicates with the Rust backend through typed IPC commands across these categories:

| Category | Commands | Examples |
|----------|----------|----------|
| **Agent Management** | 7 | `create_agent`, `list_agents`, `update_agent`, `delete_agent`, `clone_agent` |
| **Chat & Sessions** | 9 | `send_message`, `get_session_messages`, `list_sessions`, `create_chat`, `export_session_markdown` |
| **Skills** | 6 | `list_skills`, `activate_skill`, `deactivate_skill`, `register_skill`, `validate_skill` |
| **Memory** | 5 | `remember_memory`, `recall_memories`, `forget_memory`, `get_memory_stats`, `remember_batch` |
| **Security & Auth** | 10 | `get_security_status`, `start_oauth_flow`, `generate_scoped_token`, `add_acl_rule`, `approve_request` |
| **Configuration** | 6 | `get_config`, `set_config`, `list_models`, `test_llm_connection`, `list_providers` |
| **Browser** | 11 | `browser_navigate`, `browser_click`, `browser_type`, `browser_screenshot`, `browser_scroll` |
| **Canvas & A2UI** | 8 | `canvas_present`, `canvas_hide`, `a2ui_push`, `a2ui_reset`, `device_info` |
| **MCP** | 4 | `mcp_connect`, `mcp_call`, `mcp_discover`, `mcp_list_tools` |
| **Threads** | 7 | `create_thread`, `get_thread`, `append_message`, `get_messages`, `thread_stats` |
| **Discovery & Tunnel** | 7 | `discovery_mdns_start`, `discovery_pair`, `tunnel_create_invite`, `tunnel_metrics` |
| **Media** | 5 | `media_transcribe_audio`, `media_analyze_image`, `media_tts` |
| **RAG** | 4 | `rag_ingest_file`, `rag_search`, `rag_list_documents`, `rag_remove_document` |
| **Local Models** | 5 | `local_models_status`, `local_models_recommend`, `local_models_download`, `local_models_start` |
| **Observability** | 6 | `get_metrics`, `get_agent_trace`, `list_traces`, `get_observability_dashboard` |
| **Cron** | 4 | `cron_list`, `cron_create`, `cron_remove`, `cron_trigger` |
| **Plugins** | 4 | `list_plugins`, `reload_plugin`, `enable_plugin`, `disable_plugin` |
| **Channels** | 3 | `list_channels`, `update_channel_config`, `test_channel_connection` |
| **System** | 6 | `daemon_status`, `clipboard_get`, `terminal_execute`, `file_read`, `file_write` |
| **Orchestration** | 5 | `orchestration_create_team`, `orchestration_add_agent`, `orchestration_spawn` |
| **Migration** | 3 | `migrate_import`, `migrate_preview`, `migrate_run` |

</details>

### Frontend (React + TypeScript)

The UI is located at `crates/ui/` and built with:

- **React 18** with hooks-based architecture
- **TypeScript** with strict mode
- **Vite** for development and building
- **Tailwind CSS** for styling

Key files:
- `src/App.tsx` — Main application component with 5 navigation views
- `src/api.ts` — Typed wrappers around Tauri `invoke()` calls
- `src/types.ts` — TypeScript interfaces matching Rust Serialize structs

## Contributing

ClawDesk is under active development, and help is welcome across the board:

- **Bug reports** — if something breaks, behaves oddly, or is unclear, please open an issue.
- **Testing** — especially on different operating systems, cloud VMs, Raspberry Pi, Docker, tmux, TUI, and headless deployments.
- **Documentation improvements** — missing setup steps, unclear explanations, platform notes, examples.
- **Code contributions** — features, fixes, refactors, tests, and performance work.

If you are using ClawDesk in real work, on unusual hardware, or in production-like environments, that feedback is particularly valuable.

1. Fork the repository
2. Create a feature branch (`git checkout -b feature/my-feature`)
3. Commit changes (`git commit -am 'Add my feature'`)
4. Push to branch (`git push origin feature/my-feature`)
5. Open a Pull Request

Please ensure:
- `cargo test --workspace` passes
- `cargo clippy --workspace` has no warnings
- New code includes tests

## License

This project is licensed under the MIT License — see the [LICENSE](LICENSE) file for details.

## Acknowledgments

- [OpenClaw](https://github.com/openclaw/openclaw) — The original AI agent gateway (TypeScript) that inspired ClawDesk's architecture, skill system, and channel abstractions.
- [SochDB](https://github.com/sochdb/sochdb) — Embedded ACID vector database powering all of ClawDesk's persistent storage.
- [Tauri](https://tauri.app/) — Desktop app framework that makes single-binary native apps possible.
- [llmfit](https://github.com/AlexsJones/llmfit) — LLM fine-tuning toolkit.
- [llama-swap](https://github.com/mostlygeek/llama-swap) — Hot-swap proxy for llama.cpp model serving.
- [llama.cpp](https://github.com/ggml-org/llama.cpp) — The inference engine behind ClawDesk's local model support.
- [pi-mono](https://github.com/badlogic/pi-mono) — AI agent toolkit: coding agent CLI, unified LLM API, TUI & web UI libraries, Slack bot, vLLM pods.
- [agency-agents](https://github.com/msitarzewski/agency-agents) — Multi-agent orchestration patterns.
