//! User shell detection — identifies the user's preferred shell
//! and provides syntax adaptation hints.
//!
//! The agent generates commands assuming bash syntax. A user on fish shell
//! gets errors from `&&` chaining. This module detects the shell and
//! provides operators that work for each variant.

use serde::{Deserialize, Serialize};

/// The user's shell type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UserShell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
    Cmd,
    Nushell,
    Dash,
    Unknown(String),
}

impl UserShell {
    /// Detect the current user's shell.
    pub fn detect() -> Self {
        // 1. Check $SHELL env (Unix convention)
        if let Ok(shell) = std::env::var("SHELL") {
            let basename = shell.rsplit('/').next().unwrap_or(&shell);
            match basename {
                "bash" => return Self::Bash,
                "zsh" => return Self::Zsh,
                "fish" => return Self::Fish,
                "nu" | "nushell" => return Self::Nushell,
                "dash" => return Self::Dash,
                _ => {}
            }
        }

        // 2. Check $FISH_VERSION (fish doesn't always set $SHELL correctly)
        if std::env::var("FISH_VERSION").is_ok() {
            return Self::Fish;
        }

        // 3. Check $PSVersionTable (PowerShell)
        if std::env::var("PSModulePath").is_ok() {
            return Self::PowerShell;
        }

        // 4. Check COMSPEC (Windows CMD)
        #[cfg(windows)]
        if let Ok(comspec) = std::env::var("COMSPEC") {
            if comspec.to_lowercase().contains("cmd.exe") {
                return Self::Cmd;
            }
        }

        // 5. Default
        #[cfg(unix)]
        { Self::Bash }
        #[cfg(windows)]
        { Self::Cmd }
    }

    /// The command chaining operator for this shell.
    ///
    /// Bash/zsh: `&&`
    /// Fish: `; and`
    /// PowerShell: `-and` or `; if ($?) {`
    pub fn chain_operator(&self) -> &str {
        match self {
            Self::Fish => "; and ",
            _ => " && ",
        }
    }

    /// The environment variable export syntax.
    pub fn export_var(&self, key: &str, value: &str) -> String {
        match self {
            Self::Fish => format!("set -gx {} {}", key, shell_quote(value)),
            Self::PowerShell => format!("$env:{} = '{}'", key, value.replace('\'', "''")),
            Self::Cmd => format!("set {}={}", key, value),
            Self::Nushell => format!("$env.{} = '{}'", key, value.replace('\'', "''")),
            _ => format!("export {}={}", key, shell_quote(value)),
        }
    }

    /// Human-readable name for system prompt injection.
    pub fn display_name(&self) -> &str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
            Self::PowerShell => "PowerShell",
            Self::Cmd => "cmd.exe",
            Self::Nushell => "nushell",
            Self::Dash => "dash",
            Self::Unknown(s) => s,
        }
    }

    /// Generate a system prompt fragment about the user's shell.
    pub fn to_prompt_hint(&self) -> String {
        match self {
            Self::Fish => {
                "User's shell is fish. Use `; and` instead of `&&` for chaining. \
                 Use `set -gx KEY VALUE` instead of `export KEY=VALUE`. \
                 Do not use bash-specific syntax like `$(...)` — use `(...)` instead."
                    .into()
            }
            Self::PowerShell => {
                "User's shell is PowerShell. Use `$env:KEY = 'VALUE'` for env vars. \
                 Use semicolons to chain commands. Use `Get-ChildItem` instead of `ls`."
                    .into()
            }
            Self::Cmd => {
                "User's shell is cmd.exe. Use `set KEY=VALUE` for env vars. \
                 Use `&` to chain commands. Use backslashes for paths."
                    .into()
            }
            Self::Nushell => {
                "User's shell is nushell. Use `$env.KEY = 'VALUE'` for env vars. \
                 Pipelines work differently — nu uses structured data, not text streams."
                    .into()
            }
            _ => format!("User's shell is {}.", self.display_name()),
        }
    }
}

/// Shell-quote a value for POSIX shells (bash/zsh/dash).
fn shell_quote(value: &str) -> String {
    if value.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.' || c == '/') {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_some_shell() {
        let shell = UserShell::detect();
        // Should always return something, never panic
        assert!(!shell.display_name().is_empty());
    }

    #[test]
    fn fish_chain_operator() {
        assert_eq!(UserShell::Fish.chain_operator(), "; and ");
        assert_eq!(UserShell::Bash.chain_operator(), " && ");
    }

    #[test]
    fn fish_export_syntax() {
        let export = UserShell::Fish.export_var("PATH", "/usr/bin");
        assert_eq!(export, "set -gx PATH /usr/bin");
    }

    #[test]
    fn powershell_export_syntax() {
        let export = UserShell::PowerShell.export_var("NODE_ENV", "production");
        assert_eq!(export, "$env:NODE_ENV = 'production'");
    }

    #[test]
    fn shell_quoting() {
        assert_eq!(shell_quote("simple"), "simple");
        assert_eq!(shell_quote("has space"), "'has space'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn prompt_hints_non_empty() {
        for shell in &[UserShell::Fish, UserShell::PowerShell, UserShell::Cmd, UserShell::Nushell, UserShell::Bash] {
            assert!(!shell.to_prompt_hint().is_empty());
        }
    }
}
