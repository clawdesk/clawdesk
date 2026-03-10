# ClawDesk CLI Guide

ClawDesk is a **local agent control plane** — a single binary that lets you run, manage, secure, and observe AI agents from your terminal. Unlike hosted chat wrappers, ClawDesk gives you daemon-managed services, encrypted backups, security auditing, permission-gated tool use, and a tmux-native workspace that mirrors the full desktop app.

This guide is organized around **golden paths**: the workflows you'll actually follow, in the order you'll need them.

---

## Path 1 — First Run

Go from zero to a working agent in under five minutes.

### 1.1 Install & Initialize

```bash
# Clone and build
git clone https://github.com/clawdesk/clawdesk.git && cd clawdesk
cargo build -p clawdesk-cli --release

# Move the binary somewhere in $PATH (optional)
cp target/release/clawdesk ~/.local/bin/

# Run the first-time wizard
clawdesk init
```

`clawdesk init` walks through provider setup, default model selection, and data directory creation (`~/.clawdesk/`). It writes a starter config and exits.

### 1.2 Configure a Provider

You need at least one LLM provider. Set keys interactively or via environment:

```bash
# Interactive login (validates the key immediately)
clawdesk login

# Or export directly
export ANTHROPIC_API_KEY="sk-ant-..."
export OPENAI_API_KEY="sk-..."
export GOOGLE_API_KEY="..."
```

| Provider | Key Source | Env Var |
|----------|-----------|---------|
| Anthropic | console.anthropic.com | `ANTHROPIC_API_KEY` |
| OpenAI | platform.openai.com | `OPENAI_API_KEY` |
| Google Gemini | aistudio.google.com | `GOOGLE_API_KEY` |
| Ollama | Local — no key needed | `OLLAMA_HOST` |
| Azure OpenAI | Azure portal | Endpoint + key in config |
| AWS Bedrock | AWS credentials | Standard AWS env vars |
| Cohere | dashboard.cohere.com | `COHERE_API_KEY` |

### 1.3 Verify Everything Works

```bash
clawdesk doctor --verbose
```

Doctor checks the Rust toolchain, database integrity (SochDB), provider API key validity, network connectivity, and disk space. Fix anything it flags before moving on.

### 1.4 Launch the tmux Desktop

The fastest path to a productive workspace:

```bash
# First time — guided onboarding + auto-launch
clawdesk tmux setup

# Subsequent launches
clawdesk tmux launch
```

`tmux setup` runs a 6-step wizard:

| Step | What It Does |
|------|-------------|
| 1 | Checks dependencies (tmux, cargo, curl, watch) |
| 2 | Configures provider API keys |
| 3 | Selects default model |
| 4 | Configures channels (Telegram, Discord, Slack) |
| 5 | Picks a layout (desktop / workspace / monitor / chat) |
| 6 | Launches the tmux session |

After setup completes, you're inside a 10-window tmux session mirroring the full Tauri desktop app. Navigate with `Ctrl-B + 0..9`.

> **Why tmux?** Sessions persist across disconnects (`Ctrl-B + d` to detach, `clawdesk tmux attach` to return). You can run multiple layouts simultaneously. Every pane is a real terminal where any CLI command works. See the full [tmux Desktop Guide](tmux-workspace.md).

### 1.5 Generate Shell Completions

```bash
clawdesk completions zsh  > ~/.zfunc/_clawdesk   # zsh
clawdesk completions bash > /etc/bash_completion.d/clawdesk  # bash
clawdesk completions fish > ~/.config/fish/completions/clawdesk.fish  # fish
```

---

## Path 2 — Daily Use

Your day-to-day loop: launch workspace, talk to agents, manage skills.

### 2.1 Launch Your Workspace

Pick the layout that fits your task:

| Layout | Panes | Best For |
|--------|-------|----------|
| `desktop` (default) | 10 windows | Full control plane — mirrors Tauri |
| `workspace` / `ws` / `dev` | 4 panes | Development — agent + gateway + health |
| `monitor` / `mon` / `ops` | 3 panes | Ops — gateway health + channels + logs |
| `chat` / `focus` | 2 panes | Quick conversation — agent + commands |

