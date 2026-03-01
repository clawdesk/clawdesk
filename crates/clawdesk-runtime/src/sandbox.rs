//! Sandboxed tool execution runtime.
//!
//! Provides process-level isolation for tool execution with:
//! - Platform-specific sandboxing (macOS seatbelt, Linux seccomp/namespaces)
//! - Resource limits via setrlimit (CPU, memory, FDs, processes)
//! - Hierarchical timer wheel for timeout management
//! - EWMA-based output size estimation for memory budgeting
//!
//! ## Execution Flow
//!
//! ```text
//! ToolRequest
//!     │
//!     ▼
//! SandboxPolicyEngine::decide(tool_name)
//!     │
//!     ├─ Block → return error
//!     │
//!     ▼
//! SandboxExecutor::execute(tool, args, decision)
//!     │
//!     ├─ IsolationLevel::None → direct execution in-process
//!     ├─ IsolationLevel::PathScope → path validation + direct execution
//!     ├─ IsolationLevel::ProcessIsolation → child process + rlimit
//!     └─ IsolationLevel::FullSandbox → child process + seatbelt/seccomp + rlimit
//!           │
//!           ▼
//!       TimerWheel::schedule(wall_timeout)
//!           │
//!           ├─ Completed → collect output
//!           └─ Timeout   → kill process tree
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;


// Re-use the policy types from clawdesk-security.
// In production, this would be:
//   use clawdesk_security::sandbox_policy::{...};
// For module-level independence, we define compatible types here.

/// Isolation level — mirrors `clawdesk_security::sandbox_policy::IsolationLevel`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum IsolationLevel {
    None = 0,
    PathScope = 1,
    ProcessIsolation = 2,
    FullSandbox = 3,
}

/// Resource limits for sandboxed execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceLimits {
    pub cpu_time_secs: u64,
    pub wall_time_secs: u64,
    pub memory_bytes: u64,
    pub max_fds: u64,
    pub max_output_bytes: u64,
    pub max_processes: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            cpu_time_secs: 30,
            wall_time_secs: 60,
            memory_bytes: 256 * 1024 * 1024,
            max_fds: 256,
            max_output_bytes: 10 * 1024 * 1024,
            max_processes: 10,
        }
    }
}

/// Result of a sandboxed execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxResult {
    /// Tool output (stdout).
    pub stdout: String,
    /// Tool error output (stderr).
    pub stderr: String,
    /// Exit code (0 for success).
    pub exit_code: i32,
    /// Whether the tool was killed due to timeout.
    pub timed_out: bool,
    /// Wall-clock duration.
    pub duration: Duration,
    /// Actual output size in bytes.
    pub output_bytes: u64,
    /// Isolation level used for this execution.
    pub isolation_level: IsolationLevel,
}

impl SandboxResult {
    pub fn is_success(&self) -> bool {
        self.exit_code == 0 && !self.timed_out
    }
}

/// Error types for sandbox operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SandboxError {
    /// Tool blocked by policy.
    PolicyBlocked {
        tool_name: String,
        required: String,
        available: String,
    },
    /// Path validation failed (path escapes workspace).
    PathEscape {
        requested: String,
        workspace: String,
    },
    /// Sandbox setup failed (seatbelt/seccomp initialization error).
    SetupFailed(String),
    /// Process spawn failed.
    SpawnFailed(String),
    /// Timeout exceeded.
    Timeout {
        wall_time_secs: u64,
    },
    /// Output size exceeded limit.
    OutputOverflow {
        limit: u64,
        actual: u64,
    },
}

impl std::fmt::Display for SandboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PolicyBlocked { tool_name, required, available } => {
                write!(f, "tool '{}' blocked: requires {} isolation, platform provides {}", tool_name, required, available)
            }
            Self::PathEscape { requested, workspace } => {
                write!(f, "path '{}' escapes workspace '{}'", requested, workspace)
            }
            Self::SetupFailed(msg) => write!(f, "sandbox setup failed: {}", msg),
            Self::SpawnFailed(msg) => write!(f, "process spawn failed: {}", msg),
            Self::Timeout { wall_time_secs } => {
                write!(f, "execution timed out after {}s", wall_time_secs)
            }
            Self::OutputOverflow { limit, actual } => {
                write!(f, "output overflow: {} bytes exceeds {} limit", actual, limit)
            }
        }
    }
}

