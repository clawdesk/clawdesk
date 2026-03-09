//! Subprocess environment sandbox — environment-sanitized process spawning.
//!
//! Eliminates secret leakage through inherited environment variables by:
//! 1. Clearing the inherited environment (`cmd.env_clear()`)
//! 2. Re-adding only a platform-specific safe allowlist
//! 3. Adding caller-specified passthrough variables

use crate::{
    IsolationLevel, ResourceUsage, Sandbox, SandboxCommand, SandboxError, SandboxRequest,
    SandboxResult,
};
use async_trait::async_trait;
use std::collections::HashSet;
use std::path::Path;
use std::time::Instant;
use tokio::process::Command;
use tracing::{debug, warn};

/// Platform-specific safe environment variable allowlist.
///
/// These are the only variables inherited from the parent environment.
/// All others (including API keys) are stripped.
fn safe_env_allowlist() -> HashSet<&'static str> {
    let mut set = HashSet::new();

    // Universal
    set.insert("PATH");
    set.insert("HOME");
    set.insert("USER");
    set.insert("LANG");
    set.insert("LC_ALL");
    set.insert("LC_CTYPE");
    set.insert("TERM");
    set.insert("TMPDIR");
    set.insert("TZ");
    set.insert("SHELL");
    set.insert("LOGNAME");

    // macOS-specific
    #[cfg(target_os = "macos")]
    {
        set.insert("DYLD_FALLBACK_LIBRARY_PATH");
        set.insert("__CF_USER_TEXT_ENCODING");
    }

    // Linux-specific
    #[cfg(target_os = "linux")]
    {
        set.insert("LD_LIBRARY_PATH");
        set.insert("XDG_RUNTIME_DIR");
        set.insert("XDG_DATA_HOME");
        set.insert("XDG_CONFIG_HOME");
        set.insert("XDG_CACHE_HOME");
    }

    // Windows-specific
    #[cfg(target_os = "windows")]
    {
        set.insert("SYSTEMROOT");
        set.insert("COMSPEC");
        set.insert("WINDIR");
        set.insert("TEMP");
        set.insert("TMP");
        set.insert("SYSTEMDRIVE");
        set.insert("USERPROFILE");
        set.insert("APPDATA");
        set.insert("LOCALAPPDATA");
        set.insert("PROGRAMFILES");
    }

    set
}

/// Validate a command for injection patterns.
///
/// 3-layer defense:
/// - Layer 1: Container name sanitization (for Docker, handled separately)
/// - Layer 2: Image name validation (for Docker, handled separately)
/// - Layer 3: Command injection detection — reject backticks and `$()` patterns
pub fn validate_command(command: &str) -> Result<(), SandboxError> {
    let injection_patterns = ["`", "$(", "${"];

    for pattern in &injection_patterns {
        if command.contains(pattern) {
            return Err(SandboxError::CommandInjection {
                pattern: pattern.to_string(),
            });
        }
    }

    Ok(())
}

/// Build a sanitized Command with clean environment.
///
/// The returned Command has:
/// - Cleared environment (no inherited secrets)
/// - Only safe allowlist variables re-added
/// - Caller-specified passthrough variables added
/// - Working directory set to workspace root
pub fn sandbox_command(
    program: &str,
    args: &[String],
    workspace_root: &Path,
    env_passthrough: &std::collections::HashMap<String, String>,
) -> Result<Command, SandboxError> {
    // Validate executable path
    crate::workspace::validate_executable_path(Path::new(program))?;

    let mut cmd = Command::new(program);

    // CRITICAL: Clear all inherited environment
    cmd.env_clear();

    // Re-add safe allowlist from parent environment
    let allowlist = safe_env_allowlist();
    for key in &allowlist {
        if let Ok(value) = std::env::var(key) {
            cmd.env(key, value);
        }
    }

    // Add caller-specified passthrough variables
    for (key, value) in env_passthrough {
        cmd.env(key, value);
    }

    // Set working directory
    cmd.current_dir(workspace_root);

    // Add arguments
    cmd.args(args);

    // Redirect stderr
    cmd.stderr(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());

    Ok(cmd)
}

/// Subprocess sandbox runtime.
#[derive(Debug, Clone)]
pub struct SubprocessSandbox {
    /// Additional environment variables always passed through
    pub default_passthrough: Vec<String>,
}

impl SubprocessSandbox {
    pub fn new() -> Self {
        Self {
            default_passthrough: Vec::new(),
        }
    }
}

impl Default for SubprocessSandbox {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Sandbox for SubprocessSandbox {
    fn name(&self) -> &str {
        "subprocess"
    }

    fn isolation_level(&self) -> IsolationLevel {
        IsolationLevel::ProcessIsolation
    }

    async fn is_available(&self) -> bool {
        true // Always available
    }

