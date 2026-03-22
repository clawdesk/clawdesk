//! WSL bridge — path translation and command routing between
//! native Windows and Windows Subsystem for Linux.

use std::path::{Path, PathBuf};

/// WSL availability and configuration.
#[derive(Debug, Clone)]
pub struct WslBridge {
    /// Whether WSL is available on this system.
    pub available: bool,
    /// Default WSL distribution name (from `wsl --list --quiet`).
    pub default_distro: Option<String>,
}

/// Where a command should execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionTarget {
    /// Run inside WSL.
    Wsl,
    /// Run as native Windows process.
    NativeWindows,
}

impl WslBridge {
    /// Detect WSL availability.
    pub fn detect() -> Self {
        #[cfg(windows)]
        {
            let available = std::process::Command::new("wsl")
                .arg("--status")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map(|s| s.success())
                .unwrap_or(false);

            let default_distro = if available {
                std::process::Command::new("wsl")
                    .args(["--list", "--quiet"])
                    .output()
                    .ok()
                    .and_then(|out| {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        stdout.lines()
                            .next()
                            .map(|l| l.trim().trim_matches('\0').to_string())
                            .filter(|s| !s.is_empty())
                    })
            } else {
                None
            };

            Self { available, default_distro }
        }

        #[cfg(not(windows))]
        {
            // On non-Windows, check if we're inside WSL
            let inside_wsl = crate::shell_dispatch::is_wsl_environment();
            Self {
                available: inside_wsl,
                default_distro: None,
            }
        }
    }

    /// Translate a Windows path to its WSL mount point.
    ///
    /// `C:\Users\foo\project` → `/mnt/c/Users/foo/project`
    pub fn to_wsl_path(windows_path: &str) -> Option<String> {
        let path = windows_path.replace('\\', "/");
        // Match drive letter: C:/... → /mnt/c/...
        if path.len() >= 2 && path.as_bytes()[1] == b':' {
            let drive = (path.as_bytes()[0] as char).to_lowercase().next()?;
            let rest = &path[2..];
            Some(format!("/mnt/{}{}", drive, rest))
        } else {
            None
        }
    }

    /// Translate a WSL path to a Windows UNC path.
    ///
    /// `/home/user/project` → `\\wsl$\<distro>\home\user\project`
    pub fn to_windows_path(&self, wsl_path: &str) -> Option<String> {
        if !wsl_path.starts_with('/') {
            return None;
        }

        // /mnt/c/... → C:\...
        if wsl_path.starts_with("/mnt/") && wsl_path.len() >= 6 {
            let drive = wsl_path.as_bytes()[5] as char;
            if drive.is_ascii_alphabetic() {
                let rest = &wsl_path[6..];
                return Some(format!("{}:{}", drive.to_uppercase(), rest.replace('/', "\\")));
            }
        }

        // General WSL path → UNC path
        let distro = self.default_distro.as_deref().unwrap_or("Ubuntu");
        Some(format!("\\\\wsl$\\{}\\{}", distro, wsl_path.replace('/', "\\")))
    }

    /// Determine if a command should run in WSL or native Windows.
    pub fn route_command(command: &str) -> ExecutionTarget {
        let first_word = command.split_whitespace().next().unwrap_or("");

        // Explicit commands for each target
        if first_word.ends_with(".exe")
            || first_word.eq_ignore_ascii_case("powershell")
            || first_word.eq_ignore_ascii_case("cmd")
        {
            return ExecutionTarget::NativeWindows;
        }

        let unix_tools = [
            "ls", "cat", "grep", "find", "sed", "awk", "make", "cmake",
            "gcc", "g++", "python3", "pip3", "node", "npm", "cargo",
            "rustc", "git", "curl", "wget", "ssh", "tar",
        ];
        if unix_tools.contains(&first_word) || command.starts_with("./") {
            return ExecutionTarget::Wsl;
        }

        // Default: native Windows
        ExecutionTarget::NativeWindows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_to_wsl_path() {
        assert_eq!(
            WslBridge::to_wsl_path("C:\\Users\\foo\\project"),
            Some("/mnt/c/Users/foo/project".into())
        );
        assert_eq!(
            WslBridge::to_wsl_path("D:\\data"),
            Some("/mnt/d/data".into())
        );
    }

    #[test]
    fn wsl_to_windows_path_mnt() {
        let bridge = WslBridge { available: true, default_distro: Some("Ubuntu".into()) };
        assert_eq!(
            bridge.to_windows_path("/mnt/c/Users/foo"),
            Some("C:\\Users\\foo".into())
        );
    }

    #[test]
    fn wsl_to_windows_path_unc() {
        let bridge = WslBridge { available: true, default_distro: Some("Ubuntu".into()) };
        assert_eq!(
            bridge.to_windows_path("/home/user/project"),
            Some("\\\\wsl$\\Ubuntu\\\\home\\user\\project".into())
        );
    }

    #[test]
    fn route_exe_to_native() {
        assert_eq!(WslBridge::route_command("notepad.exe"), ExecutionTarget::NativeWindows);
    }

    #[test]
    fn route_unix_to_wsl() {
        assert_eq!(WslBridge::route_command("cargo build"), ExecutionTarget::Wsl);
        assert_eq!(WslBridge::route_command("grep -r foo"), ExecutionTarget::Wsl);
    }
}