```bash
clawdesk tmux launch                       # desktop (default)
clawdesk tmux launch -l workspace          # dev layout
clawdesk tmux launch -l chat -m gpt-4o     # chat with specific model
clawdesk tmux launch -l monitor -s ops     # ops monitor in named session
```

Run multiple sessions in parallel:
```bash
clawdesk tmux launch -l desktop -s main
clawdesk tmux launch -l monitor -s ops
clawdesk tmux list                         # see all sessions
clawdesk tmux attach ops                   # switch to ops
```

### 2.2 Talk to Agents

**Interactive session** (REPL inside tmux or standalone):

```bash
clawdesk agent run                                # default model
clawdesk agent run --model claude-sonnet-4-20250514       # specific model
clawdesk agent run --workspace ./my-project       # set working dir
clawdesk agent run --permission-mode strict       # require approval for tools
clawdesk agent run --allow-all-tools              # skip tool confirmations
clawdesk agent run --system-prompt "You are a code reviewer"
clawdesk agent run --max-tool-rounds 20           # limit tool iterations
clawdesk agent run --team-dir ./teams/backend     # multi-agent team config
```

**Single-shot message** (scripting, pipelines, cron):

```bash
clawdesk agent msg "summarize the last 3 git commits"
clawdesk agent msg --model gpt-4o "review this PR"
clawdesk agent msg --thinking "plan a migration strategy"  # extended thinking
clawdesk message send --session abc123 "follow up on that"
```

### 2.3 Manage Agents

```bash
clawdesk agent list                        # list agents with routing info
clawdesk agent list --bindings             # show channel bindings
clawdesk agent list --json                 # machine-readable output
clawdesk agent add --from-toml agent.toml  # add from definition file
clawdesk agent validate                    # validate all agent definitions
clawdesk agent export my-agent --output .  # export agent to TOML
clawdesk agent apply                       # hot-reload all agents
clawdesk agent apply agent-id              # hot-reload specific agent
```

### 2.4 Skills

Skills extend what agents can do — code review, web search, file ops, custom tools.

```bash
# Browse & search
clawdesk skill list                        # installed skills
clawdesk skill list --eligible             # skills matching current agent
clawdesk skill search "code review"        # search the store
clawdesk skill search --category dev --verified  # filtered search
clawdesk skill info code-review            # show skill details

# Install & manage
clawdesk skill install code-review         # install from store
clawdesk skill install code-review --dry-run  # preview without installing
clawdesk skill update --all                # update all skills
clawdesk skill uninstall code-review       # remove

# Author & publish
clawdesk skill create --name my-skill      # scaffold a new skill
clawdesk skill lint                        # lint skill definitions
clawdesk skill test my-skill --input "test" # test with sample input
clawdesk skill audit                       # audit installed skills
clawdesk skill check my-skill              # check eligibility
clawdesk skill publish my-skill            # publish to store
```

### 2.5 Configuration

```bash
clawdesk config get model                  # read a value
clawdesk config set model claude-sonnet-4-20250514   # write a value
```

### 2.6 Channels

Connect agents to external platforms:

```bash
clawdesk channels status                   # show all channel states
clawdesk channels status --probe           # actively probe connectivity
```

Channel adapters (Telegram, Discord, Slack, etc.) are configured via `clawdesk config set` or the Settings screen in the tmux desktop (`Ctrl-B + 7`).

### 2.7 Scheduled Tasks

Automate agent work on a schedule:

```bash
clawdesk cron list                         # show scheduled tasks
clawdesk cron create                       # create a new task
clawdesk cron trigger task-id              # manually trigger now
clawdesk cron delete task-id               # remove a task
```

### 2.8 Terminal UI

For a ratatui-based dashboard with Vim keybindings (alternative to tmux):

```bash
clawdesk tui                               # launch TUI
clawdesk tui --theme dark                  # specific theme
clawdesk tui --gateway http://host:18789   # connect to remote gateway
```

---

## Path 3 — Service Mode

Run ClawDesk as a background service with the daemon. This is how you keep agents, channels, and cron tasks alive across reboots.

