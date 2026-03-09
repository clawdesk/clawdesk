# Contributing to ClawDesk

Thank you for your interest in contributing to ClawDesk! This guide will help you get started.

## Table of Contents

- [Development Setup](#development-setup)
- [Architecture Overview](#architecture-overview)
- [Building](#building)
- [Testing](#testing)
- [Code Style](#code-style)
- [Pull Request Process](#pull-request-process)
- [Crate Guide](#crate-guide)

## Development Setup

### Prerequisites

- **Rust**: Install via [rustup](https://rustup.rs/). The project's `rust-toolchain.toml` will automatically install the correct toolchain version.
- **Node.js 18+**: Required for the Tauri frontend (React).
- **System dependencies**: See platform-specific notes below.

### Quick Start

```bash
# Clone the repository
git clone https://github.com/clawdesk/clawdesk.git
cd clawdesk

# Build all crates (rustup reads rust-toolchain.toml automatically)
cargo build

# Run tests
cargo test --workspace

# Run the CLI
cargo run --package clawdesk-cli

# Run the TUI
cargo run --package clawdesk-tui

# Run the Tauri desktop app
cd crates/clawdesk-tauri && cargo tauri dev
```

### Platform Notes

**macOS:**
```bash
# Xcode command line tools (for linking)
xcode-select --install
```

**Linux (Debian/Ubuntu):**
```bash
sudo apt-get install -y libssl-dev pkg-config libgtk-3-dev libwebkit2gtk-4.1-dev libayatana-appindicator3-dev librsvg2-dev
```

### Cross-Compilation

ARM and multi-architecture builds use `cross`:

```bash
cargo install cross
cross build --target aarch64-unknown-linux-gnu --release
```

See `Cross.toml` for configured targets.

## Architecture Overview

ClawDesk uses a **hexagonal (ports-and-adapters) architecture** with 43 crates organized into 8 layers:

```
Layer 0: clawdesk-types              ← Foundation (zero internal deps)
Layer 1: clawdesk-storage            ← Storage traits (no implementations)
Layer 2: clawdesk-domain, sochdb, simd ← Domain logic + storage impls
Layer 3: providers, security, channel  ← Adapters
Layer 4: agents, skills, mcp, sandbox  ← Implementations
Layer 5: gateway, runtime, acp, bus    ← Orchestration
Layer 6: observability, extensions     ← Infrastructure
Layer 7: cli, tauri, tui              ← Leaf binaries
```

**Key invariant:** Dependencies flow strictly downward. A crate at layer N may only depend on crates at layers < N. This is enforced by `cargo xtask dep-lint` in CI.

For architectural decisions and their rationale, see `docs/adr/`.

### Key Crates

| Crate | Purpose | Layer |
|-------|---------|-------|
| `clawdesk-types` | Shared type definitions (Message, Config, Error, etc.) | 0 |
| `clawdesk-storage` | Trait definitions for all storage backends | 1 |
| `clawdesk-domain` | Pure business logic, no I/O | 2 |
| `clawdesk-sochdb` | SochDB embedded ACID vector database adapter | 2 |
| `clawdesk-providers` | LLM provider adapters (OpenAI, Anthropic, Gemini, etc.) | 3 |
| `clawdesk-agents` | Agent execution engine, context management, tool calls | 4 |
| `clawdesk-skills` | Composable skill system, tool bundling, hot-reload | 4 |
| `clawdesk-gateway` | HTTP/WebSocket API server (axum) | 5 |
| `clawdesk-acp` | Agent Communication Protocol (A2A) | 5 |
| `clawdesk-cli` | 40+ command CLI interface | 7 |
| `clawdesk-tauri` | Tauri 2.0 desktop app with React frontend | 7 |
| `clawdesk-tui` | Terminal UI with 10 screens, Vim keybindings | 7 |

## Building

### Feature Tiers

ClawDesk supports feature-gated compilation tiers:

```bash
# Core only (~15 crates, fastest build)
cargo build --features core

# Desktop tier (adds Tauri, TUI, browser, canvas)
cargo build --features desktop

# Full build (all 43 crates)
cargo build --features full
```

### Build Automation (xtask)

All build tasks are automated via the `xtask` crate:

```bash
cargo xtask dep-lint    # Verify dependency boundaries
cargo xtask ci          # Full CI pre-flight (fmt + clippy + test + doc)
cargo xtask bench       # Run benchmarks
cargo xtask docs        # Build documentation
cargo xtask fuzz        # Manage fuzz targets
cargo xtask release     # Prepare a release
```

## Testing

```bash
# All tests
cargo test --workspace

# Specific crate
cargo test --package clawdesk-agents

# Property-based tests (proptest)
cargo test --workspace -- --include-ignored proptest

# With coverage
cargo llvm-cov --workspace --html
```

### Test Categories

- **Unit tests**: In-module `#[test]` functions
- **Integration tests**: `tests/` directories per crate
- **Property tests**: `proptest`-based invariant verification
- **Fuzz tests**: `cargo-fuzz` targets in `fuzz/`
- **E2E tests**: `tests/e2e_pipeline.rs`

## Code Style

### Enforced via CI

- `cargo fmt --all --check` — Consistent formatting
- `cargo clippy --workspace -- -D warnings` — Lint compliance
- `cargo xtask dep-lint` — Dependency boundary enforcement

### Guidelines

1. **No `unwrap()` in production code.** Use `?`, `unwrap_or`, `unwrap_or_else`, or `expect("justification")`.
2. **No `unsafe` without documentation.** Every `unsafe` block must have a `// SAFETY:` comment.
3. **No `println!`/`eprintln!` in library code.** Use `tracing::info!`, `tracing::warn!`, etc.
4. **No `todo!()` or `unimplemented!()` on main branch.** Use feature flags instead.
5. **Trait interfaces follow ISP.** No client should depend on methods it doesn't use.
6. **All public async functions should have `#[instrument]` annotations** for observability.

### Error Handling

- Library crates: Return `Result<T, E>` using `thiserror` for typed errors.
- Binary crates (cli, tauri, tui): Use `anyhow::Result` at the application boundary.
- Never panic in library code. Panics in async tasks poison shared state.

## Pull Request Process

1. **Fork and branch**: Create a feature branch from `main`.
2. **Run local CI**: `cargo xtask ci` before pushing.
3. **Write tests**: Every behavior change needs a test.
4. **Update docs**: If you change public APIs, update doc comments.
5. **Small PRs**: Prefer focused PRs over large changes.
6. **Sign commits**: Use `git commit -s` for DCO sign-off.

### Commit Messages

Follow [Conventional Commits](https://www.conventionalcommits.org/):

```
feat(agents): add declarative agent config via TOML
fix(providers): handle SSE stream truncation gracefully
docs(adr): add ADR-005 for SochDB selection rationale
test(acp): add property tests for capability lattice
refactor(security): extract capability enforcement module
```

## Crate Guide

### Adding a New Crate

1. Create `crates/clawdesk-<name>/Cargo.toml` using workspace dependencies.
2. Add to `members` in root `Cargo.toml`.
3. Assign a layer in `xtask/src/dep_lint.rs`.
4. Run `cargo xtask dep-lint` to verify boundaries.

### Adding a New Agent (No Code Required)

```bash
# Create from template
clawdesk agent create my-agent

# Edit the generated TOML
vim agents/my-agent.toml

# Deploy
clawdesk agent deploy agents/my-agent.toml
```

See `agents/` for 30+ pre-built agent templates.

### Adding a New Tool

Tools can be defined declaratively via `TOOL.toml` manifests:

```toml
[tool]
name = "my_tool"
description = "Does something useful"
version = "0.1.0"

[tool.capabilities]
network = ["api.example.com"]
filesystem = ["read"]

[tool.parameters]
query = { type = "string", required = true, description = "The query to process" }
```

See `tools/bundled/` for examples.

## Getting Help

- **Architecture questions**: Check `docs/adr/` for design decisions.
- **API reference**: `cargo doc --workspace --open`
- **Issues**: File a GitHub issue with reproduction steps.

## License

ClawDesk is licensed under the MIT License. See [LICENSE](LICENSE) for details.
