# ClawDesk Documentation

Welcome to the ClawDesk documentation. ClawDesk is a privacy-first, security-hardened AI agent desktop runtime built in Rust with a React + TypeScript frontend.

## Documentation Index

| Document | Description |
|----------|-------------|
| [Architecture Overview](architecture.md) | System architecture, crate structure, data flow |
| [Getting Started](getting-started.md) | Installation, first run, creating your first agent |
| [Agent System](agent-system.md) | Agent runner, pipelines, failover, tools, context management |
| [Memory System](memory-system.md) | Embeddings, hybrid search, memory lifecycle |
| [Skills & Plugins](skills-and-plugins.md) | Skill authoring, registry, triggers, plugin hooks |
| [Channels & Messaging](channels-and-messaging.md) | Channel adapters, auto-reply, threading, media |
| [Security & Safety](security-and-safety.md) | Audit trails, scanning, RBAC, OAuth2, sandboxing |
| [Configuration Guide](configuration.md) | Provider setup, agent config, skill config, env vars |
| [API Reference](api-reference.md) | Tauri IPC commands, gateway HTTP/WS API |
| [Troubleshooting](troubleshooting.md) | Common issues, debugging, diagnostics |

## Quick Start

```bash
# Build and run the desktop app
./run-tauri.sh

# Or run the gateway server standalone
cargo run -p clawdesk-cli -- gateway run

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