### 3.1 Install the Daemon

```bash
# Install as a platform service (systemd / launchd / Windows SCM)
clawdesk daemon install

# Start the daemon
clawdesk daemon start

# Check it's running
clawdesk daemon status
```

On macOS this creates a launchd plist. On Linux, a systemd unit. The daemon manages the gateway, channel adapters, cron scheduler, and agent lifecycle.

### 3.2 Daemon Operations

```bash
clawdesk daemon start                      # start the service
clawdesk daemon stop                       # graceful 6-phase shutdown
clawdesk daemon restart                    # stop + start
clawdesk daemon status                     # health check
clawdesk daemon logs                       # tail recent logs
clawdesk daemon logs --lines 200           # more history
clawdesk daemon run                        # foreground mode (debugging)
clawdesk daemon run --port 8080 --bind 0.0.0.0  # custom bind
clawdesk daemon uninstall                  # remove platform service
```

The daemon performs a **6-phase graceful shutdown**: stop accepting → drain requests → flush buffers → close channels → checkpoint database → exit. No data loss on `stop` or `restart`.

### 3.3 Gateway Server

The gateway is ClawDesk's HTTP/WebSocket API layer. It's usually managed by the daemon, but you can run it standalone:

```bash
clawdesk gateway run                       # default: 127.0.0.1:18789
clawdesk gateway run --port 8080           # custom port
clawdesk gateway run --bind 0.0.0.0        # listen on all interfaces
clawdesk gateway run --force               # force start even if port busy
```

The gateway serves:
- REST API at `/api/v1/...`
- OpenAI-compatible endpoint for drop-in replacement
- WebSocket streaming for real-time token output

### 3.4 Hot-Reload Configuration

Change configuration without restarting the daemon:

```bash
# Edit a config value
clawdesk config set model claude-sonnet-4-20250514

# Trigger reload (sends SIGHUP to daemon)
clawdesk config reload

# Validate config before reloading
clawdesk config validate

# View the reload policy (what can be hot-reloaded vs. requires restart)
clawdesk config policy
```

The reload policy tells you which settings take effect immediately and which require a full `daemon restart`.

### 3.5 Plugins

```bash
clawdesk plugins list                      # installed plugins
clawdesk plugins info my-plugin            # plugin details
clawdesk plugins reload my-plugin          # hot-reload a plugin
```

---

## Path 4 — Security & Compliance

ClawDesk is built with defense-in-depth. This path covers auditing, backups, and permission controls.

### 4.1 Security Audit

```bash
# Quick scan
clawdesk security audit

# Deep scan — checks credentials, permissions, configs, dependencies
clawdesk security audit --deep

# Auto-fix what it can
clawdesk security audit --fix

# Audit a specific config directory
clawdesk security audit --config-dir /path/to/config
```

The audit checks:
- Credential storage (are keys encrypted at rest?)
- File permissions on data directories
- Configuration for unsafe defaults
- Skill and plugin integrity
- Network exposure (is the gateway bound to 0.0.0.0?)
- Dependency vulnerabilities

### 4.2 Encrypted Backups

```bash
# Create an encrypted backup
clawdesk config backup

# Backup including API keys (requires confirmation)
clawdesk config backup --include-keys

# Backup to a specific location
clawdesk config backup --output ~/safe/clawdesk-backup.enc

# Restore from backup
clawdesk config restore ~/safe/clawdesk-backup.enc

# Dry-run restore (preview what would change)
clawdesk config restore ~/safe/clawdesk-backup.enc --dry-run

# Restore to a specific directory
clawdesk config restore backup.enc --target /path/to/dir
```

Backups are AES-encrypted. Store them off-machine for disaster recovery.

### 4.3 Permission Modes

Control how much autonomy agents have with tools:

```bash
# Strict mode — agent must request permission for every tool call
clawdesk agent run --permission-mode strict

# Default mode — common tools allowed, dangerous tools require approval
clawdesk agent run

# Unrestricted — skip all confirmations (use with caution)
clawdesk agent run --allow-all-tools
```

Permission decisions are logged in the audit trail for compliance review.

### 4.4 Skill Auditing

