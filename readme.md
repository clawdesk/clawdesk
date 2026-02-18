# ClawDesk

**A privacy-first, security-hardened AI agent desktop runtime built in Rust.**

ClawDesk is a Tauri 2.0 desktop application that runs AI agents locally with full audit trails, identity verification, and zero-trust networking. Built as a 27-crate Rust workspace, it provides a production-grade agent runtime with a React + TypeScript frontend.

## Features

- **Local-First**: Agents run on your machine. No cloud dependency required.
- **Security-Hardened**: CascadeScanner (Aho-Corasick + Regex), SHA-256 audit chain, scoped tokens, identity contracts.
- **Multi-Model**: Supports Claude (Haiku/Sonnet/Opus), OpenAI, Gemini, Bedrock, and local models via Ollama.
- **Skill Registry**: 15 built-in skills with hot-loading, activation/deactivation, and per-skill ACLs.
- **WireGuard Tunnel**: Peer-to-peer encrypted networking with invite-based device pairing.
- **Full Observability**: Real-time tracing, cost tracking, token budgeting, and tamper-evident audit logs.
- **Desktop UI**: Tauri 2.0 + React frontend with 5 navigation views (Now, Ask, Routines, Accounts, Library).

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

## Crate Structure

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

## Prerequisites

- **Rust** 1.75+ (`rustup` recommended)
- **Node.js** 20+ and **pnpm** (for the UI)
- **Tauri CLI**: `cargo install tauri-cli`
- **System dependencies** (macOS): Xcode Command Line Tools
- **System dependencies** (Linux): `webkit2gtk-4.1`, `libssl-dev`, `libgtk-3-dev`

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

### Tauri IPC Commands

The frontend communicates with the Rust backend through 21 typed IPC commands:

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

## Security Model

ClawDesk implements defense-in-depth:

1. **CascadeScanner**: Two-phase content scanning using Aho-Corasick (fast pass) + Regex (deep pass) for detecting secrets, PII, and prompt injection.

2. **SHA-256 Audit Chain**: Every action (message send/receive, agent creation, skill activation, config changes) is logged to a tamper-evident hash chain.

3. **Identity Contracts**: Each agent has a hash-locked persona. The `IdentityContract` verifies persona integrity on every message to prevent drift.

4. **Scoped Tokens**: Authentication uses capability-separated tokens (chat, admin, tools) rather than a single god-token.

5. **Rate Limiting**: Lock-free `ShardedRateLimiter` with 256KB fixed memory, no heap allocation per request.

6. **Network Isolation**: Gateway binds to `127.0.0.1` only. External access requires WireGuard tunnel with invite-based pairing.

## Cost Model

ClawDesk tracks token costs in real-time with per-model pricing:

| Model | Input (per 1M tokens) | Output (per 1M tokens) |
|-------|----------------------|------------------------|
| Claude Haiku 4.5 | $0.25 | $1.25 |
| Claude Sonnet 4.5 | $3.00 | $15.00 |
| Claude Opus 4.6 | $15.00 | $75.00 |
| Local (Ollama) | Free | Free |

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

- [Tauri](https://tauri.app/) — Desktop app framework
- [SochDB](https://github.com/anthropics/sochdb) — Embedded ACID vector database
- [OpenClaw](https://github.com/anthropics/openclaw) — Original AI agent gateway (TypeScript)
