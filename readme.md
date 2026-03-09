<h1 align="center">ClawDesk</h1>

<p align="center">
  <strong>Privacy-first, security-hardened AI agent desktop runtime — 100% Rust backend, zero cloud dependency.</strong>
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
  <a href="#security-model">Security</a> ·
  <a href="#crate-structure">Crates</a> ·
  <a href="#development">Development</a> ·
  <a href="#contributing">Contributing</a>
</p>

---

ClawDesk is a Tauri 2.0 desktop application that runs AI agents locally with full audit trails, identity verification, and zero-trust networking. Built as a **27-crate Rust workspace**, it provides a production-grade agent runtime with a React + TypeScript frontend.

Inspired by [OpenClaw](https://github.com/openclaw/openclaw) — the TypeScript AI agent gateway — ClawDesk reimagines the same powerful concepts (multi-channel messaging, skill orchestration, agent sessions) as a **native desktop app** with a pure Rust backend. Less moving parts, fewer dependencies, one binary.

## Goal

> **Simplify. Reduce friction. Ship one binary.**

OpenClaw is incredibly capable — but running a Node.js gateway, wiring up channels, and managing configs can be a lot of moving parts. ClawDesk's goal is to take those same ideas and collapse them into a single desktop app that just works:

- **No gateway to run** — the Tauri app _is_ the runtime.
- **No config files to write** — everything is managed through the UI.
- **No separate daemon** — agents, skills, channels, and storage live in one process.
- **No runtime dependencies** — Rust compiles to a single native binary. No Node.js, no Python, no Docker required.

## Why ClawDesk

- **Your machine, your data.** Agents run locally — no cloud roundtrips, no data leaving your device unless you choose to.
- **Defense-in-depth by default.** CascadeScanner, SHA-256 audit chain, scoped tokens, and identity contracts ship with every build.
- **Multi-model, no lock-in.** Swap between Claude, OpenAI, Gemini, Bedrock, and Ollama via a single provider trait.
- **One binary, full stack.** 27 Rust crates compile into a single Tauri app — agents, skills, security, tunnels, and UI included.

## Features

| | |
|---|---|
| **Local-First Runtime** | Agents execute on your machine. No cloud dependency required. |
| **Security Hardened** | CascadeScanner (Aho-Corasick + Regex), SHA-256 audit chain, scoped tokens, identity contracts. |
| **Multi-Model Support** | Claude (Haiku/Sonnet/Opus), OpenAI, Gemini, Bedrock, and local models via Ollama. |
| **Skill Registry** | 15 built-in skills ported from [OpenClaw's skill system](https://github.com/openclaw/openclaw), with hot-loading, activation/deactivation, and per-skill ACLs. |
| **WireGuard Tunnel** | Peer-to-peer encrypted networking with invite-based device pairing. |
| **Full Observability** | Real-time tracing, cost tracking, token budgeting, and tamper-evident audit logs. |
| **Desktop UI** | Tauri 2.0 + React frontend with intuitive navigation — Chat, Overview, Automations, Skills, Settings, and more. |

## Powered by SochDB

All persistent storage — agent state, sessions, audit logs, skill configs, and vector embeddings — is handled by [**SochDB**](https://github.com/sochdb/sochdb), an embedded ACID-compliant database written in Rust. SochDB provides:

- **Embedded & zero-config** — no external database process, no connection strings.
- **ACID transactions** — crash-safe writes for audit chains and session state.
- **Vector search** — built-in cosine similarity for memory/recall without external vector DBs.
- **Single-file storage** — one portable database file per workspace.

## Channels

Channels are messaging platform integrations that let your agent send and receive messages across different surfaces. ClawDesk implements the channel abstraction from OpenClaw as Rust traits:

| Channel | Status | Description |
|---------|--------|-------------|
| **Telegram** | Supported | Bot API via grammY-equivalent Rust client |
| **Discord** | Supported | Bot with slash commands, DM pairing, guild routing |
| **Slack** | Supported | Bolt-equivalent with app/bot token auth |
| **WebChat** | Supported | Built-in browser UI served from the gateway |
| **Google Chat** | Planned | Chat API integration |
| **Signal** | Planned | signal-cli bridge |
| **Microsoft Teams** | Planned | Bot Framework adapter |
| **Matrix** | Planned | Matrix SDK client |
| **WhatsApp** | Planned | Baileys-equivalent bridge |

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
│                    ClawDesk Desktop App                       │
├──────────────┬───────────────────────────────────────────────┤
│  React UI    │              Tauri IPC Bridge                 │
│  (TypeScript)│  21 commands · typed invoke() wrappers        │
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

The React frontend communicates with the Rust backend through **21 typed IPC commands** over the Tauri bridge. The backend manages agent lifecycle, security scanning, skill orchestration, and model routing — all in-process, no sidecar daemons.

## Crate Structure

<details>
<summary><strong>All 27 crates</strong> (click to expand)</summary>

| Crate | Purpose |
|-------|---------|
| `clawdesk-tauri` | Tauri 2.0 app shell, IPC commands, application state |
| `clawdesk-agents` | Agent runner, tool registry, context assembly |
| `clawdesk-providers` | LLM provider trait + implementations (Anthropic, OpenAI, Gemini, Bedrock, Ollama) |
| `clawdesk-skills` | Skill definition, registry, bundled skills, hot-loading |
| `clawdesk-security` | CascadeScanner, AuditLogger, IdentityContract, rate limiting, allowlists |
| `clawdesk-tunnel` | WireGuard-based P2P tunnel, invite management, metrics |
| `clawdesk-gateway` | HTTP gateway, Responses API, SSE streaming |
| `clawdesk-channel` | Channel trait for messaging platform integration |
| `clawdesk-channels` | Channel implementations (Telegram, Discord, Slack, etc.) |
| `clawdesk-domain` | Domain models, session state machines |
| `clawdesk-types` | Shared type definitions (messages, envelopes, security types) |
| `clawdesk-storage` | Storage abstraction layer |
| `clawdesk-sochdb` | SochDB embedded database integration |
| `clawdesk-memory` | Memory and vector search integration |
| `clawdesk-media` | Media processing (images, audio, video) |
| `clawdesk-cron` | Scheduled task execution |
| `clawdesk-plugin` | Plugin system and lifecycle management |
| `clawdesk-infra` | Infrastructure utilities (config, error handling, observability) |
| `clawdesk-cli` | Command-line interface |
| `clawdesk-autoreply` | Auto-reply rule engine |
| `clawdesk-acp` | Agent Communication Protocol |
| `ui` | React + TypeScript + Vite frontend |

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
git clone https://github.com/anthropics/clawdesk.git
cd clawdesk

# Install UI dependencies
cd crates/ui && pnpm install && cd ../..

# Run in development mode
cargo tauri dev

# Build for production
cargo tauri build
```

`cargo tauri dev` and `cargo tauri build` automatically prepare the bundled `llama-server` sidecar used by Local Models. A machine-level `llama-server` install is not required.

If you need to skip the auto-download in offline CI, set `CLAWDESK_SKIP_LLAMA_SERVER_DOWNLOAD=1` and provide the sidecar files under [crates/clawdesk-tauri/binaries](/Users/sushanth/llamabot/clawdesk/crates/clawdesk-tauri/binaries) yourself.

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
<summary><strong>Tauri IPC Commands</strong> (21 commands)</summary>

The frontend communicates with the Rust backend through typed IPC commands:

| Command | Description |
|---------|-------------|
| `get_health` | Engine health check (version, uptime, skills, tunnel) |
| `create_agent` | Create agent with IdentityContract and security scan |
| `list_agents` | List all registered agents |
| `delete_agent` | Delete agent and clean up identity/sessions |
| `import_openclaw_config` | Import OpenClaw JSON config with security scanning |
| `send_message` | Send message with CascadeScanner + audit logging |
| `get_session_messages` | Retrieve message history for an agent |
| `list_skills` | List all skills from the real SkillRegistry |
| `activate_skill` | Activate a skill in the registry |
| `deactivate_skill` | Deactivate a skill in the registry |
| `list_pipelines` | List agent pipelines |
| `create_pipeline` | Create a multi-agent pipeline |
| `run_pipeline` | Execute a pipeline |
| `get_metrics` | Get cost/token metrics |
| `get_security_status` | Query CascadeScanner + AuditLogger status |
| `get_agent_trace` | Get execution trace for an agent |
| `get_tunnel_status` | WireGuard tunnel metrics |
| `create_invite` | Create device pairing invite |
| `get_config` | Get runtime configuration |
| `list_models` | List available LLM models |
| `list_channels` | List messaging channels |

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