```bash
clawdesk skill audit                       # audit all installed skills
clawdesk skill audit --json                # machine-readable output
clawdesk skill lint                        # lint skill definitions
clawdesk skill check my-skill              # check specific skill eligibility
```

---

## Path 5 — Upgrades & Maintenance

### 5.1 Update ClawDesk

```bash
# Check for available updates
clawdesk update check

# Apply the update
clawdesk update apply

# Apply a prerelease version
clawdesk update apply --prerelease

# Something went wrong? Roll back
clawdesk update rollback
```

### 5.2 Upgrade Workflow

The safe upgrade path:

```bash
# 1. Backup current state
clawdesk config backup --output ~/clawdesk-pre-upgrade.enc

# 2. Check what's available
clawdesk update check

# 3. Stop the daemon
clawdesk daemon stop

# 4. Apply update
clawdesk update apply

# 5. Validate config against new version
clawdesk config validate

# 6. Restart daemon
clawdesk daemon start

# 7. Run diagnostics
clawdesk doctor --verbose

# 8. Verify audit is clean
clawdesk security audit
```

If step 5 or 6 fails:
```bash
clawdesk update rollback
clawdesk config restore ~/clawdesk-pre-upgrade.enc
clawdesk daemon start
```

### 5.3 Diagnostics

```bash
clawdesk doctor                            # quick health check
clawdesk doctor --verbose                  # detailed diagnostics
```

Doctor verifies:
- Rust toolchain version
- SochDB database connectivity and integrity
- Provider API key validity
- Network connectivity
- Disk space availability

---

## tmux Quick Reference

| Action | Command |
|--------|---------|
| First-time setup | `clawdesk tmux setup` |
| Launch desktop | `clawdesk tmux launch` |
| Launch layout | `clawdesk tmux launch -l <layout>` |
| Named session | `clawdesk tmux launch -s myname` |
| List sessions | `clawdesk tmux list` |
| Attach | `clawdesk tmux attach <session>` |
| Kill session | `clawdesk tmux kill <session>` |
| Show layouts | `clawdesk tmux layouts` |
| Key bindings | `clawdesk tmux keys` |

### tmux Key Bindings

| Key | Action |
|-----|--------|
| `Ctrl-B + 0..9` | Switch between desktop screens |
| `Ctrl-B + n` / `p` | Next / previous screen |
| `Ctrl-B + d` | Detach (session stays alive) |
| `Ctrl-B + z` | Zoom/unzoom pane (fullscreen toggle) |
| `Ctrl-B + arrow` | Switch panes within a screen |
| `Ctrl-B + [` | Scroll/copy mode (`q` to exit) |
| `Ctrl-B + s` | Session picker |
| `Ctrl-B + w` | Window picker |

### Desktop Layout — 10 Screens

| Key | Screen | What It Shows |
|-----|--------|---------------|
| `0` | Dashboard | System health, providers, agents, daemon |
| `1` | Chat | Agent REPL + session info |
| `2` | Sessions | Session list + detail/export |
| `3` | Agents | Agent registry + management |
| `4` | Channels | Channel connectivity + config |
| `5` | Memory | Memory search + stats (HNSW, BM25, RRF) |
| `6` | Skills | Skill registry + management |
| `7` | Settings | Config viewer + provider setup |
| `8` | Logs | Gateway output + daemon logs |
| `9` | Security | Audit report + security overview |

---

## Directory Structure

| Location | Content |
|----------|---------|
| `~/.clawdesk/` | Main data directory |
| `~/.clawdesk/sochdb/` | SochDB database files |
| `~/.clawdesk/skills/` | User-installed skills |
| `~/.clawdesk/plugins/` | User-installed plugins |
| `~/.clawdesk/backups/` | Encrypted backups |
| `~/.clawdesk/logs/` | Rotating log files |

Override with `CLAWDESK_DATA_DIR`.

## Environment Variables

