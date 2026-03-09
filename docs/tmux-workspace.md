# tmux Desktop Guide

ClawDesk includes a built-in tmux session manager that mirrors the **Tauri desktop app experience** in the terminal. The **desktop** layout creates 10 tmux windows — one for each screen in the Tauri app — so you get the same full-featured experience without a GUI.

## Quick Start

```bash
# First-time: guided wizard + auto-launch (default: desktop layout)
clawdesk tmux setup

# Launch the full 10-screen desktop experience
clawdesk tmux launch

# Or a simpler preset
clawdesk tmux launch --layout workspace   # 4-pane dev layout
clawdesk tmux launch --layout monitor     # 3-pane ops dashboard
clawdesk tmux launch --layout chat        # 2-pane focused chat
```

## Prerequisites

- **tmux** 3.0+ — Install with `brew install tmux` (macOS) or `sudo apt install tmux` (Linux)
- **watch** — Usually pre-installed; used for live status panels

## Layouts

### Desktop (default) — Full Tauri Experience

The **desktop** layout creates **10 tmux windows**, each mirroring a screen from the Tauri desktop app. Navigate between screens with `Ctrl-B + 0..9`.

```
  Status Bar:  ClawDesk  | 0:Dashboard  1:Chat  2:Sessions ... 9:Security | 14:32 clawdesk
```

| Window | Screen | Panes | What It Shows |
|--------|--------|-------|---------------|
| `Ctrl-B + 0` | **Dashboard** | 4 | System health, provider status, agent list, daemon status |
| `Ctrl-B + 1` | **Chat** | 2 | Agent REPL (interactive chat) + session info |
| `Ctrl-B + 2` | **Sessions** | 2 | Session list + session detail/export |
| `Ctrl-B + 3` | **Agents** | 3 | Agent registry + management commands + actions |
| `Ctrl-B + 4` | **Channels** | 2 | Channel connectivity status + configuration guide |
| `Ctrl-B + 5` | **Memory** | 2 | Memory search + system stats (HNSW, BM25, RRF) |
| `Ctrl-B + 6` | **Skills** | 3 | Skill registry + management commands + actions |
| `Ctrl-B + 7` | **Settings** | 2 | Configuration viewer + provider setup guide |
| `Ctrl-B + 8` | **Logs** | 2 | Gateway output (live) + daemon logs |
| `Ctrl-B + 9` | **Security** | 2 | Security audit report + security overview |

#### Window 0: Dashboard
```
┌─────────────────────────┬──────────────────────┐
│   System Health         │  Providers & Nav     │
│   (auto-refresh)        │  Quick reference     │
├─────────────────────────┼──────────────────────┤
│   Agent Overview        │  Daemon Status       │
│   (auto-refresh)        │  (auto-refresh)      │
└─────────────────────────┴──────────────────────┘
```

#### Window 1: Chat
```
┌────────────────────────────────────────────────┐
│              Agent Chat REPL (80%)              │
│  Interactive agent session — type and chat     │
├────────────────────────────────────────────────┤
│           Session Info (20%)                    │
│  Model, workspace, commands, key bindings      │
└────────────────────────────────────────────────┘
```

### Workspace — Quick Dev Layout

```
┌────────────────────────────┬────────────────────┐
│                            │   Gateway Output   │
│    Agent REPL              │   (live)           │
│    (interactive chat)      ├────────────────────┤
│                            │   Health Monitor   │
│                            │   (auto-refresh)   │
├────────────────────────────┴────────────────────┤
│              Quick Commands Reference            │
└──────────────────────────────────────────────────┘
```

```bash
clawdesk tmux launch --layout workspace
# Aliases: ws, dev
```

### Monitor — Ops Dashboard

```
┌────────────────────────────┬────────────────────┐
│                            │  Channel Status    │
│    Gateway Health          │  (auto-refresh)    │
│    (auto-refresh)          ├────────────────────┤
│                            │  Daemon Logs       │
└────────────────────────────┴────────────────────┘
```

```bash
clawdesk tmux launch --layout monitor
# Aliases: mon, ops
```

### Chat — Focused Conversation

```
┌──────────────────────────────────────────────────┐
│              Agent Chat REPL (75%)               │
├──────────────────────────────────────────────────┤
│            Quick Commands (25%)                   │
└──────────────────────────────────────────────────┘
```

```bash
clawdesk tmux launch --layout chat
# Aliases: focus
```

## Commands Reference

| Command | Description |
|---------|-------------|
| `clawdesk tmux setup` | Interactive first-time setup wizard with layout selection |
| `clawdesk tmux launch` | Launch a tmux session (default: desktop layout) |
| `clawdesk tmux list` | List all active ClawDesk tmux sessions |
| `clawdesk tmux attach <session>` | Attach to an existing session |
| `clawdesk tmux kill <session>` | Kill a session |
| `clawdesk tmux layouts` | Show all available layouts with descriptions |
| `clawdesk tmux keys` | Show tmux key bindings cheat sheet |

### Launch Options

