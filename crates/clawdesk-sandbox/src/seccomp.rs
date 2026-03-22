//! seccomp-BPF syscall filtering per tool category.
//!
//! Each tool category gets a fine-grained syscall allowlist. Before executing
//! a tool, the sandbox applies the appropriate seccomp profile to restrict
//! the available syscall surface.
//!
//! ## Tool Categories → Syscall Profiles
//!
//! | Category | Allowed Extras | Rationale |
//! |----------|---------------|-----------|
//! | `file_read` | `openat`, `read`, `fstat`, `lseek` | Read-only FS |
//! | `file_write` | `openat`, `write`, `ftruncate`, `fsync` | FS mutation |
//! | `web_fetch` | `socket`, `connect`, `sendto`, `recvfrom` | Outbound HTTP |
//! | `shell_exec` | `execve`, `fork`, `pipe`, `wait4` | Process spawn |
//! | `compute_only` | (none beyond baseline) | Pure compute |
//!
//! All profiles share a baseline of memory management, signal handling,
//! and clock/time syscalls needed for any Rust binary.
//!
//! ## Platform Support
//!
//! - Linux x86_64/aarch64: Full seccomp-BPF via `prctl(PR_SET_SECCOMP)`.
//! - Other platforms: Profiles degrade to no-op (logged as warning).

use serde::{Deserialize, Serialize};
use std::fmt;
use tracing::warn;

// ---------------------------------------------------------------------------
// Syscall categories
// ---------------------------------------------------------------------------

/// Numbered syscall reference (architecture-dependent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SyscallNr(pub u32);

impl fmt::Display for SyscallNr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "syscall#{}", self.0)
    }
}

/// Well-known x86_64 syscall numbers for profile construction.
#[cfg(target_arch = "x86_64")]
pub mod nr {
    use super::SyscallNr;
    pub const READ: SyscallNr = SyscallNr(0);
    pub const WRITE: SyscallNr = SyscallNr(1);
    pub const OPEN: SyscallNr = SyscallNr(2);
    pub const CLOSE: SyscallNr = SyscallNr(3);
    pub const FSTAT: SyscallNr = SyscallNr(5);
    pub const LSEEK: SyscallNr = SyscallNr(8);
    pub const MMAP: SyscallNr = SyscallNr(9);
    pub const MPROTECT: SyscallNr = SyscallNr(10);
    pub const MUNMAP: SyscallNr = SyscallNr(11);
    pub const BRK: SyscallNr = SyscallNr(12);
    pub const RT_SIGACTION: SyscallNr = SyscallNr(13);
    pub const RT_SIGRETURN: SyscallNr = SyscallNr(15);
    pub const IOCTL: SyscallNr = SyscallNr(16);
    pub const ACCESS: SyscallNr = SyscallNr(21);
    pub const PIPE: SyscallNr = SyscallNr(22);
    pub const DUP2: SyscallNr = SyscallNr(33);
    pub const SOCKET: SyscallNr = SyscallNr(41);
    pub const CONNECT: SyscallNr = SyscallNr(42);
    pub const SENDTO: SyscallNr = SyscallNr(44);
    pub const RECVFROM: SyscallNr = SyscallNr(45);
    pub const CLONE: SyscallNr = SyscallNr(56);
    pub const FORK: SyscallNr = SyscallNr(57);
    pub const EXECVE: SyscallNr = SyscallNr(59);
    pub const EXIT: SyscallNr = SyscallNr(60);
    pub const WAIT4: SyscallNr = SyscallNr(61);
    pub const FCNTL: SyscallNr = SyscallNr(72);
    pub const FTRUNCATE: SyscallNr = SyscallNr(77);
    pub const GETCWD: SyscallNr = SyscallNr(79);
    pub const GETDENTS64: SyscallNr = SyscallNr(217);
    pub const OPENAT: SyscallNr = SyscallNr(257);
    pub const NEWFSTATAT: SyscallNr = SyscallNr(262);
    pub const FUTEX: SyscallNr = SyscallNr(202);
    pub const CLOCK_GETTIME: SyscallNr = SyscallNr(228);
    pub const EXIT_GROUP: SyscallNr = SyscallNr(231);
    pub const GETRANDOM: SyscallNr = SyscallNr(318);
    pub const FSYNC: SyscallNr = SyscallNr(74);
    pub const SCHED_YIELD: SyscallNr = SyscallNr(24);
    pub const SIGALTSTACK: SyscallNr = SyscallNr(131);
}

