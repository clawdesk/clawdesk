# CLI Agent Orchestration

ClawDesk can invoke external CLI agents (Claude Code, Codex, Gemini CLI, Aider, GitHub Copilot) as child processes with full lifecycle management.

## Architecture

```
Parent Agent (ClawDesk runner)
  │
  ├─ spawn("claude", task_a) ──→ [Process A] ──→ output_a.json
  │                                                    ↓
  ├─ spawn("codex", task_b)  ──→ [Process B] ──→ output_b.jsonl
  │                                                    ↓
  └─ wait_any / wait_all     ←── output relay ←── system events queue
```

## Built-in Agents

| Agent | Command | Session Resume | Output Format | Cost/Run |
|-------|---------|----------------|---------------|----------|
| Claude Code | `claude` | `--resume {sessionId}` | JSON | ~$0.50 |
| Codex CLI | `codex` | `exec resume {sessionId}` | JSONL (fresh) / Text (resume) | ~$0.30 |
| Gemini CLI | `gemini` | N/A | Text | ~$0.20 |
| Aider | `aider` | N/A | Text | ~$0.40 |
| GitHub Copilot | `gh copilot` | N/A | Text | ~$0.01 |

## Session Resume (GAP 1)

Multi-turn conversations with CLI agents reuse session IDs to avoid re-sending full context:

```rust
// First invocation — fresh run
let config = SpawnConfig {
    agent_id: "claude-code".into(),
    task: "Implement the auth module".into(),
    session_id: None,  // fresh
    ..
};

// After completion, extract session ID from output
let session_id = orchestrator.extract_session_id("run-1").await;

// Subsequent invocation — resume
let config = SpawnConfig {
    agent_id: "claude-code".into(),
    task: "Now add rate limiting".into(),
    session_id,  // resumes the existing session
    ..
};
```

**Impact:** Without resume, a 10-turn conversation costs 10× the tokens. With resume: 1× base + incremental additions.

Each agent spec defines:
- `resume_args` — argument template with `{sessionId}` placeholder
- `session_id_fields` — JSON fields to search in output for session ID
- `session_mode` — `always` (Claude), `existing` (Codex), `never` (Aider)

## Output Parsing (GAP 2)

CLI output is parsed according to per-agent `output_format`:

| Format | Parser | Example |
|--------|--------|---------|
| JSON | `parse_cli_json()` | `{"result": "...", "session_id": "...", "usage": {...}}` |
| JSONL | `parse_cli_jsonl()` | Line-by-line JSON, aggregated text + last usage |
| Text | Passthrough | Raw stdout |

Parsed `CliOutput` contains:
- `text` — agent's response
- `session_id` — for future resume
- `usage` — input/output/cache tokens for cost tracking

## Bootstrap Context (GAP 3)

Workspace context is injected into CLI agents automatically:

- Agents with `system_prompt_flag` (e.g., Claude Code's `--append-system-prompt`) get bootstrap as a separate argument
- Agents without the flag get bootstrap prepended to the task prompt
- Bootstrap is truncated to `bootstrap_max_chars` per agent

Bootstrap context is derived from the workspace via `bootstrap.rs` discovery (README, Cargo.toml, CLAUDE.md, etc.).

## Config Override (GAP 4)

Users can override any agent spec field via `PartialCliSpec` deep merge:

```toml
[cli_backends.claude-code]
no_output_timeout_secs = 60
max_concurrent = 5
```

Only specified fields override — all other defaults are preserved.

## Auth Credential Management (GAP 5)

Each agent spec defines `clear_env` — environment variables stripped from the child process:

```
Claude Code:  clear_env = ["ANTHROPIC_API_KEY"]
Codex CLI:    clear_env = ["OPENAI_API_KEY"]
```

This prevents:
- Shared API keys hitting rate limits
- Child processes using the wrong billing account
- Credential leakage to untrusted CLI agents

## Watchdog (GAP 6)

Timeouts are mode-aware:

| Mode | Claude Code | Codex | Rationale |
|------|-------------|-------|-----------|
| Fresh | 120s | 120s | Needs time to build context |
| Resume | 60s | 45s | Context already loaded — faster expected |

The health checker uses `effective_timeout(spec, is_resume)` to apply the correct timeout.

## Concurrency & Cost Control

- **Per-agent limits:** `max_concurrent` prevents one agent from monopolizing resources
- **Global limit:** `max_global_concurrent` caps total parallel agents (default: 8)
- **Budget cap:** `global_budget_cap_usd` kills spawns when total estimated cost exceeds threshold (default: $50)
- **Blocking detection:** `is_blocked()` detects hung agents that produce no output

## Git Worktree Isolation

`use_worktree: true` in `SpawnConfig` creates a separate git worktree for each agent, preventing merge conflicts when multiple agents edit the same repo.