```bash
clawdesk tmux launch [OPTIONS]

Options:
  -l, --layout <LAYOUT>      Layout: desktop, workspace, monitor, chat [default: desktop]
  -s, --session <NAME>       Session name [default: clawdesk]
  -w, --workspace <DIR>      Working directory for agent panes
  -m, --model <MODEL>        Default model for agent sessions
      --no-attach            Create session without attaching
```

## Onboarding Flow

Running `clawdesk tmux setup` walks you through:

```
Step 1  →  Dependency check (tmux, cargo, curl, watch)
Step 2  →  Provider API key setup (Anthropic, OpenAI, Gemini, Ollama)
Step 3  →  Default model selection
Step 4  →  Channel configuration (Telegram, Discord, Slack)
Step 5  →  Layout selection (desktop / workspace / monitor / chat)
Step 6  →  Auto-launch the tmux session
```

If tmux is not installed, the wizard falls back to the standard `clawdesk init` setup.

## tmux Key Bindings

Mouse support is enabled in all layouts. Click to select panes, drag borders to resize, scroll with mouse wheel.

### Desktop Navigation (10 Screens)

| Key | Screen |
|-----|--------|
| `Ctrl-B + 0` | Dashboard |
| `Ctrl-B + 1` | Chat |
| `Ctrl-B + 2` | Sessions |
| `Ctrl-B + 3` | Agents |
| `Ctrl-B + 4` | Channels |
| `Ctrl-B + 5` | Memory |
| `Ctrl-B + 6` | Skills |
| `Ctrl-B + 7` | Settings |
| `Ctrl-B + 8` | Logs |
| `Ctrl-B + 9` | Security |

### General

| Key | Action |
|-----|--------|
| `Ctrl-B + n` / `p` | Next / previous screen |
| `Ctrl-B + d` | Detach (session stays alive in background) |
| `Ctrl-B + z` | Zoom/unzoom current pane (full screen toggle) |
| `Ctrl-B + arrow` | Switch between panes within a screen |
| `Ctrl-B + [` | Enter scroll/copy mode (`q` to exit) |
| `Ctrl-B + s` | Session picker (multiple sessions) |
| `Ctrl-B + w` | Window picker (all screens) |
| `Ctrl-B + x` | Kill current pane |
| `Ctrl-B + c` | Create a new window |

### Tips

- **Zoom any pane:** `Ctrl-B + z` toggles any pane to full screen. Great for focusing on agent chat or reading logs.
- **Detach & re-attach:** `Ctrl-B + d` detaches. Sessions persist. Re-attach with `clawdesk tmux attach`.
- **Multiple sessions:** Run different layouts in parallel:
  ```bash
  clawdesk tmux launch --layout desktop --session main
  clawdesk tmux launch --layout monitor --session ops
  ```

## Status Bar

```
 ClawDesk  │ 0:Dashboard  1:Chat  2:Sessions ... │  14:32  clawdesk
```

- **Left:** ClawDesk branding
- **Center:** Window tabs (current screen highlighted)
- **Right:** Clock + session name
- **Pane borders:** Labeled with pane purpose (System Health, Agent Chat, etc.)

Tokyo Night color theme for comfortable terminal aesthetics.

## Tauri vs tmux — Feature Comparison

| Feature | Tauri Desktop | tmux Desktop |
|---------|--------------|--------------|
| Agent chat | WebView with markdown | Agent REPL in terminal |
| 10 screens | React router | 10 tmux windows |
| Navigation | Sidebar clicks | `Ctrl-B + 0..9` |
| Live monitoring | React state | `watch` auto-refresh |
| Gateway logs | Console panel | Dedicated log window |
| Skill browsing | React list/detail | `watch` + CLI commands |
| Security audit | React dashboard | CLI audit report |
| Session management | WebView | CLI + tmux sessions |
| Persistent sessions | As long as app runs | `Ctrl-B + d` → persistent |
| Multiple sessions | Tabs in UI | Multiple tmux sessions |
| Mouse support | Full | Click/drag/scroll |

## Integration with CLI

All panes are standard terminals. Run any ClawDesk command in any pane:

```bash
# Agent interaction
clawdesk agent msg "explain this codebase"
clawdesk agent run --model gpt-4o

# Skill management
clawdesk skill list
clawdesk skill install code-review

# Configuration
clawdesk config set model claude-sonnet-4-20250514
clawdesk config backup

# Diagnostics
clawdesk doctor --verbose
clawdesk security audit --deep

# Daemon
clawdesk daemon start
clawdesk daemon status
```

## Troubleshooting

### "tmux is not installed"
```bash
brew install tmux        # macOS
sudo apt install tmux    # Debian/Ubuntu
sudo dnf install tmux    # Fedora
```

### "Session already exists"
The launcher offers to attach or replace. Manage manually:
```bash
clawdesk tmux list
clawdesk tmux kill clawdesk
clawdesk tmux launch
```

### Panes are too small
- Maximize your terminal window
- Use `Ctrl-B + z` to zoom a single pane
- The chat layout uses fewer panes for smaller terminals

### Gateway pane shows errors
Start the gateway first, or use `clawdesk tmux launch -l desktop` which starts it automatically in the Logs window.
