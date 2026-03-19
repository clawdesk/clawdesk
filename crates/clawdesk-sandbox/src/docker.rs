//! Docker container sandbox — OS-level namespace isolation.
//!
//! Provides kernel-level isolation via Linux namespaces (PID, network, mount, user)
//! with cgroup resource limits. Shells out to Docker CLI (avoids 8MB bollard dependency).

use crate::{
    IsolationLevel, ResourceUsage, Sandbox, SandboxCommand, SandboxError, SandboxRequest,
    SandboxResult,
};
use async_trait::async_trait;
use std::time::Instant;
use tokio::process::Command;
use tracing::{debug, info, warn};

/// Sanitize a container name to prevent injection.
///
/// Only allows `[a-zA-Z0-9-]`, truncates to 63 chars.
fn sanitize_container_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(63)
        .collect();

    if sanitized.is_empty() {
        "clawdesk-sandbox".to_string()
    } else {
        sanitized
    }
}

/// Validate a Docker image name.
///
/// Allows `[a-zA-Z0-9.:/_-]` only.
fn validate_image_name(image: &str) -> Result<(), SandboxError> {
    if image.is_empty() {
        return Err(SandboxError::InvalidConfig("empty image name".into()));
    }

    let valid = image
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || ".:/_-@".contains(c));

    if !valid {
        return Err(SandboxError::CommandInjection {
            pattern: format!("invalid image name: {}", image),
        });
    }

    Ok(())
}

/// Docker container sandbox runtime.
#[derive(Debug, Clone)]
pub struct DockerSandbox {
    /// Container name prefix for agent-scoped cleanup
    pub name_prefix: String,
    /// Default Docker image for shell execution
    pub default_image: String,
    /// Whether to auto-pull images
    pub auto_pull: bool,
}

impl DockerSandbox {
    pub fn new() -> Self {
        Self {
            name_prefix: "clawdesk".to_string(),
            default_image: "alpine:3.19".to_string(),
            auto_pull: true,
        }
    }

    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.name_prefix = prefix.into();
        self
    }

    pub fn with_default_image(mut self, image: impl Into<String>) -> Self {
        self.default_image = image.into();
        self
    }

    /// Generate a unique container name
    fn container_name(&self, execution_id: &str) -> String {
        let raw = format!("{}-{}", self.name_prefix, execution_id);
        sanitize_container_name(&raw)
    }

    /// Forcefully remove a container (cleanup)
    async fn remove_container(&self, name: &str) {
        let _ = Command::new("docker")
            .args(["rm", "-f", name])
            .output()
            .await;
    }
}

impl Default for DockerSandbox {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Sandbox for DockerSandbox {
    fn name(&self) -> &str {
        "docker"
    }

    fn isolation_level(&self) -> IsolationLevel {
        IsolationLevel::ProcessIsolation
    }