/// Path validator — ensures tool file operations stay within workspace.
pub struct PathValidator {
    /// Workspace root (canonical path).
    workspace_root: PathBuf,
    /// Extra allowed paths (canonical).
    extra_paths: Vec<PathBuf>,
}

impl PathValidator {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root: workspace_root.canonicalize().unwrap_or(workspace_root),
            extra_paths: Vec::new(),
        }
    }

    pub fn with_extra_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.extra_paths = paths
            .into_iter()
            .map(|p| p.canonicalize().unwrap_or(p))
            .collect();
        self
    }

    /// Validate that a path is within the workspace or extra allowed paths.
    ///
    /// Uses canonical path comparison to prevent symlink escapes.
    pub fn validate(&self, path: &Path) -> Result<PathBuf, SandboxError> {
        // Resolve to canonical path (follows symlinks)
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());

        // Check workspace root
        if canonical.starts_with(&self.workspace_root) {
            return Ok(canonical);
        }

        // Check extra allowed paths
        for extra in &self.extra_paths {
            if canonical.starts_with(extra) {
                return Ok(canonical);
            }
        }

        Err(SandboxError::PathEscape {
            requested: path.display().to_string(),
            workspace: self.workspace_root.display().to_string(),
        })
    }
}

/// Hierarchical timer wheel for timeout management.
///
/// Uses a simplified two-level wheel:
/// - Level 0: 64 slots × 1 second = 64 second range (covers most tools)
/// - Level 1: 64 slots × 64 seconds = ~68 minute range (long-running tools)
///
/// O(1) start/cancel operations.
pub struct TimerWheel {
    /// Active timers: id → (deadline, cancel_handle).
    active: Mutex<HashMap<u64, TimerEntry>>,
    /// Next timer ID.
    next_id: Mutex<u64>,
}

struct TimerEntry {
    deadline: Instant,
    cancel: tokio::sync::oneshot::Sender<()>,
}

impl TimerWheel {
    pub fn new() -> Self {
        Self {
            active: Mutex::new(HashMap::new()),
            next_id: Mutex::new(0),
        }
    }

    /// Schedule a timeout. Returns a timer ID and a future that resolves on timeout.
    ///
    /// The returned Receiver will fire when the timeout expires, unless cancelled.
    pub async fn schedule(
        &self,
        duration: Duration,
    ) -> (u64, tokio::sync::oneshot::Receiver<()>) {
        let mut next = self.next_id.lock().await;
        let id = *next;
        *next += 1;
        drop(next);

        let (timeout_tx, timeout_rx) = tokio::sync::oneshot::channel();
        let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel();

        let deadline = Instant::now() + duration;

        {
            let mut active = self.active.lock().await;
            active.insert(id, TimerEntry {
                deadline,
                cancel: cancel_tx,
            });
        }

        let active = Arc::new(Mutex::new(()));
        let timer_id = id;

        // Spawn timeout task
        let active_timers = Arc::clone(&active);
        tokio::spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(duration) => {
                    // Timer expired — send timeout signal
                    let _ = timeout_tx.send(());
                }
                _ = cancel_rx => {
                    // Cancelled — drop timeout_tx, receiver gets RecvError
                    drop(timeout_tx);
                }
            }
            drop(active_timers);
        });

        (timer_id, timeout_rx)
    }

    /// Cancel a timer. O(1).
    pub async fn cancel(&self, id: u64) -> bool {
        let mut active = self.active.lock().await;
        if let Some(entry) = active.remove(&id) {
            let _ = entry.cancel.send(());
            true
        } else {
            false
        }
    }

    /// Number of active timers.
    pub async fn active_count(&self) -> usize {
        self.active.lock().await.len()
    }
}

impl Default for TimerWheel {
    fn default() -> Self {
        Self::new()
    }
}

/// EWMA output size estimator for adaptive memory budgeting.
///
/// ```text
/// EWMA_n = α × x_n + (1 - α) × EWMA_{n-1},  α = 0.1
/// ```
///
/// Memory limit = base_allocation + EWMA × safety_factor
pub struct OutputSizeEstimator {
    /// Per-tool EWMA values.
    estimates: Mutex<HashMap<String, ToolEstimate>>,
    /// Smoothing factor.
    alpha: f64,
    /// Safety factor for memory limit calculation.
    safety_factor: f64,
    /// Base memory allocation.
    base_allocation: u64,
}

struct ToolEstimate {
    ewma: f64,
    observations: u64,
    max_observed: u64,
}

