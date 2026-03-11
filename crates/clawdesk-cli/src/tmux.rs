//! # tmux session manager for ClawDesk
//!
//! Creates and manages tmux-based multi-window terminal layouts that mirror
//! the Tauri desktop experience. Each tmux window corresponds to a screen
//! in the desktop app, organized by the same sidebar groups:
//!
//! | Win | Group     | Tauri Screen   | tmux Panes                          |
//! |-----|-----------|----------------|-------------------------------------|
//! | 0   | MAIN      | Overview       | Health + Providers + Metrics + Nav  |
//! | 1   | MAIN      | Chat           | Agent REPL + Session Info           |
//! | 2   | CLUSTER   | A2A Directory  | A2A peers + Agent cards             |
//! | 3   | BUILD     | Skills         | Skill list + Skill detail           |
//! | 4   | BUILD     | Automations    | Cron jobs + Scheduled tasks         |
//! | 5   | WORKSPACE | Agents         | Agent list + Agent config           |
//! | 6   | WORKSPACE | Channels       | Channel status + Channel config     |
//! | 7   | WORKSPACE | Files          | File browser + File actions         |
//! | 8   | CONNECT   | Extensions     | Extension list + Extension detail   |
//! | 9   | CONNECT   | MCP            | MCP servers + MCP tools             |
//! | 10  | SYSTEM    | Settings       | Config view + Provider setup        |
//! | 11  | SYSTEM    | Logs           | Gateway logs + Daemon logs          |
//! | 12  | SYSTEM    | Local Models   | Ollama models + Download manager    |
//! | 13  | SYSTEM    | Documents      | RAG documents + Search              |
//! | 14  | SYSTEM    | Runtime        | Process tree + Resource usage       |
//!
//! Also supports three quick-start presets for users who want a simpler
//! layout: `workspace`, `monitor`, and `chat`.

use std::io::{self, Write};
use std::process::Command;

// ── Types ────────────────────────────────────────────────────

/// Tmux layout presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Full 15-window desktop experience (mirrors Tauri)
    Desktop,
    /// 4-pane development workspace
    Workspace,
    /// 3-pane monitoring dashboard
    Monitor,
    /// 2-pane focused chat
    Chat,
}

impl Layout {
    pub fn from_str(s: &str) -> Self {
        match s {
            "desktop" | "full" | "app" => Self::Desktop,
            "workspace" | "ws" | "dev" => Self::Workspace,
            "monitor" | "mon" | "ops" => Self::Monitor,
            "chat" | "focus" => Self::Chat,
            _ => Self::Desktop,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Desktop => "desktop",
            Self::Workspace => "workspace",
            Self::Monitor => "monitor",
            Self::Chat => "chat",
        }
    }
}

/// Configuration for a tmux session.
pub struct TmuxConfig {
    pub session_name: String,
    pub layout: Layout,
    pub gateway_url: String,
    pub model: Option<String>,
    pub workspace_dir: Option<String>,
    pub attach: bool,
}

impl Default for TmuxConfig {
    fn default() -> Self {
        Self {
            session_name: "clawdesk".to_string(),
            layout: Layout::Desktop,
            gateway_url: "http://127.0.0.1:18789".to_string(),
            model: None,
            workspace_dir: None,
            attach: true,
        }
    }
}

// ── Public API ───────────────────────────────────────────────