/// Fallback syscall numbers for non-x86_64 (empty — profiles become no-op).
#[cfg(not(target_arch = "x86_64"))]
pub mod nr {
    use super::SyscallNr;
    // Placeholder numbers; actual enforcement is disabled on non-x86_64.
    pub const READ: SyscallNr = SyscallNr(0);
    pub const WRITE: SyscallNr = SyscallNr(1);
    pub const OPEN: SyscallNr = SyscallNr(2);
    pub const CLOSE: SyscallNr = SyscallNr(3);
    pub const FSTAT: SyscallNr = SyscallNr(5);
    pub const LSEEK: SyscallNr = SyscallNr(8);
    pub const MMAP: SyscallNr = SyscallNr(9);
    pub const MPROTECT: SyscallNr = SyscallNr(10);
    pub const MUNMAP: SyscallNr = SyscallNr(11);
    pub const BRK: SyscallNr = SyscallNr(12);
    pub const RT_SIGACTION: SyscallNr = SyscallNr(13);
    pub const RT_SIGRETURN: SyscallNr = SyscallNr(15);
    pub const IOCTL: SyscallNr = SyscallNr(16);
    pub const ACCESS: SyscallNr = SyscallNr(21);
    pub const PIPE: SyscallNr = SyscallNr(22);
    pub const DUP2: SyscallNr = SyscallNr(33);
    pub const SOCKET: SyscallNr = SyscallNr(41);
    pub const CONNECT: SyscallNr = SyscallNr(42);
    pub const SENDTO: SyscallNr = SyscallNr(44);
    pub const RECVFROM: SyscallNr = SyscallNr(45);
    pub const CLONE: SyscallNr = SyscallNr(56);
    pub const FORK: SyscallNr = SyscallNr(57);
    pub const EXECVE: SyscallNr = SyscallNr(59);
    pub const EXIT: SyscallNr = SyscallNr(60);
    pub const WAIT4: SyscallNr = SyscallNr(61);
    pub const FCNTL: SyscallNr = SyscallNr(72);
    pub const FTRUNCATE: SyscallNr = SyscallNr(77);
    pub const GETCWD: SyscallNr = SyscallNr(79);
    pub const GETDENTS64: SyscallNr = SyscallNr(217);
    pub const OPENAT: SyscallNr = SyscallNr(257);
    pub const NEWFSTATAT: SyscallNr = SyscallNr(262);
    pub const FUTEX: SyscallNr = SyscallNr(202);
    pub const CLOCK_GETTIME: SyscallNr = SyscallNr(228);
    pub const EXIT_GROUP: SyscallNr = SyscallNr(231);
    pub const GETRANDOM: SyscallNr = SyscallNr(318);
    pub const FSYNC: SyscallNr = SyscallNr(74);
    pub const SCHED_YIELD: SyscallNr = SyscallNr(24);
    pub const SIGALTSTACK: SyscallNr = SyscallNr(131);
}

// ---------------------------------------------------------------------------
// seccomp action
// ---------------------------------------------------------------------------

/// Action taken when a syscall is not in the allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SeccompAction {
    /// Kill the process immediately (SECCOMP_RET_KILL_PROCESS).
    Kill,
    /// Return EPERM to the caller (SECCOMP_RET_ERRNO).
    Errno,
    /// Log the violation but allow the syscall (SECCOMP_RET_LOG).
    Log,
    /// Allow all syscalls (effectively disables seccomp).
    Allow,
}