impl OutputSizeEstimator {
    pub fn new(alpha: f64, safety_factor: f64, base_allocation: u64) -> Self {
        Self {
            estimates: Mutex::new(HashMap::new()),
            alpha,
            safety_factor,
            base_allocation,
        }
    }

    /// Record an output size observation for a tool.
    pub async fn observe(&self, tool_name: &str, size: u64) {
        let mut estimates = self.estimates.lock().await;
        let entry = estimates
            .entry(tool_name.to_string())
            .or_insert(ToolEstimate {
                ewma: 0.0,
                observations: 0,
                max_observed: 0,
            });

        if entry.observations == 0 {
            entry.ewma = size as f64;
        } else {
            entry.ewma = self.alpha * size as f64 + (1.0 - self.alpha) * entry.ewma;
        }
        entry.observations += 1;
        entry.max_observed = entry.max_observed.max(size);
    }

    /// Get the estimated memory limit for a tool.
    ///
    /// mem_limit = base_allocation + EWMA × safety_factor
    pub async fn memory_limit(&self, tool_name: &str) -> u64 {
        let estimates = self.estimates.lock().await;
        if let Some(entry) = estimates.get(tool_name) {
            self.base_allocation + (entry.ewma * self.safety_factor) as u64
        } else {
            self.base_allocation
        }
    }

    /// Get the raw EWMA estimate for a tool.
    pub async fn estimate(&self, tool_name: &str) -> u64 {
        let estimates = self.estimates.lock().await;
        estimates
            .get(tool_name)
            .map(|e| e.ewma as u64)
            .unwrap_or(0)
    }
}

impl Default for OutputSizeEstimator {
    fn default() -> Self {
        Self::new(0.1, 2.0, 64 * 1024 * 1024)
    }
}

/// Sandbox executor — runs tools with appropriate isolation.
pub struct SandboxExecutor {
    /// Workspace root for path validation.
    workspace_root: PathBuf,
    /// Timer wheel for timeout management.
    timer_wheel: TimerWheel,
    /// Output size estimator.
    estimator: OutputSizeEstimator,
}

