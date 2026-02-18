//! Shell completion generation for ClawDesk CLI.
//!
//! Generates completion scripts for bash, zsh, fish, and PowerShell
//! using clap_complete.
//!
//! ## Usage
//! ```bash
//! # Generate and install (bash)
//! clawdesk completions bash > ~/.bash_completion.d/clawdesk
//!
//! # Generate and install (zsh)
//! clawdesk completions zsh > ~/.zfunc/_clawdesk
//!
//! # Generate and install (fish)
//! clawdesk completions fish > ~/.config/fish/completions/clawdesk.fish
//!
//! # PowerShell
//! clawdesk completions powershell >> $PROFILE
//! ```

use clap::CommandFactory;
use clap_complete::{generate, Shell};
use std::io;

/// Generate shell completion script for the given shell.
pub fn generate_completions(shell: &str) -> Result<(), String> {
    let shell = match shell.to_lowercase().as_str() {
        "bash" => Shell::Bash,
        "zsh" => Shell::Zsh,
        "fish" => Shell::Fish,
        "powershell" | "ps" | "pwsh" => Shell::PowerShell,
        other => return Err(format!(
            "Unknown shell: '{}'. Supported: bash, zsh, fish, powershell",
            other
        )),
    };

    let mut cmd = super::Cli::command();
    generate(shell, &mut cmd, "clawdesk", &mut io::stdout());

    Ok(())
}