| Variable | Purpose | Default |
|----------|---------|---------|
| `CLAWDESK_DATA_DIR` | Data directory | `~/.clawdesk` |
| `CLAWDESK_GATEWAY_PORT` | Gateway port | `18789` |
| `CLAWDESK_GATEWAY_HOST` | Gateway bind address | `127.0.0.1` |
| `CLAWDESK_LOG_LEVEL` | Log verbosity | `info` |
| `OTEL_EXPORTER_OTLP_ENDPOINT` | OpenTelemetry collector | disabled |
| `ANTHROPIC_API_KEY` | Anthropic API key | — |
| `OPENAI_API_KEY` | OpenAI API key | — |
| `GOOGLE_API_KEY` | Google API key | — |
| `OLLAMA_HOST` | Ollama server URL | `http://localhost:11434` |

## Full Command Reference

```
clawdesk init                              # First-time setup wizard
clawdesk login                             # Interactive provider login
clawdesk doctor [--verbose]                # Run diagnostics

clawdesk agent run [OPTIONS]               # Interactive agent session
clawdesk agent msg <MESSAGE> [OPTIONS]     # Single-shot message
clawdesk agent list [--bindings|--json]    # List agents
clawdesk agent add --from-toml <FILE>      # Add agent from TOML
clawdesk agent validate                    # Validate agent definitions
clawdesk agent export <ID> [--output DIR]  # Export agent to TOML
clawdesk agent apply [ID]                  # Hot-reload agents

clawdesk config set <KEY> <VALUE>          # Set config value
clawdesk config get <KEY>                  # Get config value
clawdesk config backup [--output|--include-keys]  # Encrypted backup
clawdesk config restore <FILE> [--dry-run|--target]  # Restore backup
clawdesk config reload                     # Hot-reload (SIGHUP)
clawdesk config validate                   # Validate configuration
clawdesk config policy                     # Show reload policy

clawdesk daemon run [--port|--bind]        # Foreground daemon
clawdesk daemon install                    # Install platform service
clawdesk daemon uninstall                  # Remove platform service
clawdesk daemon start                      # Start service
clawdesk daemon stop                       # Graceful shutdown
clawdesk daemon restart                    # Stop + start
clawdesk daemon status                     # Health check
clawdesk daemon logs [--lines N]           # Tail logs

clawdesk gateway run [--port|--bind|--force]  # Start gateway

clawdesk security audit [--deep|--fix|--config-dir]  # Security scan

clawdesk skill list [--eligible|--json|--verbose]  # List skills
clawdesk skill info <ID> [--json]          # Skill details
clawdesk skill search <QUERY> [OPTIONS]    # Search store
clawdesk skill install <ID> [--force|--dry-run]  # Install
clawdesk skill uninstall <ID>              # Remove
clawdesk skill update [ID|--all]           # Update skills
clawdesk skill create --name <NAME>        # Scaffold new skill
clawdesk skill lint [--dir DIR]            # Lint definitions
clawdesk skill test <ID> --input <INPUT>   # Test skill
clawdesk skill audit [--json]              # Audit skills
clawdesk skill check <ID>                  # Check eligibility
clawdesk skill publish <ID>                # Publish to store

clawdesk tmux setup [--session|--workspace]  # Onboarding wizard
clawdesk tmux launch [OPTIONS]             # Launch workspace
clawdesk tmux list                         # List sessions
clawdesk tmux attach <SESSION>             # Attach to session
clawdesk tmux kill <SESSION>               # Kill session
clawdesk tmux layouts                      # Show available layouts
clawdesk tmux keys                         # Key bindings cheat sheet

clawdesk channels status [--probe]         # Channel status
clawdesk message send [--session|--model]  # Send message
clawdesk cron list                         # List tasks
clawdesk cron create                       # Create task
clawdesk cron trigger <ID>                 # Trigger task
clawdesk cron delete <ID>                  # Delete task

clawdesk plugins list                      # List plugins
clawdesk plugins info <ID>                 # Plugin details
clawdesk plugins reload <ID>               # Hot-reload plugin

clawdesk update check                      # Check for updates
clawdesk update apply [--prerelease]       # Apply update
clawdesk update rollback                   # Rollback update

clawdesk tui [--gateway URL|--theme THEME] # Terminal UI
clawdesk completions <SHELL>               # Shell completions
```