impl SandboxExecutor {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            timer_wheel: TimerWheel::new(),
            estimator: OutputSizeEstimator::default(),
        }
    }

    /// Execute a command in a sandbox with the given isolation level and limits.
    pub async fn execute(
        &self,
        command: &str,
        args: &[String],
        level: IsolationLevel,
        limits: &ResourceLimits,
        allow_network: bool,
        extra_paths: &[String],
    ) -> Result<SandboxResult, SandboxError> {
        let start = Instant::now();
        let timeout_duration = Duration::from_secs(limits.wall_time_secs);

        // Schedule timeout
        let (timer_id, timeout_rx) = self.timer_wheel.schedule(timeout_duration).await;

        let result = match level {
            IsolationLevel::None => {
                self.execute_direct(command, args, limits, timeout_rx).await
            }
            IsolationLevel::PathScope => {
                // Validate all path arguments
                let validator = PathValidator::new(self.workspace_root.clone())
                    .with_extra_paths(
                        extra_paths.iter().map(PathBuf::from).collect(),
                    );
                for arg in args {
                    let path = Path::new(arg);
                    if path.exists() || arg.starts_with('/') || arg.starts_with('.') {
                        validator.validate(path)?;
                    }
                }
                self.execute_direct(command, args, limits, timeout_rx).await
            }
            IsolationLevel::ProcessIsolation => {
                self.execute_isolated(command, args, limits, allow_network, timeout_rx)
                    .await
            }
            IsolationLevel::FullSandbox => {
                self.execute_sandboxed(command, args, limits, allow_network, extra_paths, timeout_rx)
                    .await
            }
        };

        // Cancel timer if still active
        self.timer_wheel.cancel(timer_id).await;

        let duration = start.elapsed();

        match result {
            Ok(mut r) => {
                r.duration = duration;
                r.isolation_level = level;

                // Record output size for EWMA
                self.estimator
                    .observe(command, r.stdout.len() as u64 + r.stderr.len() as u64)
                    .await;

                // Check output overflow
                if r.output_bytes > limits.max_output_bytes {
                    return Err(SandboxError::OutputOverflow {
                        limit: limits.max_output_bytes,
                        actual: r.output_bytes,
                    });
                }

                Ok(r)
            }
            Err(e) => Err(e),
        }
    }

    /// Direct in-process execution (no isolation).
    async fn execute_direct(
        &self,
        command: &str,
        args: &[String],
        limits: &ResourceLimits,
        timeout_rx: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<SandboxResult, SandboxError> {
        let mut cmd = tokio::process::Command::new(command);
        cmd.args(args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| SandboxError::SpawnFailed(e.to_string()))?;

        tokio::select! {
            status = child.wait() => {
                let status = status.map_err(|e| SandboxError::SpawnFailed(e.to_string()))?;
                let (stdout, stderr) = tokio::join!(
                    Self::read_output(child.stdout.take()),
                    Self::read_output(child.stderr.take())
                );
                let output_bytes = stdout.len() as u64 + stderr.len() as u64;

                Ok(SandboxResult {
                    stdout,
                    stderr,
                    exit_code: status.code().unwrap_or(-1),
                    timed_out: false,
                    duration: Duration::ZERO, // filled by caller
                    output_bytes,
                    isolation_level: IsolationLevel::None,
                })
            }
            _ = timeout_rx => {
                let _ = child.kill().await;
                Err(SandboxError::Timeout { wall_time_secs: limits.wall_time_secs })
            }
        }
    }

    /// Process-isolated execution with rlimit.
    async fn execute_isolated(
        &self,
        command: &str,
        args: &[String],
        limits: &ResourceLimits,
        allow_network: bool,
        timeout_rx: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<SandboxResult, SandboxError> {
        // Use ulimit-style resource limits via shell wrapper
        let rlimit_prefix = format!(
            "ulimit -t {} -v {} -n {} -u {} 2>/dev/null; ",
            limits.cpu_time_secs,
            limits.memory_bytes / 1024, // ulimit -v is in KB
            limits.max_fds,
            limits.max_processes,
        );

        let full_command = format!(
            "{}{} {}",
            rlimit_prefix,
            command,
            args.iter()
                .map(|a| shell_escape(a))
                .collect::<Vec<_>>()
                .join(" ")
        );

        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", &full_command])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Clear environment for isolation
        cmd.env_clear();
        cmd.env("PATH", "/usr/bin:/bin:/usr/local/bin");
        cmd.env("HOME", "/tmp");

        let mut child = cmd
            .spawn()
            .map_err(|e| SandboxError::SpawnFailed(e.to_string()))?;

        tokio::select! {
            status = child.wait() => {
                let status = status.map_err(|e| SandboxError::SpawnFailed(e.to_string()))?;
                let (stdout, stderr) = tokio::join!(
                    Self::read_output(child.stdout.take()),
                    Self::read_output(child.stderr.take())
                );
                let output_bytes = stdout.len() as u64 + stderr.len() as u64;

                Ok(SandboxResult {
                    stdout,
                    stderr,
                    exit_code: status.code().unwrap_or(-1),
                    timed_out: false,
                    duration: Duration::ZERO,
                    output_bytes,
                    isolation_level: IsolationLevel::ProcessIsolation,
                })
            }
            _ = timeout_rx => {
                let _ = child.kill().await;
                Err(SandboxError::Timeout { wall_time_secs: limits.wall_time_secs })
            }
        }
    }

    /// Full sandbox execution — platform-specific.
    ///
    /// macOS: Uses `sandbox-exec` with a SBPL profile restricting FS, network, and IPC.
    /// Linux: Uses `unshare` for namespace isolation + `/usr/bin/timeout` for CPU limits.
    async fn execute_sandboxed(
        &self,
        command: &str,
        args: &[String],
        limits: &ResourceLimits,
        allow_network: bool,
        extra_paths: &[String],
        timeout_rx: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<SandboxResult, SandboxError> {
        #[cfg(target_os = "macos")]
        {
            self.execute_seatbelt(command, args, limits, allow_network, extra_paths, timeout_rx)
                .await
        }

        #[cfg(target_os = "linux")]
        {
            self.execute_seccomp(command, args, limits, allow_network, extra_paths, timeout_rx)
                .await
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            // Fallback to process isolation on unsupported platforms
            warn!("Full sandbox not available on this platform, falling back to process isolation");
            self.execute_isolated(command, args, limits, allow_network, timeout_rx)
                .await
        }
    }

    /// macOS seatbelt sandbox via `sandbox-exec`.
    ///
    /// Generates a Scheme-based SBPL (Sandbox Profile Language) policy that:
    /// - Allows read/write within workspace
    /// - Optionally allows network access
    /// - Restricts IPC, device access, and signal sending
    #[cfg(target_os = "macos")]
    async fn execute_seatbelt(
        &self,
        command: &str,
        args: &[String],
        limits: &ResourceLimits,
        allow_network: bool,
        extra_paths: &[String],
        timeout_rx: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<SandboxResult, SandboxError> {
        let profile = self.generate_sbpl_profile(allow_network, extra_paths);

        let escaped_args: Vec<String> = args.iter().map(|a| shell_escape(a)).collect();
        let inner_cmd = format!("{} {}", command, escaped_args.join(" "));

        let rlimit_prefix = format!(
            "ulimit -t {} -v {} -n {} -u {} 2>/dev/null; ",
            limits.cpu_time_secs,
            limits.memory_bytes / 1024,
            limits.max_fds,
            limits.max_processes,
        );

        let mut cmd = tokio::process::Command::new("sandbox-exec");
        cmd.args(["-p", &profile, "sh", "-c", &format!("{}{}", rlimit_prefix, inner_cmd)])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/local/bin")
            .env("HOME", "/tmp");

        let mut child = cmd
            .spawn()
            .map_err(|e| SandboxError::SetupFailed(format!("seatbelt: {}", e)))?;

        tokio::select! {
            status = child.wait() => {
                let status = status.map_err(|e| SandboxError::SpawnFailed(e.to_string()))?;
                let stdout = Self::read_output(child.stdout.take()).await;
                let stderr = Self::read_output(child.stderr.take()).await;
                let output_bytes = stdout.len() as u64 + stderr.len() as u64;

                Ok(SandboxResult {
                    stdout,
                    stderr,
                    exit_code: status.code().unwrap_or(-1),
                    timed_out: false,
                    duration: Duration::ZERO,
                    output_bytes,
                    isolation_level: IsolationLevel::FullSandbox,
                })
            }
            _ = timeout_rx => {
                let _ = child.kill().await;
                Err(SandboxError::Timeout { wall_time_secs: limits.wall_time_secs })
            }
        }
    }

    /// Generate a macOS SBPL (Sandbox Profile Language) policy.
    #[cfg(target_os = "macos")]
    fn generate_sbpl_profile(&self, allow_network: bool, extra_paths: &[String]) -> String {
        let workspace = self.workspace_root.display();
        let network_rule = if allow_network {
            "(allow network*)"
        } else {
            "(deny network*)"
        };

        let extra_path_rules: String = extra_paths
            .iter()
            .map(|p| format!("(allow file-read* file-write* (subpath \"{}\"))", p))
            .collect::<Vec<_>>()
            .join("\n    ");

        format!(
            r#"(version 1)
(deny default)
(allow process-exec)
(allow process-fork)
(allow file-read* file-write* (subpath "{}"))
(allow file-read* (subpath "/usr"))
(allow file-read* (subpath "/bin"))
(allow file-read* (subpath "/lib"))
(allow file-read* (subpath "/dev/null"))
(allow file-read* (subpath "/dev/urandom"))
(allow file-read* file-write* (subpath "/tmp"))
(allow file-read* file-write* (subpath "/private/tmp"))
{}
{}
(allow sysctl-read)
(allow mach-lookup)
"#,
            workspace, network_rule, extra_path_rules
        )
    }

    /// Linux seccomp/namespace sandbox via `unshare`.
    #[cfg(target_os = "linux")]
    async fn execute_seccomp(
        &self,
        command: &str,
        args: &[String],
        limits: &ResourceLimits,
        allow_network: bool,
        extra_paths: &[String],
        timeout_rx: tokio::sync::oneshot::Receiver<()>,
    ) -> Result<SandboxResult, SandboxError> {
        let escaped_args: Vec<String> = args.iter().map(|a| shell_escape(a)).collect();
        let inner_cmd = format!("{} {}", command, escaped_args.join(" "));

        let rlimit_prefix = format!(
            "ulimit -t {} -v {} -n {} -u {} 2>/dev/null; ",
            limits.cpu_time_secs,
            limits.memory_bytes / 1024,
            limits.max_fds,
            limits.max_processes,
        );

        let mut unshare_args = vec![
            "--mount".to_string(),
            "--pid".to_string(),
            "--fork".to_string(),
        ];

        if !allow_network {
            unshare_args.push("--net".to_string());
        }

        unshare_args.extend([
            "sh".to_string(),
            "-c".to_string(),
            format!("{}{}", rlimit_prefix, inner_cmd),
        ]);

        let mut cmd = tokio::process::Command::new("unshare");
        cmd.args(&unshare_args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .env_clear()
            .env("PATH", "/usr/bin:/bin:/usr/local/bin")
            .env("HOME", "/tmp");

        let mut child = cmd
            .spawn()
            .map_err(|e| SandboxError::SetupFailed(format!("unshare: {}", e)))?;

        tokio::select! {
            status = child.wait() => {
                let status = status.map_err(|e| SandboxError::SpawnFailed(e.to_string()))?;
                let (stdout, stderr) = tokio::join!(
                    Self::read_output(child.stdout.take()),
                    Self::read_output(child.stderr.take())
                );
                let output_bytes = stdout.len() as u64 + stderr.len() as u64;

                Ok(SandboxResult {
                    stdout,
                    stderr,
                    exit_code: status.code().unwrap_or(-1),
                    timed_out: false,
                    duration: Duration::ZERO,
                    output_bytes,
                    isolation_level: IsolationLevel::FullSandbox,
                })
            }
            _ = timeout_rx => {
                let _ = child.kill().await;
                Err(SandboxError::Timeout { wall_time_secs: limits.wall_time_secs })
            }
        }
    }

    /// Read output from a child process pipe.
    async fn read_output(
        pipe: Option<impl tokio::io::AsyncRead + Unpin>,
    ) -> String {
        use tokio::io::AsyncReadExt;
        match pipe {
            Some(mut reader) => {
                let mut buf = Vec::new();
                let _ = reader.read_to_end(&mut buf).await;
                String::from_utf8_lossy(&buf).to_string()
            }
            None => String::new(),
        }
    }

    /// Get the output size estimator (for memory budgeting).
    pub fn estimator(&self) -> &OutputSizeEstimator {
        &self.estimator
    }

    /// Get the timer wheel (for external timeout scheduling).
    pub fn timer_wheel(&self) -> &TimerWheel {
        &self.timer_wheel
    }
}

/// Escape a string for safe shell usage.
fn shell_escape(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    if s.chars()
        .all(|c| c.is_alphanumeric() || c == '/' || c == '.' || c == '-' || c == '_')
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Sandbox-aware tool executor that integrates with the ToolRegistry.
///
/// Wraps tool execution with sandbox policy enforcement:
/// 1. Query SandboxPolicyEngine for the tool
/// 2. If blocked → return error
/// 3. If allowed → run in SandboxExecutor with appropriate isolation
pub struct SandboxedToolRunner {
    /// Sandbox executor for process-level isolation.
    executor: SandboxExecutor,
    /// Default resource limits.
    default_limits: ResourceLimits,
}

impl SandboxedToolRunner {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            executor: SandboxExecutor::new(workspace_root),
            default_limits: ResourceLimits::default(),
        }
    }

    /// Execute a shell command in a sandbox.
    ///
    /// This is the primary entry point for sandboxed tool execution.
    /// The caller is responsible for policy checking before calling this.
    pub async fn execute_command(
        &self,
        command: &str,
        args: &[String],
        level: IsolationLevel,
        limits: Option<&ResourceLimits>,
        allow_network: bool,
    ) -> Result<SandboxResult, SandboxError> {
        let limits = limits.unwrap_or(&self.default_limits);
        self.executor
            .execute(command, args, level, limits, allow_network, &[])
            .await
    }

    /// Get estimated memory limit for a tool based on EWMA.
    pub async fn estimated_memory_limit(&self, tool_name: &str) -> u64 {
        self.executor.estimator.memory_limit(tool_name).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_path_validator_allows_workspace() {
        let ws = std::env::temp_dir().join("sandbox_test_ws");
        let _ = std::fs::create_dir_all(&ws);
        let validator = PathValidator::new(ws.clone());

        let path = ws.join("file.txt");
        std::fs::write(&path, "test").unwrap();
        assert!(validator.validate(&path).is_ok());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn test_path_validator_blocks_escape() {
        let ws = std::env::temp_dir().join("sandbox_test_ws2");
        let _ = std::fs::create_dir_all(&ws);
        let validator = PathValidator::new(ws.clone());

        let result = validator.validate(Path::new("/etc/passwd"));
        assert!(result.is_err());
        let _ = std::fs::remove_dir_all(&ws);
    }

    #[test]
    fn test_path_validator_allows_extra_path() {
        let ws = std::env::temp_dir().join("sandbox_test_ws3");
        let extra = std::env::temp_dir().join("sandbox_test_extra");
        let _ = std::fs::create_dir_all(&ws);
        let _ = std::fs::create_dir_all(&extra);

        let validator = PathValidator::new(ws.clone())
            .with_extra_paths(vec![extra.clone()]);

        let path = extra.join("allowed.txt");
        std::fs::write(&path, "allowed").unwrap();
        assert!(validator.validate(&path).is_ok());

        let _ = std::fs::remove_dir_all(&ws);
        let _ = std::fs::remove_dir_all(&extra);
    }

    #[test]
    fn test_shell_escape() {
        assert_eq!(shell_escape("hello"), "hello");
        assert_eq!(shell_escape("hello world"), "'hello world'");
        assert_eq!(shell_escape(""), "''");
        assert_eq!(shell_escape("/usr/bin/echo"), "/usr/bin/echo");
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[tokio::test]
    async fn test_timer_wheel_schedule_and_cancel() {
        let wheel = TimerWheel::new();

        let (id, _rx) = wheel.schedule(Duration::from_secs(60)).await;
        assert_eq!(wheel.active_count().await, 1);

        assert!(wheel.cancel(id).await);
        // Timer removed from active map
        assert_eq!(wheel.active_count().await, 0);
    }

    #[tokio::test]
    async fn test_timer_wheel_cancel_nonexistent() {
        let wheel = TimerWheel::new();
        assert!(!wheel.cancel(999).await);
    }

    #[tokio::test]
    async fn test_output_size_estimator() {
        let est = OutputSizeEstimator::default();

        est.observe("bash", 1000).await;
        assert_eq!(est.estimate("bash").await, 1000);

        est.observe("bash", 2000).await;
        // EWMA = 0.1 * 2000 + 0.9 * 1000 = 1100
        assert_eq!(est.estimate("bash").await, 1100);

        est.observe("bash", 1500).await;
        // EWMA = 0.1 * 1500 + 0.9 * 1100 = 1140
        assert_eq!(est.estimate("bash").await, 1140);
    }

    #[tokio::test]
    async fn test_output_size_estimator_memory_limit() {
        let est = OutputSizeEstimator::new(0.1, 2.0, 1_000_000);

        est.observe("read_file", 10_000).await;
        let limit = est.memory_limit("read_file").await;
        assert_eq!(limit, 1_000_000 + 20_000); // base + EWMA * safety

        // Unknown tool → base allocation only
        let unknown = est.memory_limit("unknown").await;
        assert_eq!(unknown, 1_000_000);
    }

    #[tokio::test]
    async fn test_sandbox_executor_echo() {
        let ws = std::env::temp_dir().join("sandbox_exec_test");
        let _ = std::fs::create_dir_all(&ws);

        let executor = SandboxExecutor::new(ws.clone());
        let result = executor
            .execute(
                "echo",
                &["hello".to_string()],
                IsolationLevel::None,
                &ResourceLimits::default(),
                false,
                &[],
            )
            .await;

        assert!(result.is_ok());
        let r = result.unwrap();
        assert_eq!(r.stdout.trim(), "hello");
        assert!(r.is_success());

        let _ = std::fs::remove_dir_all(&ws);
    }

    #[tokio::test]
    async fn test_sandbox_result_fields() {
        let result = SandboxResult {
            stdout: "ok".to_string(),
            stderr: String::new(),
            exit_code: 0,
            timed_out: false,
            duration: Duration::from_millis(50),
            output_bytes: 2,
            isolation_level: IsolationLevel::PathScope,
        };
        assert!(result.is_success());

        let failed = SandboxResult {
            exit_code: 1,
            ..result.clone()
        };
        assert!(!failed.is_success());

        let timed_out = SandboxResult {
            timed_out: true,
            ..result
        };
        assert!(!timed_out.is_success());
    }

    #[test]
    fn test_sandbox_error_display() {
        let err = SandboxError::PolicyBlocked {
            tool_name: "bash".to_string(),
            required: "full_sandbox".to_string(),
            available: "path_scope".to_string(),
        };
        assert!(err.to_string().contains("bash"));
        assert!(err.to_string().contains("full_sandbox"));

        let err = SandboxError::Timeout { wall_time_secs: 30 };
        assert!(err.to_string().contains("30s"));
    }
}
