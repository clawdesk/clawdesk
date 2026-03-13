//! Canonical directory conventions for ClawDesk.
//!
//! All binaries (CLI, Tauri desktop, gateway, bench) MUST use these functions
//! to resolve storage paths. This eliminates the class of bugs where different
//! entrypoints write to different directories (e.g. `~/.clawdesk/sochdb/` vs
//! `~/.clawdesk/data/`).
//!
//! # Platform conventions
//!
//! | Platform | `home()`          | `data()`                                  |
//! |----------|-------------------|-------------------------------------------|
//! | macOS    | `$HOME`           | `~/Library/Application Support/clawdesk`  |
//! | Linux    | `$HOME`           | `$XDG_DATA_HOME/clawdesk` or `~/.local/share/clawdesk` |
//! | Windows  | `%USERPROFILE%`   | `%APPDATA%/clawdesk`                      |
//! | Fallback | `.`               | `~/.clawdesk`                             |

use std::path::PathBuf;

/// User's home directory.
///
/// Resolution order: `$HOME` → `%USERPROFILE%` → `.` (current dir).
pub fn home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

/// Platform-appropriate data directory for persistent application state.
///
/// - macOS: `~/Library/Application Support/clawdesk`
/// - Linux: `$XDG_DATA_HOME/clawdesk` or `~/.local/share/clawdesk`
/// - Windows: `%APPDATA%/clawdesk`
/// - Fallback: `~/.clawdesk`
pub fn data() -> PathBuf {
    // macOS
    if cfg!(target_os = "macos") {
        return home().join("Library").join("Application Support").join("clawdesk");
    }

    // Linux: respect XDG Base Directory Specification
    if cfg!(target_os = "linux") {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            return PathBuf::from(xdg).join("clawdesk");
        }
        return home().join(".local").join("share").join("clawdesk");
    }

    // Windows: %APPDATA%
    if cfg!(target_os = "windows") {
        if let Ok(appdata) = std::env::var("APPDATA") {
            return PathBuf::from(appdata).join("clawdesk");
        }
    }

    // Fallback
    home().join(".clawdesk")
}

/// Canonical SochDB storage directory.
///
/// **All binaries must use this path** — not `~/.clawdesk/data/` or any other
/// ad-hoc path. Using different paths causes data bifurcation where Tauri and
/// CLI binaries see different state.
///
/// Resolves to `~/.clawdesk/sochdb/`.
pub fn sochdb() -> PathBuf {
    home().join(".clawdesk").join("sochdb")
}

/// Canonical threads storage directory.
///
/// Resolves to `~/.clawdesk/threads/`.
pub fn threads() -> PathBuf {
    home().join(".clawdesk").join("threads")
}

/// Dot-directory for clawdesk config and state.
///
/// Resolves to `~/.clawdesk/`.
pub fn dot_clawdesk() -> PathBuf {
    home().join(".clawdesk")
}

/// Skills directory for user-installed skills.
///
/// - macOS: `~/Library/Application Support/clawdesk/skills/`
/// - Others: `<data()>/skills/`
pub fn skills() -> PathBuf {
    data().join("skills")
}

/// Agents directory for agent definitions (TOML + instruction.md).
///
/// Resolves to `~/.clawdesk/agents/`.
pub fn agents() -> PathBuf {
    dot_clawdesk().join("agents")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sochdb_is_under_dot_clawdesk() {
        let path = sochdb();
        assert!(path.to_str().unwrap().contains(".clawdesk"));
        assert!(path.ends_with("sochdb"));
    }

    #[test]
    fn dot_clawdesk_is_under_home() {
        let dot = dot_clawdesk();
        let h = home();
        assert!(dot.starts_with(&h));
    }

    #[test]
    fn threads_is_sibling_of_sochdb() {
        let t = threads();
        let s = sochdb();
        assert_eq!(t.parent(), s.parent());
    }
}
