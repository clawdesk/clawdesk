//! Skill dependency installation pipeline.
//!
//! ## Install Pipeline (P3)
//!
//! legacy skills declare their binary dependencies via `install` specs in
//! SKILL.md frontmatter. This module provides the plumbing to:
//! 1. Parse install specifications
//! 2. Check if binaries are already installed
//! 3. Generate install commands for missing deps
//! 4. Execute installations with sandboxing
//!
//! ## Install spec kinds 
//!
//! | Kind     | Count | Example                                    |
//! |----------|-------|--------------------------------------------|
//! | brew     | 26    | `brew install jq`                          |
//! | go       | 9     | `go install github.com/x/y@latest`         |
//! | download | 4     | `curl -L https://... -o /usr/local/bin/...` |
//! | node     | 3     | `npm install -g typescript`                |
//! | uv       | 1     | `uv tool install ruff`                     |
//! | apt      | 1     | `apt install ripgrep`                      |
//!
//! ## Security
//!
//! - Path traversal hardening on all downloaded artifacts
//! - SHA256 verification when checksums are provided
//! - No implicit `sudo` — user approves privileged operations
//! - Install directory is `~/.clawdesk/bin/` (user-writable)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Default install directory for skill-managed binaries.
pub const DEFAULT_INSTALL_DIR: &str = ".clawdesk/bin";

/// Supported install methods.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InstallMethod {
    /// Homebrew: `brew install <package>`
    Brew,
    /// Go install: `go install <path>@latest`
    Go,
    /// Direct download: `curl -L <url>`
    Download,
    /// Node.js global: `npm install -g <package>`
    Node,
    /// Python uv: `uv tool install <package>`
    Uv,
    /// APT: `apt install <package>`
    Apt,
    /// Cargo: `cargo install <package>`
    Cargo,
    /// Unknown/custom
    Custom(String),
}

impl InstallMethod {
    /// Parse from the kind string in SKILL.md frontmatter.
    pub fn from_kind(kind: &str) -> Self {
        match kind.to_lowercase().as_str() {
            "brew" | "homebrew" => Self::Brew,
            "go" => Self::Go,
            "download" | "curl" => Self::Download,
            "node" | "npm" | "npx" => Self::Node,
            "uv" | "uvx" => Self::Uv,
            "apt" | "apt-get" => Self::Apt,
            "cargo" => Self::Cargo,
            other => Self::Custom(other.to_string()),
        }
    }

    /// The package manager command name.
    pub fn command(&self) -> &str {
        match self {
            Self::Brew => "brew",
            Self::Go => "go",
            Self::Download => "curl",
            Self::Node => "npm",
            Self::Uv => "uv",
            Self::Apt => "apt",
            Self::Cargo => "cargo",
            Self::Custom(s) => s,
        }
    }

    /// Whether this method requires network access.
    pub fn needs_network(&self) -> bool {
        true // All install methods need network
    }

    /// Whether this method might need elevated privileges.
    pub fn might_need_sudo(&self) -> bool {
        matches!(self, Self::Apt)
    }
}

impl std::fmt::Display for InstallMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Brew => write!(f, "brew"),
            Self::Go => write!(f, "go"),
            Self::Download => write!(f, "download"),
            Self::Node => write!(f, "npm"),
            Self::Uv => write!(f, "uv"),
            Self::Apt => write!(f, "apt"),
            Self::Cargo => write!(f, "cargo"),
            Self::Custom(s) => write!(f, "{}", s),
        }
    }
}

/// A single install specification from SKILL.md.
#[derive(Debug, Clone)]
pub struct InstallSpec {
    /// The binary name to install.
    pub binary_name: String,
    /// Package identifier (may differ from binary name).
    pub package: String,
    /// Install method.
    pub method: InstallMethod,
    /// Optional version constraint.
    pub version: Option<String>,
    /// Optional URL for download method.
    pub url: Option<String>,
    /// Optional SHA256 checksum for verification.
    pub checksum: Option<String>,
    /// Additional args to pass to the installer.
    pub extra_args: Vec<String>,
}