/// Check if tmux is installed and available.
pub fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Get the installed tmux version string.
pub fn tmux_version() -> Option<String> {
    Command::new("tmux")
        .arg("-V")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Check if a named tmux session already exists.
pub fn session_exists(name: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// List all running clawdesk tmux sessions.
pub fn list_sessions() -> Vec<String> {
    let output = Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output();

    match output {
        Ok(o) if o.status.success() => {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| l.starts_with("clawdesk"))
                .map(|l| l.to_string())
                .collect()
        }
        _ => vec![],
    }
}

/// Kill a named tmux session.
pub fn kill_session(name: &str) -> Result<(), String> {
    let status = Command::new("tmux")
        .args(["kill-session", "-t", name])
        .status()
        .map_err(|e| format!("failed to kill session: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("tmux kill-session failed for '{name}'"))
    }
}

/// Attach to an existing tmux session.
pub fn attach_session(name: &str) -> Result<(), String> {
    let status = Command::new("tmux")
        .args(["attach-session", "-t", name])
        .status()
        .map_err(|e| format!("failed to attach: {e}"))?;

    if !status.success() {
        return Err(format!("tmux attach failed for '{name}'"));
    }
    Ok(())
}

/// Launch a new tmux session with the given layout.
pub fn launch(config: &TmuxConfig) -> Result<(), String> {
    if !tmux_available() {
        return Err(
            "tmux is not installed. Install it with:\n  \
             macOS:  brew install tmux\n  \
             Linux:  sudo apt install tmux  /  sudo dnf install tmux\n  \
             See: https://github.com/tmux/tmux/wiki/Installing"
                .to_string(),
        );
    }

    // If the session already exists, offer to attach
    if session_exists(&config.session_name) {
        println!("  Session '{}' already exists.", config.session_name);
        print!("  Attach to it? [Y/n]: ");
        io::stdout().flush().ok();
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        if input.trim().is_empty()
            || input.trim().eq_ignore_ascii_case("y")
            || input.trim().eq_ignore_ascii_case("yes")
        {
            return attach_session(&config.session_name);
        } else {
            // Kill existing and create fresh
            kill_session(&config.session_name).ok();
        }
    }

    match config.layout {
        Layout::Desktop => launch_desktop(config),
        Layout::Workspace => launch_workspace(config),
        Layout::Monitor => launch_monitor(config),
        Layout::Chat => launch_chat(config),
    }
}

// ══════════════════════════════════════════════════════════════
// ── Desktop Layout (Tauri-equivalent, 15 windows) ────────────
// ══════════════════════════════════════════════════════════════

/// Full desktop experience: 15 tmux windows mirroring every Tauri app screen.
///
/// ```text
/// MAIN:      0:Overview  1:Chat
/// CLUSTER:   2:A2A
/// BUILD:     3:Skills  4:Automations
/// WORKSPACE: 5:Agents  6:Channels  7:Files
/// CONNECT:   8:Extensions  9:MCP
/// SYSTEM:   10:Settings  11:Logs  12:Models  13:Docs  14:Runtime
///
/// Navigate: Ctrl-B + 0..9  or  Ctrl-B + n/p
/// ```
fn launch_desktop(config: &TmuxConfig) -> Result<(), String> {
    let name = &config.session_name;
    let dir = config.workspace_dir.as_deref().unwrap_or(".");
    let clawdesk = find_clawdesk_binary();
    let _gw = &config.gateway_url;
    let model_flag = config
        .model
        .as_ref()
        .map(|m| format!(" --model {m}"))
        .unwrap_or_default();

    // ╔════════════════════════════════════════════════════════╗
    // ║  MAIN                                                 ║
    // ╚════════════════════════════════════════════════════════╝

    // ── Window 0: Overview ───────────────────────────────────
    // ┌─────────────────────┬────────────────────┐
    // │   System Health     │   Provider Status  │
    // ├─────────────────────┼────────────────────┤
    // │   Agent Overview    │   Quick Navigation │
    // └─────────────────────┴────────────────────┘

    tmux_cmd(&[
        "new-session", "-d", "-s", name, "-c", dir,
        "-x", "220", "-y", "55",
        "-n", "Overview",
    ])?;

        execute_payload(name, "Overview.0", &format!("watch -n5 -t '{clawdesk} doctor --verbose 2>/dev/null || echo \"Run: {clawdesk} gateway run\"'"))?;
    set_pane_title(name, "Overview.0", "System Health")?;

    tmux_cmd(&["split-window", "-h", "-t", &format!("{name}:Overview.0"), "-c", dir, "-p", "45"])?;
        execute_payload(name, "Overview.1", &format!("echo '┌─── Provider Status ───┐' && echo '' && \
                   {clawdesk} config get providers 2>/dev/null || echo '  No providers configured' && \
                   echo '' && echo '  Run: {clawdesk} init' && echo '' && \
                   echo '┌─── Navigation ────────┐' && echo '' && \
                   echo '  MAIN' && \
                   echo '    Ctrl-B + 0  Overview' && \
                   echo '    Ctrl-B + 1  Chat' && echo '' && \
                   echo '  CLUSTER' && \
                   echo '    Ctrl-B + 2  A2A Directory' && echo '' && \
                   echo '  BUILD' && \
                   echo '    Ctrl-B + 3  Skills' && \
                   echo '    Ctrl-B + 4  Automations' && echo '' && \
                   echo '  WORKSPACE' && \
                   echo '    Ctrl-B + 5  Agents' && \
                   echo '    Ctrl-B + 6  Channels' && \
                   echo '    Ctrl-B + 7  Files' && echo '' && \
                   echo '  CONNECT' && \
                   echo '    Ctrl-B + 8  Extensions' && \
                   echo '    Ctrl-B + 9  MCP' && echo '' && \
                   echo '  SYSTEM (Ctrl-B + n to scroll)' && \
                   echo '    10:Settings  11:Logs  12:Models' && \
                   echo '    13:Docs  14:Runtime' && echo '' && \
                   echo '  Ctrl-B + d  Detach (session stays alive)'"))?;
    set_pane_title(name, "Overview.1", "Providers & Navigation")?;

    tmux_cmd(&["split-window", "-v", "-t", &format!("{name}:Overview.0"), "-c", dir, "-p", "40"])?;
        execute_payload(name, "Overview.2", &format!("watch -n10 -t '{clawdesk} agent list 2>/dev/null || echo \"No agents registered\"'"))?;
    set_pane_title(name, "Overview.2", "Agents")?;

    tmux_cmd(&["split-window", "-v", "-t", &format!("{name}:Overview.1"), "-c", dir, "-p", "40"])?;
        execute_payload(name, "Overview.3", &format!("watch -n10 -t '{clawdesk} daemon status 2>/dev/null || echo \"Daemon not running\"'"))?;
    set_pane_title(name, "Overview.3", "Daemon Status")?;

    tmux_cmd(&["select-pane", "-t", &format!("{name}:Overview.0")])?;

    // ── Window 1: Chat ───────────────────────────────────────
    // ┌──────────────────────────────────────┐
    // │           Agent REPL (80%)           │
    // ├──────────────────────────────────────┤
    // │    Session / Model Info (20%)         │
    // └──────────────────────────────────────┘

    tmux_cmd(&["new-window", "-t", name, "-n", "Chat", "-c", dir])?;
        execute_payload(name, "Chat.0", &format!("{clawdesk} agent run --workspace {dir}{model_flag}"))?;
    set_pane_title(name, "Chat.0", "Agent Chat")?;

    tmux_cmd(&["split-window", "-v", "-t", &format!("{name}:Chat.0"), "-c", dir, "-p", "20"])?;
        execute_payload(name, "Chat.1", &format!("echo '──── Chat Session ────' && echo '' && \
                   echo '  Model: {}{model_flag}' && \
                   echo '  Workspace: {dir}' && echo '' && \
                   echo '  Commands:' && \
                   echo '    /help     — show agent commands' && \
                   echo '    /model    — switch model' && \
                   echo '    /clear    — clear history' && \
                   echo '    /skills   — list available skills' && \
                   echo '    /memory   — search memory' && \
                   echo '    /exit     — end session' && echo '' && \
                   echo '  Tips:' && \
                   echo '    Ctrl-B + z  = zoom chat pane full screen' && \
                   echo '    Ctrl-B + 0  = back to Overview'",
                   config.model.as_deref().unwrap_or("default")))?;
    set_pane_title(name, "Chat.1", "Session Info")?;
    tmux_cmd(&["select-pane", "-t", &format!("{name}:Chat.0")])?;

    // ╔════════════════════════════════════════════════════════╗
    // ║  CLUSTER                                              ║
    // ╚════════════════════════════════════════════════════════╝

    // ── Window 2: A2A Directory ──────────────────────────────
    // ┌─────────────────────┬────────────────────┐
    // │   A2A Peers         │   Agent Card       │
    // ├─────────────────────┴────────────────────┤
    // │         Discovery & Connection            │
    // └──────────────────────────────────────────┘

    tmux_cmd(&["new-window", "-t", name, "-n", "A2A", "-c", dir])?;
        execute_payload(name, "A2A.0", &format!("echo '──── A2A Directory ────' && echo '' && \
                   echo '  Agent-to-Agent protocol for multi-agent collaboration.' && echo '' && \
                   echo '  Registered peers:' && \
                   {clawdesk} a2a list 2>/dev/null || echo '    (none discovered yet)' && echo '' && \
                   echo '  Discover:  {clawdesk} a2a discover' && \
                   echo '  Register:  {clawdesk} a2a register <url>' && \
                   echo '  Agent card: {clawdesk} a2a card' && echo ''"))?;
    set_pane_title(name, "A2A.0", "Peers")?;

    tmux_cmd(&["split-window", "-h", "-t", &format!("{name}:A2A.0"), "-c", dir, "-p", "50"])?;
        execute_payload(name, "A2A.1", &format!("echo '──── Agent Card ────' && echo '' && \
                   echo '  Your agent card (/.well-known/agent.json):' && echo '' && \
                   {clawdesk} a2a card --json 2>/dev/null || \
                   echo '  Not generated yet. Run:' && \
                   echo '    {clawdesk} a2a card --generate' && echo '' && \
                   echo '  The card describes:' && \
                   echo '    • Agent name, description, version' && \
                   echo '    • Supported skills and capabilities' && \
                   echo '    • A2A protocol endpoint URL' && \
                   echo '    • Authentication requirements' && echo ''"))?;
    set_pane_title(name, "A2A.1", "Agent Card")?;

    tmux_cmd(&["split-window", "-v", "-t", &format!("{name}:A2A.0"), "-c", dir, "-p", "30"])?;
        execute_payload(name, "A2A.2", &format!("echo '  A2A uses Google Agent-to-Agent protocol.' && \
                   echo '  Try: {clawdesk} a2a discover --network local'"))?;
    set_pane_title(name, "A2A.2", "Discovery")?;

    // ╔════════════════════════════════════════════════════════╗
    // ║  BUILD                                                ║
    // ╚════════════════════════════════════════════════════════╝

    // ── Window 3: Skills ─────────────────────────────────────
    // ┌─────────────────────┬────────────────────┐
    // │   Skill List        │   Skill Detail     │
    // ├─────────────────────┴────────────────────┤
    // │         Skill Actions                     │
    // └──────────────────────────────────────────┘

    tmux_cmd(&["new-window", "-t", name, "-n", "Skills", "-c", dir])?;
        execute_payload(name, "Skills.0", &format!("watch -n15 -t '{clawdesk} skill list 2>/dev/null || echo \"Skill registry empty\"'"))?;
    set_pane_title(name, "Skills.0", "Registry")?;

    tmux_cmd(&["split-window", "-h", "-t", &format!("{name}:Skills.0"), "-c", dir, "-p", "50"])?;
        execute_payload(name, "Skills.1", &format!("echo '──── Skill Management ────' && echo '' && \
                   echo '  15+ built-in skills: file ops, shell, browser,' && \
                   echo '  memory, cron, code review, research, etc.' && echo '' && \
                   echo '  Commands:' && \
                   echo '    {clawdesk} skill list            — browse' && \
                   echo '    {clawdesk} skill search <query>  — search registry' && \
                   echo '    {clawdesk} skill install <name>  — install' && \
                   echo '    {clawdesk} skill create <name>   — scaffold new' && \
                   echo '    {clawdesk} skill lint            — validate' && \
                   echo '    {clawdesk} skill test            — dry-run' && \
                   echo '    {clawdesk} skill audit           — security check' && echo '' && \
                   echo '  Workspace skills: drop SKILL.md in your project' && echo ''"))?;
    set_pane_title(name, "Skills.1", "Detail")?;

    tmux_cmd(&["split-window", "-v", "-t", &format!("{name}:Skills.0"), "-c", dir, "-p", "25"])?;
        execute_payload(name, "Skills.2", &format!("echo '  Try: {clawdesk} skill search code-review'"))?;
    set_pane_title(name, "Skills.2", "Actions")?;

    // ── Window 4: Automations ────────────────────────────────
    // ┌─────────────────────┬────────────────────┐
    // │   Scheduled Jobs    │   Job Detail       │
    // └─────────────────────┴────────────────────┘

    tmux_cmd(&["new-window", "-t", name, "-n", "Automations", "-c", dir])?;
        execute_payload(name, "Automations.0", &format!("watch -n10 -t '{clawdesk} cron list 2>/dev/null || echo \"No scheduled jobs\"'"))?;
    set_pane_title(name, "Automations.0", "Scheduled Jobs")?;

    tmux_cmd(&["split-window", "-h", "-t", &format!("{name}:Automations.0"), "-c", dir, "-p", "50"])?;
        execute_payload(name, "Automations.1", &format!("echo '──── Automations ────' && echo '' && \
                   echo '  Schedule recurring agent tasks with cron syntax.' && echo '' && \
                   echo '  Commands:' && \
                   echo '    {clawdesk} cron list              — view jobs' && \
                   echo '    {clawdesk} cron add <expr> <cmd>  — create job' && \
                   echo '    {clawdesk} cron remove <id>       — delete job' && \
                   echo '    {clawdesk} cron run <id>          — run now' && \
                   echo '    {clawdesk} cron pause <id>        — pause job' && echo '' && \
                   echo '  Examples:' && \
                   echo '    \"0 9 * * *\"  \"summarize daily emails\"' && \
                   echo '    \"*/30 * * * *\"  \"check channel health\"' && \
                   echo '    \"0 0 * * 0\"  \"weekly security audit\"' && echo ''"))?;
    set_pane_title(name, "Automations.1", "Job Management")?;

    // ╔════════════════════════════════════════════════════════╗
    // ║  WORKSPACE                                            ║
    // ╚════════════════════════════════════════════════════════╝

    // ── Window 5: Agents ─────────────────────────────────────
    // ┌─────────────────────┬────────────────────┐
    // │   Agent Registry    │   Agent Config     │
    // ├─────────────────────┴────────────────────┤
    // │         Agent Actions                     │
    // └──────────────────────────────────────────┘

    tmux_cmd(&["new-window", "-t", name, "-n", "Agents", "-c", dir])?;
        execute_payload(name, "Agents.0", &format!("watch -n10 -t '{clawdesk} agent list --bindings 2>/dev/null || echo \"No agents\"'"))?;
    set_pane_title(name, "Agents.0", "Registry")?;

    tmux_cmd(&["split-window", "-h", "-t", &format!("{name}:Agents.0"), "-c", dir, "-p", "50"])?;
        execute_payload(name, "Agents.1", &format!("echo '──── Agent Management ────' && echo '' && \
                   echo '  Create:    {clawdesk} agent add <id>' && \
                   echo '  Validate:  {clawdesk} agent validate' && \
                   echo '  Reload:    {clawdesk} agent apply' && \
                   echo '  Export:    {clawdesk} agent export <id>' && echo '' && \
                   echo '  Team mode: {clawdesk} agent run --team-dir ./agents/' && echo '' && \
                   echo '  Each agent is defined by a TOML file.' && \
                   echo '  Drop .toml files in ~/.clawdesk/agents/' && echo ''"))?;
    set_pane_title(name, "Agents.1", "Management")?;

    tmux_cmd(&["split-window", "-v", "-t", &format!("{name}:Agents.0"), "-c", dir, "-p", "30"])?;
        execute_payload(name, "Agents.2", &format!("echo '  Try: {clawdesk} agent add my-agent'"))?;
    set_pane_title(name, "Agents.2", "Actions")?;

    // ── Window 6: Channels ───────────────────────────────────
    // ┌─────────────────────┬────────────────────┐
    // │   Channel Status    │   Channel Config   │
    // └─────────────────────┴────────────────────┘

    tmux_cmd(&["new-window", "-t", name, "-n", "Channels", "-c", dir])?;
        execute_payload(name, "Channels.0", &format!("watch -n8 -t '{clawdesk} channels status --probe 2>/dev/null || echo \"No channels configured\"'"))?;
    set_pane_title(name, "Channels.0", "Status")?;

    tmux_cmd(&["split-window", "-h", "-t", &format!("{name}:Channels.0"), "-c", dir, "-p", "50"])?;
        execute_payload(name, "Channels.1", &format!("echo '──── Channel Configuration ────' && echo '' && \
                   echo '  Supported channels:' && \
                   echo '    Telegram, Discord, Slack, WhatsApp,' && \
                   echo '    Signal, Matrix, Email, IRC, Teams,' && \
                   echo '    iMessage, Mastodon, Nostr, Twitch,' && \
                   echo '    Line, Lark, Mattermost, Webchat' && echo '' && \
                   echo '  Configure: {clawdesk} init  (step 4)' && \
                   echo '  Or edit:   ~/.clawdesk/channels.json' && echo ''"))?;
    set_pane_title(name, "Channels.1", "Config")?;

    // ── Window 7: Files ──────────────────────────────────────
    // ┌─────────────────────┬────────────────────┐
    // │   File Browser      │   File Actions     │
    // └─────────────────────┴────────────────────┘

    tmux_cmd(&["new-window", "-t", name, "-n", "Files", "-c", dir])?;
        execute_payload(name, "Files.0", &format!("echo '──── Workspace Files ────' && echo '' && ls -la {dir} && echo '' && \
                   echo '  Storage root: ~/.clawdesk/' && \
                   echo '  Workspace:    {dir}' && echo ''"))?;
    set_pane_title(name, "Files.0", "Browser")?;

    tmux_cmd(&["split-window", "-h", "-t", &format!("{name}:Files.0"), "-c", dir, "-p", "45"])?;
        execute_payload(name, "Files.1", &format!("echo '──── File Actions ────' && echo '' && \
                   echo '  The agent can read, write, and manage files' && \
                   echo '  within the workspace scope.' && echo '' && \
                   echo '  Skills:' && \
                   echo '    file_read   — read file contents' && \
                   echo '    file_write  — create/edit files' && \
                   echo '    file_search — find files by pattern' && \
                   echo '    file_delete — remove files (confirmed)' && echo '' && \
                   echo '  Upload files: {clawdesk} upload <path>' && \
                   echo '  Browse:       {clawdesk} files list' && echo '' && \
                   echo '  Config dir layout:' && \
                   echo '    ~/.clawdesk/agents/     Agent definitions' && \
                   echo '    ~/.clawdesk/sochdb/     Vector database' && \
                   echo '    ~/.clawdesk/extensions/ Installed extensions' && echo ''"))?;
    set_pane_title(name, "Files.1", "Actions")?;

    // ╔════════════════════════════════════════════════════════╗
    // ║  CONNECT                                              ║
    // ╚════════════════════════════════════════════════════════╝

    // ── Window 8: Extensions ─────────────────────────────────
    // ┌─────────────────────┬────────────────────┐
    // │   Extension List    │   Extension Detail  │
    // └─────────────────────┴────────────────────┘

    tmux_cmd(&["new-window", "-t", name, "-n", "Extensions", "-c", dir])?;
        execute_payload(name, "Extensions.0", &format!("watch -n15 -t '{clawdesk} extension list 2>/dev/null || echo \"No extensions installed\"'"))?;
    set_pane_title(name, "Extensions.0", "Installed")?;

    tmux_cmd(&["split-window", "-h", "-t", &format!("{name}:Extensions.0"), "-c", dir, "-p", "50"])?;
        execute_payload(name, "Extensions.1", &format!("echo '──── Extensions ────' && echo '' && \
                   echo '  Extend ClawDesk with WASM plugins.' && echo '' && \
                   echo '  Commands:' && \
                   echo '    {clawdesk} extension list      — browse installed' && \
                   echo '    {clawdesk} extension search    — find in registry' && \
                   echo '    {clawdesk} extension install   — add extension' && \
                   echo '    {clawdesk} extension remove    — uninstall' && \
                   echo '    {clawdesk} extension create    — scaffold new' && echo '' && \
                   echo '  Extensions run in a capability-sandboxed WASM' && \
                   echo '  runtime with explicit permission grants.' && echo '' && \
                   echo '  Dir: ~/.clawdesk/extensions/' && echo ''"))?;
    set_pane_title(name, "Extensions.1", "Management")?;

    // ── Window 9: MCP ────────────────────────────────────────
    // ┌─────────────────────┬────────────────────┐
    // │   MCP Servers       │   MCP Tools        │
    // └─────────────────────┴────────────────────┘

    tmux_cmd(&["new-window", "-t", name, "-n", "MCP", "-c", dir])?;
        execute_payload(name, "MCP.0", &format!("watch -n10 -t '{clawdesk} mcp list 2>/dev/null || echo \"No MCP servers connected\"'"))?;
    set_pane_title(name, "MCP.0", "Servers")?;

    tmux_cmd(&["split-window", "-h", "-t", &format!("{name}:MCP.0"), "-c", dir, "-p", "50"])?;
        execute_payload(name, "MCP.1", &format!("echo '──── Model Context Protocol ────' && echo '' && \
                   echo '  MCP connects agents to external tool servers.' && echo '' && \
                   echo '  Commands:' && \
                   echo '    {clawdesk} mcp list           — list servers' && \
                   echo '    {clawdesk} mcp connect <url>  — connect server' && \
                   echo '    {clawdesk} mcp tools          — list all tools' && \
                   echo '    {clawdesk} mcp call <tool>    — invoke tool' && \
                   echo '    {clawdesk} mcp disconnect     — remove server' && echo '' && \
                   echo '  Servers provide tools, prompts, and resources' && \
                   echo '  that agents can use during conversations.' && echo '' && \
                   echo '  Config: ~/.clawdesk/mcp.json' && echo ''"))?;
    set_pane_title(name, "MCP.1", "Tools")?;

    // ╔════════════════════════════════════════════════════════╗
    // ║  SYSTEM                                               ║
    // ╚════════════════════════════════════════════════════════╝

    // ── Window 10: Settings ──────────────────────────────────
    // ┌─────────────────────┬────────────────────┐
    // │   Current Config    │   Provider Setup   │
    // └─────────────────────┴────────────────────┘

    tmux_cmd(&["new-window", "-t", name, "-n", "Settings", "-c", dir])?;
        execute_payload(name, "Settings.0", &format!("echo '──── Configuration ────' && echo '' && \
                   echo '  View:   {clawdesk} config get <key>' && \
                   echo '  Set:    {clawdesk} config set <key> <value>' && \
                   echo '  Backup: {clawdesk} config backup' && \
                   echo '  Restore:{clawdesk} config restore <file>' && echo '' && \
                   echo '  Common keys:' && \
                   echo '    model           — default LLM model' && \
                   echo '    providers       — configured providers' && \
                   echo '    gateway.port    — gateway port' && \
                   echo '    gateway.host    — gateway bind address' && echo '' && \
                   echo '  Data dir: ~/.clawdesk/' && \
                   echo '  Re-run setup: {clawdesk} init' && echo ''"))?;
    set_pane_title(name, "Settings.0", "Config")?;

    tmux_cmd(&["split-window", "-h", "-t", &format!("{name}:Settings.0"), "-c", dir, "-p", "50"])?;
        execute_payload(name, "Settings.1", &format!("echo '──── Provider Setup ────' && echo '' && \
                   echo '  Providers (8 supported):' && \
                   echo '    Anthropic  — claude-sonnet-4, claude-haiku-4.5' && \
                   echo '    OpenAI     — gpt-4o, gpt-4o-mini' && \
                   echo '    Gemini     — gemini-2.0-flash, gemini-2.5-pro' && \
                   echo '    Ollama     — llama3.2, mistral (local)' && \
                   echo '    Azure      — Azure OpenAI endpoints' && \
                   echo '    Bedrock    — AWS Bedrock models' && \
                   echo '    Cohere     — command-r-plus' && \
                   echo '    Vertex     — Google Vertex AI' && echo '' && \
                   echo '  Set API key:' && \
                   echo '    export ANTHROPIC_API_KEY=\"sk-ant-...\"' && \
                   echo '    export OPENAI_API_KEY=\"sk-...\"' && echo '' && \
                   echo '  Or use: {clawdesk} login' && echo ''"))?;
    set_pane_title(name, "Settings.1", "Providers")?;

    // ── Window 11: Logs ──────────────────────────────────────
    // ┌──────────────────────────────────────┐
    // │        Gateway Logs (70%)             │
    // ├──────────────────────────────────────┤
    // │        Daemon Logs (30%)              │
    // └──────────────────────────────────────┘

    tmux_cmd(&["new-window", "-t", name, "-n", "Logs", "-c", dir])?;
        execute_payload(name, "Logs.0", &format!("{clawdesk} gateway run --port 18789 2>&1"))?;
    set_pane_title(name, "Logs.0", "Gateway Output")?;

    tmux_cmd(&["split-window", "-v", "-t", &format!("{name}:Logs.0"), "-c", dir, "-p", "30"])?;
        execute_payload(name, "Logs.1", &format!("{clawdesk} daemon logs -n 100 2>/dev/null || echo 'Daemon not running. Start with: {clawdesk} daemon start'"))?;
    set_pane_title(name, "Logs.1", "Daemon")?;

    // ── Window 12: Local Models ──────────────────────────────
    // ┌─────────────────────┬────────────────────┐
    // │   Installed Models  │   Download Manager │
    // └─────────────────────┴────────────────────┘

    tmux_cmd(&["new-window", "-t", name, "-n", "Models", "-c", dir])?;
        execute_payload(name, "Models.0", &format!("watch -n15 -t '{clawdesk} models list 2>/dev/null || \
                   ollama list 2>/dev/null || \
                   echo \"No local model runtime found.\" && \
                   echo \"Install Ollama: curl -fsSL https://ollama.ai/install.sh | sh\"'"))?;
    set_pane_title(name, "Models.0", "Installed Models")?;

    tmux_cmd(&["split-window", "-h", "-t", &format!("{name}:Models.0"), "-c", dir, "-p", "50"])?;
        execute_payload(name, "Models.1", &format!("echo '──── Local Models ────' && echo '' && \
                   echo '  Run models locally with Ollama or llama.cpp.' && echo '' && \
                   echo '  Commands:' && \
                   echo '    {clawdesk} models list           — installed models' && \
                   echo '    {clawdesk} models pull <name>    — download model' && \
                   echo '    {clawdesk} models serve <name>   — start serving' && \
                   echo '    {clawdesk} models remove <name>  — delete model' && echo '' && \
                   echo '  Popular models:' && \
                   echo '    llama3.2     — Meta Llama 3B/8B' && \
                   echo '    mistral      — Mistral 7B' && \
                   echo '    codellama    — Code Llama 7B/13B' && \
                   echo '    deepseek-r1  — DeepSeek R1' && \
                   echo '    phi4         — Microsoft Phi-4' && echo '' && \
                   echo '  Ollama: ollama pull llama3.2' && echo ''"))?;
    set_pane_title(name, "Models.1", "Download Manager")?;

    // ── Window 13: Documents ─────────────────────────────────
    // ┌─────────────────────┬────────────────────┐
    // │   Document Index    │   Search / RAG     │
    // └─────────────────────┴────────────────────┘

    tmux_cmd(&["new-window", "-t", name, "-n", "Docs", "-c", dir])?;
        execute_payload(name, "Docs.0", &format!("echo '──── Documents ────' && echo '' && \
                   echo '  RAG (Retrieval-Augmented Generation) document store.' && echo '' && \
                   echo '  Manage:' && \
                   echo '    {clawdesk} docs index <path>   — index documents' && \
                   echo '    {clawdesk} docs list           — list indexed' && \
                   echo '    {clawdesk} docs search <query> — search' && \
                   echo '    {clawdesk} docs remove <id>    — remove' && echo '' && \
                   echo '  Supported formats:' && \
                   echo '    .md, .txt, .pdf, .html, .json, .csv,' && \
                   echo '    .rs, .py, .js, .ts, .go, .java, ...' && echo '' && \
                   echo '  Indexed documents are chunked, embedded, and' && \
                   echo '  stored in SochDB for vector + BM25 search.' && echo ''"))?;
    set_pane_title(name, "Docs.0", "Document Index")?;

    tmux_cmd(&["split-window", "-h", "-t", &format!("{name}:Docs.0"), "-c", dir, "-p", "50"])?;
        execute_payload(name, "Docs.1", &format!("echo '──── Memory & Search ────' && echo '' && \
                   echo '  Hybrid search pipeline:' && \
                   echo '    1. HNSW vector similarity (cosine)' && \
                   echo '    2. BM25 full-text search' && \
                   echo '    3. Reciprocal Rank Fusion (RRF)' && \
                   echo '    4. Temporal decay weighting' && \
                   echo '    5. MMR diversity filtering' && echo '' && \
                   echo '  Embedding providers:' && \
                   echo '    OpenAI, Cohere, Voyage, Ollama,' && \
                   echo '    HuggingFace (with FTS fallback)' && echo '' && \
                   echo '  Database: SochDB @ ~/.clawdesk/sochdb/' && \
                   echo '  Config:   {clawdesk} config get memory' && echo ''"))?;
    set_pane_title(name, "Docs.1", "Search & RAG")?;

    // ── Window 14: Runtime ───────────────────────────────────
    // ┌─────────────────────┬────────────────────┐
    // │   Process Tree      │   Resource Usage   │
    // ├─────────────────────┴────────────────────┤
    // │         Security Audit                    │
    // └──────────────────────────────────────────┘

    tmux_cmd(&["new-window", "-t", name, "-n", "Runtime", "-c", dir])?;
        execute_payload(name, "Runtime.0", &format!("watch -n5 -t '{clawdesk} daemon status --verbose 2>/dev/null || echo \"Daemon not running\"'"))?;
    set_pane_title(name, "Runtime.0", "Process Status")?;

    tmux_cmd(&["split-window", "-h", "-t", &format!("{name}:Runtime.0"), "-c", dir, "-p", "50"])?;
        execute_payload(name, "Runtime.1", &format!("watch -n3 -t 'echo \"──── Resource Usage ────\" && echo \"\" && \
                   ps aux | head -1 && \
                   ps aux | grep -E \"(clawdesk|ollama|llama)\" | grep -v grep 2>/dev/null || \
                   echo \"  No ClawDesk processes running\" && \
                   echo \"\" && echo \"──── Memory ────\" && \
                   {clawdesk} doctor 2>/dev/null | grep -i -E \"(memory|disk|cpu)\" || echo \"  Run gateway for diagnostics\"'"))?;
    set_pane_title(name, "Runtime.1", "Resources")?;

    tmux_cmd(&["split-window", "-v", "-t", &format!("{name}:Runtime.0"), "-c", dir, "-p", "35"])?;
        execute_payload(name, "Runtime.2", &format!("{clawdesk} security audit 2>/dev/null || \
                   echo '──── Security ────' && \
                   echo '' && \
                   echo '  Run: {clawdesk} security audit' && \
                   echo '  Deep: {clawdesk} security audit --deep' && \
                   echo '  Fix:  {clawdesk} security audit --fix'"))?;
    set_pane_title(name, "Runtime.2", "Security")?;

    // ── Global configuration ─────────────────────────────────

    configure_desktop_status_bar(name)?;
    configure_desktop_keybindings(name)?;

    // Select Overview window and first pane
    tmux_cmd(&["select-window", "-t", &format!("{name}:Overview")])?;
    tmux_cmd(&["select-pane", "-t", &format!("{name}:Overview.0")])?;

    // Print launch banner
    println!();
    println!("  ┌─── ClawDesk Desktop (tmux) ──────────────────────────────┐");
    println!("  │                                                          │");
    println!("  │  15 screens — mirrors the Tauri desktop app:             │");
    println!("  │                                                          │");
    println!("  │  MAIN        0:Overview  1:Chat                          │");
    println!("  │  CLUSTER     2:A2A                                       │");
    println!("  │  BUILD       3:Skills    4:Automations                   │");
    println!("  │  WORKSPACE   5:Agents    6:Channels   7:Files            │");
    println!("  │  CONNECT     8:Extensions  9:MCP                         │");
    println!("  │  SYSTEM     10:Settings  11:Logs  12:Models              │");
    println!("  │             13:Docs  14:Runtime                          │");
    println!("  │                                                          │");
    println!("  │  Ctrl-B + 0..9  Jump to window   Ctrl-B + n/p  Scroll   │");
    println!("  │  Ctrl-B + d     Detach           Ctrl-B + z    Zoom     │");
    println!("  │                                                          │");
    println!("  └──────────────────────────────────────────────────────────┘");
    println!();

    if config.attach {
        attach_session(name)?;
    } else {
        println!("  Session '{}' created. Attach with:", name);
        println!("    tmux attach -t {}", name);
    }
    Ok(())
}

/// Configure the desktop-mode status bar with grouped window tabs.
fn configure_desktop_status_bar(session: &str) -> Result<(), String> {
    // General status style
    tmux_cmd(&["set-option", "-t", session, "status", "on"])?;
    tmux_cmd(&["set-option", "-t", session, "status-position", "bottom"])?;
    tmux_cmd(&["set-option", "-t", session, "status-style", "bg=#1a1b26,fg=#a9b1d6"])?;
    tmux_cmd(&["set-option", "-t", session, "status-left-length", "28"])?;
    tmux_cmd(&["set-option", "-t", session, "status-right-length", "60"])?;

    // Left: ClawDesk brand
    tmux_cmd(&[
        "set-option", "-t", session, "status-left",
        "#[bg=#7aa2f7,fg=#1a1b26,bold]  ClawDesk #[default] ",
    ])?;

    // Right: Window count + Clock + session
    tmux_cmd(&[
        "set-option", "-t", session, "status-right",
        " #[fg=#565f89]15 screens #[fg=#3b4261]│#[fg=#565f89] %H:%M #[fg=#7aa2f7,bold]#S#[default] ",
    ])?;

    // Window tabs styling (active screen highlighted like Tauri sidebar)
    tmux_cmd(&[
        "set-option", "-t", session, "window-status-format",
        " #[fg=#565f89]#I:#W ",
    ])?;
    tmux_cmd(&[
        "set-option", "-t", session, "window-status-current-format",
        "#[bg=#3b4261,fg=#7aa2f7,bold] #I:#W #[default]",
    ])?;
    tmux_cmd(&["set-option", "-t", session, "window-status-separator", ""])?;

    // Pane borders
    tmux_cmd(&["set-option", "-t", session, "pane-border-style", "fg=#3b4261"])?;
    tmux_cmd(&["set-option", "-t", session, "pane-active-border-style", "fg=#7aa2f7"])?;
    tmux_cmd(&["set-option", "-t", session, "pane-border-status", "top"])?;
    tmux_cmd(&["set-option", "-t", session, "pane-border-format",
        " #[fg=#7aa2f7,bold]#{pane_title} #[default]"])?;

    // Mouse support
    tmux_cmd(&["set-option", "-t", session, "mouse", "on"])?;

    // Base index 0 so Ctrl-B + 0..9 maps to windows 0..9
    tmux_cmd(&["set-option", "-t", session, "base-index", "0"])?;
    tmux_cmd(&["set-option", "-t", session, "pane-base-index", "0"])?;

    // Allow renumbering for consistency
    tmux_cmd(&["set-option", "-t", session, "renumber-windows", "off"])?;

    Ok(())
}

/// Configure desktop keybindings for quick navigation across 15 windows.
fn configure_desktop_keybindings(session: &str) -> Result<(), String> {
    // Alt+number bindings as alternative navigation for 0..9
    for i in 0..=9 {
        let _ = tmux_cmd(&[
            "bind-key", "-t", session, "-n", &format!("M-{i}"),
            "select-window", "-t", &format!(":{i}"),
        ]);
    }
    // Alt+F1..F5 for windows 10..14 (beyond digit keys)
    let extended = [
        ("F1", "10"),  // Settings
        ("F2", "11"),  // Logs
        ("F3", "12"),  // Models
        ("F4", "13"),  // Docs
        ("F5", "14"),  // Runtime
    ];
    for (key, win) in &extended {
        let _ = tmux_cmd(&[
            "bind-key", "-t", session, "-n", &format!("M-{key}"),
            "select-window", "-t", &format!(":{win}"),
        ]);
    }
    Ok(())
}

// ══════════════════════════════════════════════════════════════
// ── Quick-start preset layouts ───────────────────────────────
// ══════════════════════════════════════════════════════════════

/// 4-pane workspace layout:
/// ```text
/// ┌────────────────────┬──────────────────┐
/// │                    │   Gateway Logs   │
/// │   Agent REPL       │                  │
/// │                    ├──────────────────┤
/// │                    │   Health Monitor │
/// ├────────────────────┴──────────────────┤
/// │            Quick Commands             │
/// └───────────────────────────────────────┘
/// ```
fn launch_workspace(config: &TmuxConfig) -> Result<(), String> {
    let name = &config.session_name;
    let dir = config.workspace_dir.as_deref().unwrap_or(".");
    let clawdesk = find_clawdesk_binary();
    let model_flag = config
        .model
        .as_ref()
        .map(|m| format!(" --model {m}"))
        .unwrap_or_default();

    tmux_cmd(&[
        "new-session", "-d", "-s", name, "-c", dir,
        "-x", "220", "-y", "55",
    ])?;

        execute_payload(name, "0.0", &format!("{clawdesk} agent run --workspace {dir}{model_flag}"))?;
    set_pane_title(name, "0.0", "Agent REPL")?;

    tmux_cmd(&["split-window", "-h", "-t", &format!("{name}:0.0"), "-c", dir, "-p", "40"])?;
        execute_payload(name, "0.1", &format!("{clawdesk} gateway run --port 18789 2>&1"))?;
    set_pane_title(name, "0.1", "Gateway")?;

    tmux_cmd(&["split-window", "-v", "-t", &format!("{name}:0.1"), "-c", dir, "-p", "35"])?;
        execute_payload(name, "0.2", &format!("watch -n5 '{clawdesk} doctor 2>/dev/null || echo \"Gateway not running\"'"))?;
    set_pane_title(name, "0.2", "Health")?;

    tmux_cmd(&["split-window", "-v", "-t", &format!("{name}:0.0"), "-c", dir, "-p", "20"])?;
        execute_payload(name, "0.3", &format!("echo '──── Quick Commands ────' && echo '' && \
                   echo '  {clawdesk} agent msg \"hello\"         — send message' && \
                   echo '  {clawdesk} skill list                — list skills' && \
                   echo '  {clawdesk} channels status           — channel health' && \
                   echo '  {clawdesk} doctor                    — diagnostics' && \
                   echo '  {clawdesk} security audit            — security scan' && \
                   echo '  {clawdesk} tmux launch -l desktop    — full 10-screen layout' && echo ''"))?;
    set_pane_title(name, "0.3", "Commands")?;

    configure_status_bar(name, "workspace")?;
    tmux_cmd(&["select-pane", "-t", &format!("{name}:0.0")])?;

    if config.attach {
        attach_session(name)?;
    } else {
        println!("  Session '{}' created. Attach with:", name);
        println!("    tmux attach -t {}", name);
    }
    Ok(())
}

/// 3-pane monitoring layout.
fn launch_monitor(config: &TmuxConfig) -> Result<(), String> {
    let name = &config.session_name;
    let dir = config.workspace_dir.as_deref().unwrap_or(".");
    let clawdesk = find_clawdesk_binary();

    tmux_cmd(&[
        "new-session", "-d", "-s", name, "-c", dir,
        "-x", "220", "-y", "55",
    ])?;

        execute_payload(name, "0.0", &format!("watch -n3 '{clawdesk} doctor --verbose 2>/dev/null || echo \"Waiting for gateway...\"'"))?;
    set_pane_title(name, "0.0", "Health")?;

    tmux_cmd(&["split-window", "-h", "-t", &format!("{name}:0.0"), "-c", dir, "-p", "45"])?;
        execute_payload(name, "0.1", &format!("watch -n5 '{clawdesk} channels status --probe 2>/dev/null || echo \"No channels\"'"))?;
    set_pane_title(name, "0.1", "Channels")?;

    tmux_cmd(&["split-window", "-v", "-t", &format!("{name}:0.1"), "-c", dir, "-p", "50"])?;
        execute_payload(name, "0.2", &format!("{clawdesk} daemon logs -n 100 2>/dev/null || echo 'Daemon not running — start with: {clawdesk} daemon start'"))?;
    set_pane_title(name, "0.2", "Logs")?;

    configure_status_bar(name, "monitor")?;
    tmux_cmd(&["select-pane", "-t", &format!("{name}:0.0")])?;

    if config.attach {
        attach_session(name)?;
    } else {
        println!("  Session '{}' created. Attach with:", name);
        println!("    tmux attach -t {}", name);
    }
    Ok(())
}

/// 2-pane chat-focused layout.
fn launch_chat(config: &TmuxConfig) -> Result<(), String> {
    let name = &config.session_name;
    let dir = config.workspace_dir.as_deref().unwrap_or(".");
    let clawdesk = find_clawdesk_binary();
    let model_flag = config
        .model
        .as_ref()
        .map(|m| format!(" --model {m}"))
        .unwrap_or_default();

    tmux_cmd(&[
        "new-session", "-d", "-s", name, "-c", dir,
        "-x", "220", "-y", "55",
    ])?;

        execute_payload(name, "0.0", &format!("{clawdesk} agent run --workspace {dir}{model_flag}"))?;
    set_pane_title(name, "0.0", "Chat")?;

    tmux_cmd(&["split-window", "-v", "-t", &format!("{name}:0.0"), "-c", dir, "-p", "25"])?;
        execute_payload(name, "0.1", &format!("echo '──── Chat ────  Ctrl-B + z = zoom  |  Ctrl-B + d = detach'"))?;
    set_pane_title(name, "0.1", "Commands")?;

    configure_status_bar(name, "chat")?;
    tmux_cmd(&["select-pane", "-t", &format!("{name}:0.0")])?;

    if config.attach {
        attach_session(name)?;
    } else {
        println!("  Session '{}' created. Attach with:", name);
        println!("    tmux attach -t {}", name);
    }
    Ok(())
}

// ── Onboarding ───────────────────────────────────────────────

/// Run the tmux-integrated onboarding wizard.
pub fn launch_onboarding(config: &TmuxConfig) -> Result<(), String> {
    let name = format!("{}-setup", config.session_name);
    let clawdesk = find_clawdesk_binary();
    let dir = config.workspace_dir.as_deref().unwrap_or(".");

    if session_exists(&name) {
        kill_session(&name).ok();
    }

    tmux_cmd(&[
        "new-session", "-d", "-s", &name, "-c", dir,
        "-x", "120", "-y", "40",
    ])?;

    let _layout_name = config.layout.name();
    let session_name = &config.session_name;
        execute_payload(&name, "0.0", &format!(
            "{clawdesk} tmux setup --session {session_name}"
        ))?;

    configure_status_bar(&name, "setup")?;

    if config.attach {
        attach_session(&name)?;
    }
    Ok(())
}

// ── Helpers ──────────────────────────────────────────────────


fn execute_payload(session: &str, pane: &str, payload: &str) -> Result<(), String> {
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_micros();
    // Clean up the pane identifier for safety
    let safe_pane = pane.replace(":", "_").replace(".", "_");
    let path = std::env::temp_dir().join(format!("cdesk_{}_{}.sh", ts, safe_pane));
    std::fs::write(&path, format!("clear\n{}", payload)).unwrap();
    tmux_cmd(&[
        "send-keys", "-t", &format!("{}:{}", session, pane),
        &format!("sh '{}'; rm -f '{}'\n", path.display(), path.display()),
    ])
}

fn tmux_cmd(args: &[&str]) -> Result<(), String> {
    let status = Command::new("tmux")
        .args(args)
        .status()
        .map_err(|e| format!("tmux command failed: {e}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(format!("tmux {} failed", args.first().unwrap_or(&"?")))
    }
}

fn set_pane_title(session: &str, pane: &str, title: &str) -> Result<(), String> {
    tmux_cmd(&[
        "select-pane", "-t", &format!("{session}:{pane}"),
        "-T", title,
    ])
}

/// Configure the preset-mode status bar (workspace/monitor/chat/setup).
fn configure_status_bar(session: &str, mode: &str) -> Result<(), String> {
    let mode_indicator = match mode {
        "workspace" => "#[fg=green,bold] WORKSPACE ",
        "monitor"   => "#[fg=yellow,bold] MONITOR ",
        "chat"      => "#[fg=cyan,bold] CHAT ",
        "setup"     => "#[fg=magenta,bold] SETUP ",
        _           => "#[fg=white,bold] CLAWDESK ",
    };

    tmux_cmd(&["set-option", "-t", session, "status-style", "bg=#1a1b26,fg=#a9b1d6"])?;
    tmux_cmd(&["set-option", "-t", session, "status-left-length", "40"])?;
    tmux_cmd(&["set-option", "-t", session, "status-right-length", "80"])?;
    tmux_cmd(&[
        "set-option", "-t", session, "status-left",
        &format!("#[bg=#7aa2f7,fg=#1a1b26,bold]  ClawDesk {mode_indicator}#[default] "),
    ])?;
    tmux_cmd(&[
        "set-option", "-t", session, "status-right",
        "#[fg=#565f89] %H:%M  #[fg=#7aa2f7]#S #[default]",
    ])?;
    tmux_cmd(&["set-option", "-t", session, "pane-border-style", "fg=#3b4261"])?;
    tmux_cmd(&["set-option", "-t", session, "pane-active-border-style", "fg=#7aa2f7"])?;
    tmux_cmd(&["set-option", "-t", session, "pane-border-status", "top"])?;
    tmux_cmd(&["set-option", "-t", session, "pane-border-format",
        " #[fg=#7aa2f7,bold]#{pane_title} #[default]"])?;
    tmux_cmd(&["set-option", "-t", session, "mouse", "on"])?;
    Ok(())
}

fn find_clawdesk_binary() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "clawdesk".to_string())
}

/// Print available layouts with descriptions.
pub fn print_layouts() {
    println!();
    println!("  Available tmux layouts:");
    println!();
    println!("  {:<15} {}", "desktop", "Full 15-window experience mirroring the Tauri desktop app");
    println!("  {:<15} {}", "", "  MAIN:      Overview, Chat");
    println!("  {:<15} {}", "", "  CLUSTER:   A2A Directory");
    println!("  {:<15} {}", "", "  BUILD:     Skills, Automations");
    println!("  {:<15} {}", "", "  WORKSPACE: Agents, Channels, Files");
    println!("  {:<15} {}", "", "  CONNECT:   Extensions, MCP");
    println!("  {:<15} {}", "", "  SYSTEM:    Settings, Logs, Models, Docs, Runtime");
    println!("  {:<15} {}", "", "  Navigate: Ctrl-B + 0..9, Ctrl-B + n/p for 10+");
    println!();
    println!("  {:<15} {}", "workspace", "4-pane dev layout: Agent REPL + Gateway + Health + Commands");
    println!("  {:<15} {}", "monitor", "3-pane ops layout: Health + Channels + Logs");
    println!("  {:<15} {}", "chat", "2-pane focused: Agent Chat + Quick Commands");
    println!();
    println!("  Aliases:");
    println!("    desktop   = full, app");
    println!("    workspace = ws, dev");
    println!("    monitor   = mon, ops");
    println!("    chat      = focus");
    println!();
}

/// Print tmux key bindings cheat sheet.
pub fn print_keybindings() {
    println!();
    println!("  ┌─── tmux Key Bindings ────────────────────────────────────┐");
    println!("  │                                                          │");
    println!("  │  Navigation (Desktop mode — 15 screens):                 │");
    println!("  │                                                          │");
    println!("  │  MAIN        Ctrl-B + 0  Overview                        │");
    println!("  │              Ctrl-B + 1  Chat (Agent REPL)               │");
    println!("  │  CLUSTER     Ctrl-B + 2  A2A Directory                   │");
    println!("  │  BUILD       Ctrl-B + 3  Skills                          │");
    println!("  │              Ctrl-B + 4  Automations                     │");
    println!("  │  WORKSPACE   Ctrl-B + 5  Agents                          │");
    println!("  │              Ctrl-B + 6  Channels                        │");
    println!("  │              Ctrl-B + 7  Files                           │");
    println!("  │  CONNECT     Ctrl-B + 8  Extensions                      │");
    println!("  │              Ctrl-B + 9  MCP                             │");
    println!("  │  SYSTEM      Ctrl-B + n  → 10:Settings                   │");
    println!("  │              Ctrl-B + n  → 11:Logs                       │");
    println!("  │              Ctrl-B + n  → 12:Models                     │");
    println!("  │              Ctrl-B + n  → 13:Docs                       │");
    println!("  │              Ctrl-B + n  → 14:Runtime                    │");
    println!("  │                                                          │");
    println!("  │  General:                                                │");
    println!("  │  Ctrl-B + n/p   Next/previous screen                    │");
    println!("  │  Ctrl-B + d     Detach (session stays alive)            │");
    println!("  │  Ctrl-B + z     Zoom/unzoom current pane                │");
    println!("  │  Ctrl-B + arrow Switch between panes                    │");
    println!("  │  Ctrl-B + [     Scroll mode (q to exit)                 │");
    println!("  │  Ctrl-B + c     New window (tab)                        │");
    println!("  │  Ctrl-B + s     Session picker                          │");
    println!("  │  Ctrl-B + w     Window picker                           │");
    println!("  │  Ctrl-B + x     Kill current pane                       │");
    println!("  │  Ctrl-B + %     Split pane vertically                   │");
    println!("  │  Ctrl-B + \"     Split pane horizontally                 │");
    println!("  │                                                          │");
    println!("  │  Mouse is enabled — click, drag, scroll                 │");
    println!("  │                                                          │");
    println!("  └──────────────────────────────────────────────────────────┘");
    println!();
}
