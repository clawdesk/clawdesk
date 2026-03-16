# ClawDesk Documentation

Welcome to the ClawDesk documentation. ClawDesk is a privacy-first, security-hardened AI agent desktop runtime built in Rust with a React + TypeScript frontend.

## Documentation Index

| Document | Description |
|----------|-------------|
| [Getting Started](getting-started.md) | Installation, first run, creating your first agent |
| [CLI Guide](cli-guide.md) | Golden-path workflows: first run, daily use, service mode, security, upgrades |
| [tmux Desktop Guide](tmux-workspace.md) | tmux 10-screen desktop experience mirroring Tauri, onboarding, presets |
| [Architecture Overview](architecture.md) | System architecture, crate structure, data flow |
| [Agent System](agent-system.md) | Agent runner, pipelines, failover, tools, context management |
| [CLI Orchestration](cli-orchestration.md) | External CLI agent lifecycle: spawn, resume, output parsing, cost control |
| [Memory System](memory-system.md) | Embeddings, hybrid search, memory lifecycle |
| [Skills & Plugins](skills-and-plugins.md) | Skill authoring, registry, triggers, plugin hooks |
| [Channels & Messaging](channels-and-messaging.md) | Channel adapters, auto-reply, threading, media |
| [Voice & TTS](voice-and-tts.md) | Multi-provider speech synthesis, VoiceWake, voice calls |
| [Browser Automation](browser-automation.md) | CDP, extension relay, session registry, route dispatch |
| [Security & Safety](security-and-safety.md) | Audit trails, scanning, RBAC, OAuth2, sandboxing, ReDoS protection |
| [Configuration Guide](configuration.md) | Provider setup, agent config, skill config, env vars |
| [API Reference](api-reference.md) | Tauri IPC commands, gateway HTTP/WS API |
| [Troubleshooting](troubleshooting.md) | Common issues, debugging, diagnostics |

## Quick Start

```bash
# Build and run the desktop app
./run-tauri.sh

# Or run the gateway server standalone
cargo run -p clawdesk-cli -- gateway run

# Launch a tmux desktop (10-screen Tauri-like experience)
clawdesk tmux setup          # First-time onboarding + launch
clawdesk tmux launch         # Quick launch (desktop layout)
clawdesk tmux launch -l chat # Chat-focused layout

# Run tests
cargo test --workspace
```

## Architecture at a Glance

```
┌────────────────────────────────────────────────────┐
│                   Desktop App (Tauri)              │
│  ┌──────────┐  ┌───────────┐  ┌────────────────┐  │
│  │ React UI │  │  System   │  │  IPC Commands  │  │
│  │ (WebView)│  │   Tray    │  │  (~138 cmds)   │  │
│  └──────────┘  └───────────┘  └────────────────┘  │
├────────────────────────────────────────────────────┤
│                    AppState                        │
│  ┌─────────┐ ┌────────┐ ┌──────┐ ┌────────────┐  │
│  │ Agents  │ │ Memory │ │Skills│ │  Security   │  │
│  │ Runner  │ │Manager │ │ Reg  │ │  Scanner    │  │
│  └─────────┘ └────────┘ └──────┘ └────────────┘  │
├────────────────────────────────────────────────────┤
│              Gateway (Axum HTTP/WS)                │
│  ┌──────────┐ ┌───────────┐ ┌──────────────────┐  │
│  │ REST API │ │  OpenAI   │ │   WebSocket      │  │
│  │  /api/v1 │ │  Compat   │ │   Streaming      │  │
│  └──────────┘ └───────────┘ └──────────────────┘  │
├────────────────────────────────────────────────────┤
│                   Core Engine                      │
│  ┌─────────┐ ┌────────┐ ┌──────┐ ┌────────────┐  │
│  │Providers│ │Pipeline│ │Tunnel│ │  Channels   │  │
│  │ (8 LLM) │ │ DAG    │ │WireG │ │  (25+)     │  │
│  └─────────┘ └────────┘ └──────┘ └────────────┘  │
├────────────────────────────────────────────────────┤
│                 SochDB (Embedded ACID)             │
│  Vector Search │ Knowledge Graph │ Semantic Cache  │
│  Tracing       │ Checkpoints     │ Policy Engine   │
└────────────────────────────────────────────────────┘
```