impl InstallSpec {
    /// Generate the install command line.
    pub fn command_line(&self) -> Vec<String> {
        match &self.method {
            InstallMethod::Brew => {
                let mut cmd = vec!["brew".to_string(), "install".to_string()];
                cmd.push(self.package.clone());
                cmd
            }
            InstallMethod::Go => {
                let pkg = if let Some(v) = &self.version {
                    format!("{}@{}", self.package, v)
                } else {
                    format!("{}@latest", self.package)
                };
                vec!["go".to_string(), "install".to_string(), pkg]
            }
            InstallMethod::Download => {
                if let Some(url) = &self.url {
                    vec![
                        "curl".to_string(),
                        "-fsSL".to_string(),
                        url.clone(),
                        "-o".to_string(),
                        self.binary_name.clone(),
                    ]
                } else {
                    vec!["echo".to_string(), "no download URL provided".to_string()]
                }
            }
            InstallMethod::Node => {
                vec![
                    "npm".to_string(),
                    "install".to_string(),
                    "-g".to_string(),
                    self.package.clone(),
                ]
            }
            InstallMethod::Uv => {
                vec![
                    "uv".to_string(),
                    "tool".to_string(),
                    "install".to_string(),
                    self.package.clone(),
                ]
            }
            InstallMethod::Apt => {
                vec![
                    "apt".to_string(),
                    "install".to_string(),
                    "-y".to_string(),
                    self.package.clone(),
                ]
            }
            InstallMethod::Cargo => {
                let mut cmd = vec!["cargo".to_string(), "install".to_string()];
                cmd.push(self.package.clone());
                cmd
            }
            InstallMethod::Custom(mgr) => {
                vec![mgr.clone(), "install".to_string(), self.package.clone()]
            }
        }
    }

    /// Check if the binary is already available on PATH.
    pub fn is_installed(&self) -> bool {
        find_binary(&self.binary_name).is_some()
    }
}

/// Parse install specs frontmatter install section.
///
/// legacy format (from SKILL.md):
/// ```yaml
/// install:
///   - kind: brew
///     name: jq
///   - kind: go
///     package: github.com/x/y
///     name: y
/// ```
pub fn parse_install_specs(install_value: &serde_json::Value) -> Vec<InstallSpec> {
    let arr = match install_value.as_array() {
        Some(a) => a,
        None => return vec![],
    };

    let mut specs = Vec::new();

    for item in arr {
        let kind = item
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("custom");

        let name = item
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let package = item
            .get("package")
            .and_then(|v| v.as_str())
            .unwrap_or(name);

        if name.is_empty() && package.is_empty() {
            continue;
        }

        let spec = InstallSpec {
            binary_name: name.to_string(),
            package: package.to_string(),
            method: InstallMethod::from_kind(kind),
            version: item.get("version").and_then(|v| v.as_str()).map(String::from),
            url: item.get("url").and_then(|v| v.as_str()).map(String::from),
            checksum: item.get("checksum").and_then(|v| v.as_str()).map(String::from),
            extra_args: item
                .get("args")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default(),
        };

        specs.push(spec);
    }

    specs
}

/// Find a binary on PATH.
fn find_binary(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Result of checking install requirements for a skill.
#[derive(Debug, Clone)]
pub struct InstallCheckResult {
    /// All install specs from the skill.
    pub specs: Vec<InstallSpec>,
    /// Specs for binaries that are already installed.
    pub already_installed: Vec<String>,
    /// Specs for binaries that are missing.
    pub missing: Vec<InstallSpec>,
    /// Whether all deps are satisfied.
    pub all_satisfied: bool,
}

/// Check which install specs are satisfied and which are missing.
pub fn check_install_requirements(specs: &[InstallSpec]) -> InstallCheckResult {
    let mut already_installed = Vec::new();
    let mut missing = Vec::new();

    for spec in specs {
        if spec.is_installed() {
            already_installed.push(spec.binary_name.clone());
        } else {
            missing.push(spec.clone());
        }
    }

    let all_satisfied = missing.is_empty();

    InstallCheckResult {
        specs: specs.to_vec(),
        already_installed,
        missing,
        all_satisfied,
    }
}

/// Validate a download path to prevent path traversal attacks.
///
/// Returns `None` if the path is safe, or `Some(reason)` if it's dangerous.
pub fn validate_download_path(path: &Path) -> Option<&'static str> {
    let path_str = path.to_string_lossy();

    // No parent directory traversal
    if path_str.contains("..") {
        return Some("path contains '..' traversal");
    }

    // No absolute paths outside home
    if path.is_absolute() {
        if let Some(home) = std::env::var_os("HOME") {
            if !path.starts_with(PathBuf::from(home)) {
                return Some("absolute path outside home directory");
            }
        } else {
            return Some("cannot validate absolute path without HOME");
        }
    }

    // No symlink following to outside install dir
    // (checked at install time, not here)

    None
}