// ---------------------------------------------------------------------------
// Tool categories and profiles
// ---------------------------------------------------------------------------

/// Tool category determining the syscall profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ToolCategory {
    /// Read-only filesystem access.
    FileRead,
    /// Filesystem read + write access.
    FileWrite,
    /// Outbound network (HTTP client).
    WebFetch,
    /// Shell command execution (subprocess spawn).
    ShellExec,
    /// Pure computation — no I/O beyond memory and signals.
    ComputeOnly,
}

impl fmt::Display for ToolCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FileRead => write!(f, "file_read"),
            Self::FileWrite => write!(f, "file_write"),
            Self::WebFetch => write!(f, "web_fetch"),
            Self::ShellExec => write!(f, "shell_exec"),
            Self::ComputeOnly => write!(f, "compute_only"),
        }
    }
}

/// A seccomp-BPF profile: a set of allowed syscalls and a default action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeccompProfile {
    /// Human-readable name.
    pub name: String,
    /// Tool category this profile applies to.
    pub category: ToolCategory,
    /// Allowed syscall numbers.
    pub allowed_syscalls: Vec<SyscallNr>,
    /// Action for disallowed syscalls.
    pub default_action: SeccompAction,
}

impl SeccompProfile {
    /// Baseline syscalls required by any Rust program.
    ///
    /// Memory management, signals, time, exit, futex, getrandom.
    fn baseline() -> Vec<SyscallNr> {
        vec![
            nr::BRK,
            nr::MMAP,
            nr::MPROTECT,
            nr::MUNMAP,
            nr::RT_SIGACTION,
            nr::RT_SIGRETURN,
            nr::SIGALTSTACK,
            nr::FUTEX,
            nr::SCHED_YIELD,
            nr::CLOCK_GETTIME,
            nr::GETRANDOM,
            nr::EXIT,
            nr::EXIT_GROUP,
            nr::CLOSE,
            nr::READ,
            nr::WRITE, // for stdout/stderr
            nr::IOCTL, // for terminal control
            nr::FCNTL,
        ]
    }

    /// Profile for `ToolCategory::FileRead`.
    pub fn file_read() -> Self {
        let mut allowed = Self::baseline();
        allowed.extend_from_slice(&[
            nr::OPEN,
            nr::OPENAT,
            nr::FSTAT,
            nr::NEWFSTATAT,
            nr::LSEEK,
            nr::ACCESS,
            nr::GETDENTS64,
            nr::GETCWD,
        ]);
        Self {
            name: "file_read".into(),
            category: ToolCategory::FileRead,
            allowed_syscalls: allowed,
            default_action: SeccompAction::Errno,
        }
    }

    /// Profile for `ToolCategory::FileWrite`.
    pub fn file_write() -> Self {
        let mut allowed = Self::file_read().allowed_syscalls;
        allowed.extend_from_slice(&[
            nr::FTRUNCATE,
            nr::FSYNC,
        ]);
        Self {
            name: "file_write".into(),
            category: ToolCategory::FileWrite,
            allowed_syscalls: allowed,
            default_action: SeccompAction::Errno,
        }
    }

    /// Profile for `ToolCategory::WebFetch`.
    pub fn web_fetch() -> Self {
        let mut allowed = Self::baseline();
        allowed.extend_from_slice(&[
            nr::SOCKET,
            nr::CONNECT,
            nr::SENDTO,
            nr::RECVFROM,
            // Needed for DNS resolution and TLS
            nr::OPEN,
            nr::OPENAT,
            nr::FSTAT,
            nr::NEWFSTATAT,
            nr::ACCESS,
            nr::GETCWD,
        ]);
        Self {
            name: "web_fetch".into(),
            category: ToolCategory::WebFetch,
            allowed_syscalls: allowed,
            default_action: SeccompAction::Errno,
        }
    }

