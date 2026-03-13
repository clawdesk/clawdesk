//! # Tmux-integrated onboarding for ClawDesk
//!
//! A guided, step-by-step first-run experience that works inside tmux.
//! Walks users through provider setup, model selection, and layout choice,
//! then auto-launches the selected tmux workspace.
//!
//! ## Flow
//! 1. Welcome screen with platform detection
//! 2. Dependency check (tmux, cargo, providers)
//! 3. Provider API key wizard (reuses `onboard.rs` logic)
//! 4. Default model selection
//! 5. Layout selection (desktop / workspace / monitor / chat)
//! 6. Launch the tmux session

use std::io::{self, Write};

use crate::tmux::{self, Layout, TmuxConfig};

/// Run the tmux-aware onboarding flow.
///
/// If tmux is available, guides the user through setup and launches a tmux
/// session. If tmux is not available, falls back to the standard onboarding
/// wizard and prints a setup hint.
pub async fn run_tmux_onboarding(
    session_name: Option<String>,
    workspace: Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    print_welcome_banner();

    // Step 1: Check dependencies
    println!("  [1/5] Checking Dependencies");
    println!();
    check_dependencies();
    println!();

    // Step 2: tmux availability
    if !tmux::tmux_available() {
        println!("  ⚠ tmux is not installed.");
        println!("  Install it to unlock the full tmux desktop experience:");
        println!();
        println!("    macOS:  brew install tmux");
        println!("    Linux:  sudo apt install tmux");
        println!("    Other:  https://github.com/tmux/tmux/wiki/Installing");
        println!();
        println!("  Continuing with standard setup...");
        println!();

        // Fall back to the standard onboarding
        crate::onboard::run_onboarding().await?;
        return Ok(());
    }

    if let Some(ver) = tmux::tmux_version() {
        println!("  ✓ {ver}");
    }
    println!();

    // Step 3: Provider setup (reuse the standard onboarding)
    println!("  [2/5] Provider Configuration");
    println!("  Let's configure your LLM providers.");
    println!();
    crate::onboard::run_onboarding().await?;
    println!();

    // Step 4: Layout selection
    println!("  [3/5] Choose Your Layout");
    println!();
    let layout = select_layout()?;
    println!();

    // Step 5: Model selection
    println!("  [4/5] Default Model");
    let model = select_quick_model()?;
    println!();

    // Step 6: Launch
    println!("  [5/5] Launching tmux session...");
    println!();

    let config = TmuxConfig {
        session_name: session_name.unwrap_or_else(|| "clawdesk".to_string()),
        layout,
        gateway_url: "http://127.0.0.1:18789".to_string(),
        model,
        workspace_dir: workspace,
        attach: true,
        if_exists: tmux::IfExistsPolicy::Replace,
    };

    tmux::launch(&config).map_err(|e: String| -> Box<dyn std::error::Error + Send + Sync> {
        Box::from(e.to_string())
    })?;

    Ok(())
}

/// Print the welcome banner.
fn print_welcome_banner() {
    println!();
    println!("  ╔═══════════════════════════════════════════════════════════╗");
    println!("  ║                                                           ║");
    println!("  ║      ██████╗██╗      █████╗ ██╗    ██╗                    ║");
    println!("  ║     ██╔════╝██║     ██╔══██╗██║    ██║                    ║");
    println!("  ║     ██║     ██║     ███████║██║ █╗ ██║                    ║");
    println!("  ║     ██║     ██║     ██╔══██║██║███╗██║                    ║");
    println!("  ║     ╚██████╗███████╗██║  ██║╚███╔███╔╝                    ║");
    println!("  ║      ╚═════╝╚══════╝╚═╝  ╚═╝ ╚══╝╚══╝                    ║");
    println!("  ║                                                           ║");
    println!("  ║     ClawDesk — AI Agent Desktop Runtime                   ║");
    println!("  ║     tmux Desktop Setup                                    ║");
    println!("  ║                                                           ║");
    println!("  ╚═══════════════════════════════════════════════════════════╝");
    println!();

    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    println!("  Platform: {} / {}", os, arch);
    println!("  Version:  {}", env!("CARGO_PKG_VERSION"));
    println!();
}

/// Check and report on system dependencies.
fn check_dependencies() {
    let checks: Vec<(&str, bool)> = vec![
        ("tmux", tmux::tmux_available()),
        ("cargo", which_exists("cargo")),
        ("curl", which_exists("curl")),
        ("watch", which_exists("watch")),
    ];

    for (name, ok) in &checks {
        if *ok {
            println!("  ✓ {name}");
        } else {
            println!("  ✗ {name} (not found — some features may be limited)");
        }
    }
}

/// Check if a binary is on PATH.
fn which_exists(name: &str) -> bool {
    std::process::Command::new("which")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Interactive layout selector.
fn select_layout() -> Result<Layout, Box<dyn std::error::Error + Send + Sync>> {
    println!("  Choose a layout for your ClawDesk tmux experience:");
    println!();
    println!("  ┌─────────────────────────────────────────────────────────┐");
    println!("  │                                                         │");
    println!("  │  [1] desktop    — Full 10-screen experience             │");
    println!("  │                    Mirrors the Tauri desktop app.       │");
    println!("  │                    10 windows: Dashboard, Chat,         │");
    println!("  │                    Sessions, Agents, Channels, Memory,  │");
    println!("  │                    Skills, Settings, Logs, Security     │");
    println!("  │                    Navigate: Ctrl-B + 0..9              │");
    println!("  │                                                         │");
    println!("  │  [2] workspace  — 4 panes: Agent REPL + Gateway +      │");
    println!("  │                    Health Monitor + Quick Commands       │");
    println!("  │                                                         │");
    println!("  │  [3] monitor    — 3 panes: Health + Channels + Logs     │");
    println!("  │                                                         │");
    println!("  │  [4] chat       — 2 panes: Agent Chat + Commands        │");
    println!("  │                                                         │");
    println!("  └─────────────────────────────────────────────────────────┘");
    println!();
    print!("  Select layout [1]: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let choice = input.trim();

    let layout = match choice {
        "2" | "workspace" | "ws" => Layout::Workspace,
        "3" | "monitor" | "mon" => Layout::Monitor,
        "4" | "chat" | "focus" => Layout::Chat,
        _ => Layout::Desktop,
    };

    println!("  ✓ Selected: {}", layout.name());
    Ok(layout)
}

/// Quick model selector.
fn select_quick_model() -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    println!("  Select a default model (or press Enter to skip):");
    println!();
    println!("    [1] claude-sonnet-4-20250514    (Anthropic)");
    println!("    [2] gpt-4o                      (OpenAI)");
    println!("    [3] gemini-2.0-flash             (Google)");
    println!("    [4] llama3.2:latest              (Ollama / local)");
    println!("    [5] Skip — configure later");
    println!();
    print!("  Choice [1]: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let choice = input.trim();

    let model = match choice {
        "2" => Some("gpt-4o".to_string()),
        "3" => Some("gemini-2.0-flash".to_string()),
        "4" => Some("llama3.2:latest".to_string()),
        "5" | "skip" => None,
        _ => Some("claude-sonnet-4-20250514".to_string()),
    };

    if let Some(ref m) = model {
        println!("  ✓ Default model: {m}");
    } else {
        println!("  ✓ Skipped — configure with: clawdesk config set model <name>");
    }

    Ok(model)
}