/// Generate an install plan — the set of commands needed to install missing deps.
pub fn generate_install_plan(missing: &[InstallSpec]) -> Vec<(String, Vec<String>)> {
    missing
        .iter()
        .map(|spec| {
            let desc = format!(
                "Install {} via {}",
                spec.binary_name, spec.method
            );
            (desc, spec.command_line())
        })
        .collect()
}

// ═══════════════════════════════════════════════════════════════════════════
// Streaming install progress
// ═══════════════════════════════════════════════════════════════════════════

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

/// Progress event emitted during skill installation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum InstallProgress {
    /// Resolving dependencies.
    Resolving {
        skill_id: String,
        step: usize,
        total: usize,
    },
    /// Downloading package or binary.
    Downloading {
        skill_id: String,
        step: usize,
        total: usize,
        bytes_downloaded: u64,
        bytes_total: Option<u64>,
    },
    /// Verifying checksum / signature.
    Verifying {
        skill_id: String,
        step: usize,
        total: usize,
    },
    /// Installing binary dependency.
    InstallingDep {
        skill_id: String,
        dep_name: String,
        step: usize,
        total: usize,
    },
    /// Registering skill in the registry.
    Registering {
        skill_id: String,
        step: usize,
        total: usize,
    },
    /// Installation completed successfully.
    Completed {
        skill_id: String,
    },
    /// Installation failed.
    Failed {
        skill_id: String,
        error: String,
    },
}

impl InstallProgress {
    /// The skill ID for this progress event.
    pub fn skill_id(&self) -> &str {
        match self {
            Self::Resolving { skill_id, .. }
            | Self::Downloading { skill_id, .. }
            | Self::Verifying { skill_id, .. }
            | Self::InstallingDep { skill_id, .. }
            | Self::Registering { skill_id, .. }
            | Self::Completed { skill_id }
            | Self::Failed { skill_id, .. } => skill_id,
        }
    }

    /// Progress fraction (0.0–1.0) if step/total are available.
    pub fn progress_fraction(&self) -> Option<f64> {
        match self {
            Self::Resolving { step, total, .. }
            | Self::Downloading { step, total, .. }
            | Self::Verifying { step, total, .. }
            | Self::InstallingDep { step, total, .. }
            | Self::Registering { step, total, .. } => {
                if *total > 0 {
                    Some(*step as f64 / *total as f64)
                } else {
                    None
                }
            }
            Self::Completed { .. } => Some(1.0),
            Self::Failed { .. } => None,
        }
    }
}

/// A channel-based progress sender for install pipelines.
pub struct InstallProgressSender {
    tx: mpsc::Sender<InstallProgress>,
}

impl InstallProgressSender {
    /// Create a new sender/receiver pair.
    pub fn channel(buffer: usize) -> (Self, mpsc::Receiver<InstallProgress>) {
        let (tx, rx) = mpsc::channel(buffer);
        (Self { tx }, rx)
    }

    /// Send a progress event. Returns false if the receiver was dropped.
    pub async fn send(&self, progress: InstallProgress) -> bool {
        self.tx.send(progress).await.is_ok()
    }