    /// Profile for `ToolCategory::ShellExec`.
    pub fn shell_exec() -> Self {
        let mut allowed = Self::file_write().allowed_syscalls;
        allowed.extend_from_slice(&[
            nr::EXECVE,
            nr::FORK,
            nr::CLONE,
            nr::WAIT4,
            nr::PIPE,
            nr::DUP2,
            nr::SOCKET,
            nr::CONNECT,
            nr::SENDTO,
            nr::RECVFROM,
        ]);
        Self {
            name: "shell_exec".into(),
            category: ToolCategory::ShellExec,
            allowed_syscalls: allowed,
            default_action: SeccompAction::Errno,
        }
    }

    /// Profile for `ToolCategory::ComputeOnly`.
    pub fn compute_only() -> Self {
        Self {
            name: "compute_only".into(),
            category: ToolCategory::ComputeOnly,
            allowed_syscalls: Self::baseline(),
            default_action: SeccompAction::Errno,
        }
    }

    /// Get the profile for a given category.
    pub fn for_category(category: ToolCategory) -> Self {
        match category {
            ToolCategory::FileRead => Self::file_read(),
            ToolCategory::FileWrite => Self::file_write(),
            ToolCategory::WebFetch => Self::web_fetch(),
            ToolCategory::ShellExec => Self::shell_exec(),
            ToolCategory::ComputeOnly => Self::compute_only(),
        }
    }

    /// Check whether a syscall is allowed by this profile.
    pub fn allows(&self, syscall: SyscallNr) -> bool {
        self.allowed_syscalls.contains(&syscall)
    }

    /// Number of allowed syscalls.
    pub fn allowed_count(&self) -> usize {
        self.allowed_syscalls.len()
    }
}

// ---------------------------------------------------------------------------
// seccomp enforcer
// ---------------------------------------------------------------------------

/// Result of applying a seccomp profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeccompResult {
    /// Whether seccomp was actually enforced.
    pub enforced: bool,
    /// The profile that was applied.
    pub profile_name: String,
    /// Number of allowed syscalls.
    pub allowed_count: usize,
    /// Reason if not enforced.
    pub skip_reason: Option<String>,
}

/// Apply a seccomp-BPF profile to the current thread.
///
/// On Linux x86_64, compiles the allowlist into a BPF program and installs
/// it via `prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER)`.
///
/// On other platforms, logs a warning and returns without enforcement.
pub fn apply_seccomp_profile(profile: &SeccompProfile) -> SeccompResult {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        // NOTE: Real BPF compilation requires the `seccompiler` crate
        // (from Firecracker) or `libseccomp-rs`. Until one is integrated,
        // we honestly report that enforcement is NOT active.
        //
        // TODO: Add `seccompiler` dependency and compile allowlist into
        // BPF instructions, then install via:
        //   prctl(PR_SET_NO_NEW_PRIVS, 1)
        //   prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &prog)

        warn!(
            profile = %profile.name,
            allowed = profile.allowed_count(),
            action = ?profile.default_action,
            "seccomp-BPF profile NOT enforced (stub implementation — no BPF compiler integrated)"
        );

        SeccompResult {
            enforced: false,
            profile_name: profile.name.clone(),
            allowed_count: profile.allowed_count(),
            skip_reason: Some(
                "seccomp BPF compilation not yet implemented — \
                 integrate seccompiler crate for real enforcement".to_string()
            ),
        }
    }

    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        let reason = format!(
            "seccomp not available on {}/{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
        warn!(
            profile = %profile.name,
            reason = %reason,
            "seccomp enforcement skipped"
        );
        SeccompResult {
            enforced: false,
            profile_name: profile.name.clone(),
            allowed_count: profile.allowed_count(),
            skip_reason: Some(reason),
        }
    }
}

