# Architecture Decision Records

This directory contains Architecture Decision Records (ADRs) for ClawDesk.

## What is an ADR?

An ADR captures a significant architectural decision along with its context and consequences. ADRs help new contributors understand *why* the system is structured this way, not just *how*.

## ADR Index

| ADR | Title | Status | Date |
|-----|-------|--------|------|
| [001](001-hexagonal-architecture.md) | Hexagonal Architecture | Accepted | 2024-01-15 |
| [002](002-sochdb-over-sqlite.md) | SochDB over SQLite for primary storage | Accepted | 2024-01-20 |
| [003](003-acp-protocol.md) | Agent Communication Protocol (ACP) | Accepted | 2024-02-10 |
| [004](004-single-binary.md) | Single-binary Tauri desktop app | Accepted | 2024-02-15 |
| [005](005-trait-based-storage.md) | Trait-based storage abstraction | Accepted | 2024-01-18 |
| [006](006-declarative-agents.md) | Declarative agent configuration via TOML | Accepted | 2024-06-01 |
| [007](007-capability-security.md) | Capability-based security per agent | Accepted | 2024-06-01 |

## ADR Template

Use `docs/adr/template.md` when creating new ADRs.