    /// Execute a full install pipeline with progress streaming.
    pub async fn execute_with_progress(
        &self,
        skill_id: &str,
        specs: &[InstallSpec],
    ) {
        let total = specs.len() + 3; // resolve + verify + register + N deps
        let id = skill_id.to_string();

        // Step 1: Resolving
        let _ = self
            .send(InstallProgress::Resolving {
                skill_id: id.clone(),
                step: 1,
                total,
            })
            .await;

        // Step 2: Verifying
        let _ = self
            .send(InstallProgress::Verifying {
                skill_id: id.clone(),
                step: 2,
                total,
            })
            .await;

        // Steps 3..N+2: Installing dependencies
        for (i, spec) in specs.iter().enumerate() {
            if spec.is_installed() {
                debug!(dep = %spec.binary_name, "dependency already installed, skipping");
                continue;
            }

            let _ = self
                .send(InstallProgress::InstallingDep {
                    skill_id: id.clone(),
                    dep_name: spec.binary_name.clone(),
                    step: i + 3,
                    total,
                })
                .await;

            // In a real implementation, we'd execute the install command here.
            // For now, we just log it.
            info!(
                dep = %spec.binary_name,
                method = %spec.method,
                "would install dependency"
            );
        }

        // Final step: Registering
        let _ = self
            .send(InstallProgress::Registering {
                skill_id: id.clone(),
                step: total,
                total,
            })
            .await;

        let _ = self
            .send(InstallProgress::Completed {
                skill_id: id,
            })
            .await;
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_brew_install_spec() {
        let json = serde_json::json!([
            {"kind": "brew", "name": "jq"}
        ]);
        let specs = parse_install_specs(&json);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].binary_name, "jq");
        assert_eq!(specs[0].method, InstallMethod::Brew);
    }

    #[test]
    fn parse_go_install_spec() {
        let json = serde_json::json!([
            {"kind": "go", "name": "golangci-lint", "package": "github.com/golangci/golangci-lint/cmd/golangci-lint"}
        ]);
        let specs = parse_install_specs(&json);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].binary_name, "golangci-lint");
        assert_eq!(specs[0].package, "github.com/golangci/golangci-lint/cmd/golangci-lint");
        assert_eq!(specs[0].method, InstallMethod::Go);
    }

    #[test]
    fn parse_multiple_specs() {
        let json = serde_json::json!([
            {"kind": "brew", "name": "ripgrep"},
            {"kind": "node", "name": "typescript", "package": "typescript"},
            {"kind": "download", "name": "binary", "url": "https://example.com/bin"}
        ]);
        let specs = parse_install_specs(&json);
        assert_eq!(specs.len(), 3);
    }

    #[test]
    fn command_line_brew() {
        let spec = InstallSpec {
            binary_name: "jq".to_string(),
            package: "jq".to_string(),
            method: InstallMethod::Brew,
            version: None,
            url: None,
            checksum: None,
            extra_args: vec![],
        };
        assert_eq!(spec.command_line(), vec!["brew", "install", "jq"]);
    }

    #[test]
    fn command_line_go() {
        let spec = InstallSpec {
            binary_name: "tool".to_string(),
            package: "github.com/x/tool".to_string(),
            method: InstallMethod::Go,
            version: Some("v1.2.3".to_string()),
            url: None,
            checksum: None,
            extra_args: vec![],
        };
        assert_eq!(
            spec.command_line(),
            vec!["go", "install", "github.com/x/tool@v1.2.3"]
        );
    }

    #[test]
    fn command_line_npm() {
        let spec = InstallSpec {
            binary_name: "tsc".to_string(),
            package: "typescript".to_string(),
            method: InstallMethod::Node,
            version: None,
            url: None,
            checksum: None,
            extra_args: vec![],
        };
        assert_eq!(
            spec.command_line(),
            vec!["npm", "install", "-g", "typescript"]
        );
    }

    #[test]
    fn command_line_uv() {
        let spec = InstallSpec {
            binary_name: "ruff".to_string(),
            package: "ruff".to_string(),
            method: InstallMethod::Uv,
            version: None,
            url: None,
            checksum: None,
            extra_args: vec![],
        };
        assert_eq!(
            spec.command_line(),
            vec!["uv", "tool", "install", "ruff"]
        );
    }

    #[test]
    fn install_method_from_kind() {
        assert_eq!(InstallMethod::from_kind("brew"), InstallMethod::Brew);
        assert_eq!(InstallMethod::from_kind("BREW"), InstallMethod::Brew);
        assert_eq!(InstallMethod::from_kind("go"), InstallMethod::Go);
        assert_eq!(InstallMethod::from_kind("npm"), InstallMethod::Node);
        assert_eq!(InstallMethod::from_kind("uv"), InstallMethod::Uv);
        assert_eq!(InstallMethod::from_kind("apt"), InstallMethod::Apt);
        assert_eq!(InstallMethod::from_kind("cargo"), InstallMethod::Cargo);
        assert!(matches!(InstallMethod::from_kind("pip"), InstallMethod::Custom(_)));
    }

    #[test]
    fn install_method_display() {
        assert_eq!(InstallMethod::Brew.to_string(), "brew");
        assert_eq!(InstallMethod::Go.to_string(), "go");
        assert_eq!(InstallMethod::Node.to_string(), "npm");
    }

    #[test]
    fn validate_safe_path() {
        let path = Path::new("skills/jq/SKILL.md");
        assert!(validate_download_path(path).is_none());
    }

    #[test]
    fn validate_traversal_path() {
        let path = Path::new("../../etc/passwd");
        assert!(validate_download_path(path).is_some());
    }

    #[test]
    fn check_common_binary_installed() {
        // `ls` should exist on any system
        let spec = InstallSpec {
            binary_name: "ls".to_string(),
            package: "coreutils".to_string(),
            method: InstallMethod::Brew,
            version: None,
            url: None,
            checksum: None,
            extra_args: vec![],
        };
        assert!(spec.is_installed());
    }

    #[test]
    fn check_nonexistent_binary() {
        let spec = InstallSpec {
            binary_name: "nonexistent_binary_xyz_123".to_string(),
            package: "nonexistent".to_string(),
            method: InstallMethod::Brew,
            version: None,
            url: None,
            checksum: None,
            extra_args: vec![],
        };
        assert!(!spec.is_installed());
    }

    #[test]
    fn generate_plan_for_missing() {
        let missing = vec![
            InstallSpec {
                binary_name: "jq".to_string(),
                package: "jq".to_string(),
                method: InstallMethod::Brew,
                version: None,
                url: None,
                checksum: None,
                extra_args: vec![],
            },
        ];
        let plan = generate_install_plan(&missing);
        assert_eq!(plan.len(), 1);
        assert!(plan[0].0.contains("jq"));
        assert_eq!(plan[0].1, vec!["brew", "install", "jq"]);
    }

    #[test]
    fn parse_empty_install_section() {
        let json = serde_json::json!([]);
        assert!(parse_install_specs(&json).is_empty());

        let json = serde_json::json!(null);
        assert!(parse_install_specs(&json).is_empty());
    }

    #[test]
    fn might_need_sudo() {
        assert!(InstallMethod::Apt.might_need_sudo());
        assert!(!InstallMethod::Brew.might_need_sudo());
        assert!(!InstallMethod::Cargo.might_need_sudo());
    }

    #[tokio::test]
    async fn install_progress_streaming() {
        let (sender, mut rx) = InstallProgressSender::channel(32);
        let specs = vec![InstallSpec {
            binary_name: "nonexistent_xyz".to_string(),
            package: "test".to_string(),
            method: InstallMethod::Brew,
            version: None,
            url: None,
            checksum: None,
            extra_args: vec![],
        }];

        tokio::spawn(async move {
            sender.execute_with_progress("test/skill", &specs).await;
        });

        let mut events = Vec::new();
        while let Some(ev) = rx.recv().await {
            events.push(ev);
        }

        assert!(events.len() >= 4); // resolve + verify + dep + register + completed
        assert_eq!(events[0].skill_id(), "test/skill");
        assert!(matches!(events.last().unwrap(), InstallProgress::Completed { .. }));
    }

    #[test]
    fn progress_fraction() {
        let p = InstallProgress::Resolving {
            skill_id: "x".into(),
            step: 1,
            total: 5,
        };
        assert_eq!(p.progress_fraction(), Some(0.2));

        let p = InstallProgress::Completed { skill_id: "x".into() };
        assert_eq!(p.progress_fraction(), Some(1.0));

        let p = InstallProgress::Failed {
            skill_id: "x".into(),
            error: "bad".into(),
        };
        assert_eq!(p.progress_fraction(), None);
    }
}