    async fn execute(&self, request: SandboxRequest) -> Result<SandboxResult, SandboxError> {
        let start = Instant::now();

        match &request.command {
            SandboxCommand::Shell { command, args } => {
                // Validate command for injection
                validate_command(command)?;
                for arg in args {
                    validate_command(arg)?;
                }

                let mut cmd =
                    sandbox_command(command, args, &request.workspace_root, &request.env)?;

                // Apply timeout
                let timeout =
                    std::time::Duration::from_secs(request.limits.wall_time_secs);

                debug!(
                    command = %command,
                    timeout_secs = request.limits.wall_time_secs,
                    "executing sandboxed subprocess"
                );

                let result = tokio::time::timeout(timeout, cmd.output()).await;

                match result {
                    Ok(Ok(output)) => {
                        let elapsed = start.elapsed();
                        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                        // Check output size
                        if stdout.len() as u64 > request.limits.max_output_bytes {
                            warn!(
                                size = stdout.len(),
                                limit = request.limits.max_output_bytes,
                                "subprocess output truncated"
                            );
                        }

                        let truncated_stdout = if stdout.len() as u64 > request.limits.max_output_bytes
                        {
                            stdout[..request.limits.max_output_bytes as usize].to_string()
                        } else {
                            stdout
                        };

                        Ok(SandboxResult {
                            exit_code: output.status.code().unwrap_or(-1),
                            stdout: truncated_stdout,
                            stderr,
                            duration: elapsed,
                            resource_usage: ResourceUsage {
                                wall_time_ms: elapsed.as_millis() as u64,
                                output_bytes: output.stdout.len() as u64,
                                ..Default::default()
                            },
                        })
                    }
                    Ok(Err(e)) => Err(SandboxError::ExecutionFailed(e.to_string())),
                    Err(_) => Err(SandboxError::Timeout(timeout)),
                }
            }
            _ => Err(SandboxError::InvalidConfig(
                "subprocess sandbox only handles shell commands".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_allowlist_contains_essentials() {
        let list = safe_env_allowlist();
        assert!(list.contains("PATH"));
        assert!(list.contains("HOME"));
        assert!(list.contains("LANG"));
    }

    #[test]
    fn safe_allowlist_excludes_secrets() {
        let list = safe_env_allowlist();
        assert!(!list.contains("ANTHROPIC_API_KEY"));
        assert!(!list.contains("OPENAI_API_KEY"));
        assert!(!list.contains("AWS_SECRET_ACCESS_KEY"));
        assert!(!list.contains("GITHUB_TOKEN"));
    }

    #[test]
    fn detects_command_injection() {
        assert!(validate_command("echo `whoami`").is_err());
        assert!(validate_command("echo $(id)").is_err());
        assert!(validate_command("echo ${HOME}").is_err());
        assert!(validate_command("echo hello").is_ok());
        assert!(validate_command("ls -la").is_ok());
    }

    #[tokio::test]
    async fn subprocess_sandbox_echo() {
        let sandbox = SubprocessSandbox::new();
        let request = SandboxRequest {
            execution_id: "test-1".to_string(),
            tool_name: "shell_echo".to_string(),
            command: SandboxCommand::Shell {
                command: "echo".to_string(),
                args: vec!["hello sandbox".to_string()],
            },
            limits: crate::ResourceLimits::default(),
            working_dir: None,
            env: std::collections::HashMap::new(),
            network_allowed: false,
            workspace_root: std::env::temp_dir(),
        };

        let result = sandbox.execute(request).await.unwrap();
        assert_eq!(result.exit_code, 0);
        assert!(result.stdout.contains("hello sandbox"));
    }

    #[tokio::test]
    async fn subprocess_sandbox_env_is_clean() {
        // Set a fake secret
        std::env::set_var("FAKE_SECRET_KEY_FOR_TEST", "super_secret_123");

        let sandbox = SubprocessSandbox::new();
        let request = SandboxRequest {
            execution_id: "test-env".to_string(),
            tool_name: "shell_env".to_string(),
            command: SandboxCommand::Shell {
                command: "env".to_string(),
                args: vec![],
            },
            limits: crate::ResourceLimits::default(),
            working_dir: None,
            env: std::collections::HashMap::new(),
            network_allowed: false,
            workspace_root: std::env::temp_dir(),
        };

        let result = sandbox.execute(request).await.unwrap();
        assert!(
            !result.stdout.contains("FAKE_SECRET_KEY_FOR_TEST"),
            "secret leaked in subprocess environment!"
        );
        assert!(
            !result.stdout.contains("super_secret_123"),
            "secret value leaked!"
        );

        // Clean up
        std::env::remove_var("FAKE_SECRET_KEY_FOR_TEST");
    }
}