    async fn is_available(&self) -> bool {
        Command::new("docker")
            .args(["info"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    async fn execute(&self, request: SandboxRequest) -> Result<SandboxResult, SandboxError> {
        let start = Instant::now();

        let (image, command, args) = match &request.command {
            #[cfg(feature = "sandbox-docker")]
            SandboxCommand::Docker {
                image,
                command,
                args,
            } => (image.as_str(), command.as_str(), args.as_slice()),
            SandboxCommand::Shell { command, args } => {
                (self.default_image.as_str(), command.as_str(), args.as_slice())
            }
            _ => {
                return Err(SandboxError::InvalidConfig(
                    "docker sandbox handles shell and docker commands".into(),
                ))
            }
        };

        // Validate inputs
        validate_image_name(image)?;
        crate::subprocess::validate_command(command)?;
        for arg in args {
            crate::subprocess::validate_command(arg)?;
        }

        let container_name = self.container_name(&request.execution_id);

        // Build docker run command with security flags
        let mut docker_args = vec![
            "run".to_string(),
            "--rm".to_string(),
            format!("--name={}", container_name),
            // Security: drop all capabilities
            "--cap-drop=ALL".to_string(),
            // Security: no new privileges
            "--security-opt=no-new-privileges".to_string(),
            // Resource: memory limit
            format!("--memory={}b", request.limits.memory_bytes),
            // Resource: CPU limit
            format!("--cpus={}", request.limits.cpu_time_secs.min(4)),
            // Resource: PID limit
            format!("--pids-limit={}", request.limits.max_processes),
        ];

        // Network isolation (default: none)
        if !request.network_allowed {
            docker_args.push("--network=none".to_string());
        } else {
            // Enable host gateway so containers can reach host-local services
            // (e.g., the ClawDesk gateway API, local databases, dev servers).
            docker_args.push("--add-host=host.docker.internal:host-gateway".to_string());
        }

        // Mount workspace read-write at /workspace
        let workspace_mount = format!(
            "{}:/workspace",
            request.workspace_root.display()
        );
        docker_args.push(format!("--volume={}:rw", workspace_mount));
        docker_args.push("--workdir=/workspace".to_string());

        // Environment variables (only explicitly passed)
        for (key, value) in &request.env {
            docker_args.push(format!("--env={}={}", key, value));
        }

        // Image
        docker_args.push(image.to_string());

        // Command and args
        docker_args.push(command.to_string());
        docker_args.extend(args.iter().cloned());

        debug!(
            container = %container_name,
            image = %image,
            command = %command,
            "executing in docker sandbox"
        );

        // Execute with timeout
        let timeout = std::time::Duration::from_secs(request.limits.wall_time_secs);

        let result = tokio::time::timeout(timeout, async {
            Command::new("docker")
                .args(&docker_args)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output()
                .await
        })
        .await;

        match result {
            Ok(Ok(output)) => {
                let elapsed = start.elapsed();
                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();

                Ok(SandboxResult {
                    exit_code: output.status.code().unwrap_or(-1),
                    stdout,
                    stderr,
                    duration: elapsed,
                    resource_usage: ResourceUsage {
                        wall_time_ms: elapsed.as_millis() as u64,
                        output_bytes: output.stdout.len() as u64,
                        ..Default::default()
                    },
                })
            }
            Ok(Err(e)) => {
                // Cleanup on error
                self.remove_container(&container_name).await;
                Err(SandboxError::ExecutionFailed(e.to_string()))
            }
            Err(_) => {
                // Timeout — kill container
                warn!(container = %container_name, "docker execution timed out, killing container");
                self.remove_container(&container_name).await;
                Err(SandboxError::Timeout(timeout))
            }
        }
    }

    async fn cleanup(&self) -> Result<(), SandboxError> {
        // Remove all containers with our prefix
        info!(prefix = %self.name_prefix, "cleaning up docker containers");
        let output = Command::new("docker")
            .args([
                "ps",
                "-aq",
                "--filter",
                &format!("name={}", self.name_prefix),
            ])
            .output()
            .await
            .map_err(|e| SandboxError::ExecutionFailed(e.to_string()))?;

        let ids = String::from_utf8_lossy(&output.stdout);
        for id in ids.lines() {
            let id = id.trim();
            if !id.is_empty() {
                let _ = Command::new("docker")
                    .args(["rm", "-f", id])
                    .output()
                    .await;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_container_name_basic() {
        assert_eq!(sanitize_container_name("my-container"), "my-container");
        assert_eq!(sanitize_container_name("a!b@c#d"), "abcd");
        assert_eq!(sanitize_container_name(""), "clawdesk-sandbox");
    }

    #[test]
    fn sanitize_container_name_length() {
        let long = "a".repeat(100);
        assert_eq!(sanitize_container_name(&long).len(), 63);
    }

    #[test]
    fn validate_image_name_valid() {
        assert!(validate_image_name("alpine:3.19").is_ok());
        assert!(validate_image_name("python:3.12-slim").is_ok());
        assert!(validate_image_name("ghcr.io/owner/image:latest").is_ok());
        assert!(validate_image_name("registry.example.com/ns/img:v1.0").is_ok());
    }

    #[test]
    fn validate_image_name_invalid() {
        assert!(validate_image_name("").is_err());
        assert!(validate_image_name("image;rm -rf /").is_err());
        assert!(validate_image_name("image$(evil)").is_err());
    }
}