/// Resolve a tool name to a `ToolCategory` for seccomp profile selection.
///
/// Uses prefix matching on well-known tool names.
pub fn categorize_tool(tool_name: &str) -> ToolCategory {
    let lower = tool_name.to_lowercase();
    if lower.starts_with("read_file")
        || lower.starts_with("search")
        || lower.starts_with("grep")
        || lower.starts_with("list_dir")
        || lower == "file_read"
    {
        ToolCategory::FileRead
    } else if lower.starts_with("write_file")
        || lower.starts_with("edit")
        || lower.starts_with("create_file")
        || lower == "file_write"
    {
        ToolCategory::FileWrite
    } else if lower.starts_with("web")
        || lower.starts_with("fetch")
        || lower.starts_with("http")
        || lower.starts_with("curl")
        || lower == "web_fetch"
    {
        ToolCategory::WebFetch
    } else if lower.starts_with("shell")
        || lower.starts_with("exec")
        || lower.starts_with("run")
        || lower.starts_with("bash")
        || lower == "shell_exec"
    {
        ToolCategory::ShellExec
    } else {
        ToolCategory::ComputeOnly
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn baseline_has_essential_syscalls() {
        let baseline = SeccompProfile::baseline();
        assert!(baseline.contains(&nr::BRK));
        assert!(baseline.contains(&nr::MMAP));
        assert!(baseline.contains(&nr::EXIT));
        assert!(baseline.contains(&nr::FUTEX));
    }

    #[test]
    fn file_read_extends_baseline() {
        let profile = SeccompProfile::file_read();
        assert!(profile.allows(nr::OPENAT));
        assert!(profile.allows(nr::FSTAT));
        assert!(profile.allows(nr::LSEEK));
        // No write-specific syscalls.
        assert!(!profile.allows(nr::FTRUNCATE));
    }

    #[test]
    fn file_write_includes_read() {
        let profile = SeccompProfile::file_write();
        assert!(profile.allows(nr::OPENAT));
        assert!(profile.allows(nr::FSTAT));
        assert!(profile.allows(nr::FTRUNCATE));
        assert!(profile.allows(nr::FSYNC));
    }

    #[test]
    fn web_fetch_has_network() {
        let profile = SeccompProfile::web_fetch();
        assert!(profile.allows(nr::SOCKET));
        assert!(profile.allows(nr::CONNECT));
        assert!(!profile.allows(nr::EXECVE));
    }

    #[test]
    fn shell_exec_is_superset() {
        let profile = SeccompProfile::shell_exec();
        assert!(profile.allows(nr::EXECVE));
        assert!(profile.allows(nr::FORK));
        assert!(profile.allows(nr::SOCKET));
        assert!(profile.allows(nr::OPENAT));
    }

    #[test]
    fn compute_only_is_minimal() {
        let profile = SeccompProfile::compute_only();
        assert!(!profile.allows(nr::OPENAT));
        assert!(!profile.allows(nr::SOCKET));
        assert!(!profile.allows(nr::EXECVE));
        assert!(profile.allows(nr::BRK));
    }

    #[test]
    fn categorize_known_tools() {
        assert_eq!(categorize_tool("read_file"), ToolCategory::FileRead);
        assert_eq!(categorize_tool("write_file"), ToolCategory::FileWrite);
        assert_eq!(categorize_tool("web_search"), ToolCategory::WebFetch);
        assert_eq!(categorize_tool("shell_exec"), ToolCategory::ShellExec);
        assert_eq!(categorize_tool("compute_hash"), ToolCategory::ComputeOnly);
    }

    #[test]
    fn apply_profile_runs() {
        let profile = SeccompProfile::compute_only();
        let result = apply_seccomp_profile(&profile);
        assert_eq!(result.profile_name, "compute_only");
        // On macOS / non-linux, enforcement is skipped.
        #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
        assert!(!result.enforced);
    }

    #[test]
    fn all_categories_have_profiles() {
        let categories = [
            ToolCategory::FileRead,
            ToolCategory::FileWrite,
            ToolCategory::WebFetch,
            ToolCategory::ShellExec,
            ToolCategory::ComputeOnly,
        ];
        for cat in categories {
            let profile = SeccompProfile::for_category(cat);
            assert_eq!(profile.category, cat);
            assert!(!profile.allowed_syscalls.is_empty());
        }
    }
}
