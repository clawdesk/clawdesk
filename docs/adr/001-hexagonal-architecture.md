# ADR-001: Hexagonal Architecture

## Status
Accepted

## Context
ClawDesk is a multi-channel AI agent gateway with 43 crates, 8 communication channels, 8 LLM providers, and multiple storage backends. The system needs clear boundaries between business logic and external adapters to enable independent testing, swappable implementations, and manageable complexity at scale.

We considered three architectural patterns:
1. **Layered architecture** — simple but allows transitive coupling
2. **Hexagonal (ports-and-adapters)** — strict boundary enforcement via traits
3. **Microkernel** — used by OpenFang with 13 crates, but less suitable for 40+ crates

## Decision
Adopt hexagonal architecture with 8 dependency layers (0-7). All inter-crate communication flows through trait-defined ports. Storage backends, LLM providers, and channel adapters are interchangeable implementations of abstract ports.

The layer ordering is enforced at compile time via `cargo xtask dep-lint`:
- Layer 0: `clawdesk-types` (foundation)
- Layer 1: `clawdesk-storage` (trait definitions)
- Layer 2: Domain logic + storage implementations
- Layer 3: Adapters (providers, security, channels)
- Layer 4: Implementations (agents, skills, MCP)
- Layer 5: Orchestration (gateway, runtime, ACP)
- Layer 6: Infrastructure (observability, extensions)
- Layer 7: Leaf binaries (CLI, Tauri, TUI)

## Consequences

### Positive
- Each crate can be tested in isolation with mock implementations of its trait dependencies
- Storage backend can be swapped (SochDB → SQLite → Postgres) without touching business logic
- LLM providers are hot-swappable with consistent interfaces
- Dependency boundary violations are caught by CI, not code review

### Negative
- More crates means more `Cargo.toml` maintenance and longer `cargo metadata` times
- Trait indirection can make call chains harder to follow in IDE navigation
- Some code duplication across adapter implementations

### Neutral
- Build parallelism is excellent due to the wide DAG structure

## Alternatives Considered

**Microkernel (OpenFang-style):** 13 crates with a central kernel. Rejected because ClawDesk's broader scope (8 channels, SochDB, canvas, browser automation, consensus) would create a monolithic kernel crate.

**Flat module structure:** Single crate with modules. Rejected because it provides no compile-time boundary enforcement and makes it easy to create hidden dependencies between modules.
