//! Health check endpoint probing for daemon liveness.
//!
//! The daemon exposes `GET /health` which this module polls to verify
//! the gateway is actually serving requests (not just process-alive).

use crate::DaemonError;
use serde::{Deserialize, Serialize};

/// Health status returned by the gateway's `/health` endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthStatus {
    pub status: String,
    pub uptime_secs: u64,
    pub version: String,
    #[serde(default)]
    pub channels: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub agents: usize,
    #[serde(default)]
    pub active_sessions: usize,
    #[serde(default)]
    pub memory_mb: u64,
    #[serde(default)]
    pub sochdb: String,
    #[serde(default)]
    pub last_heartbeat: String,
}

impl HealthStatus {
    /// Whether the gateway reports itself as healthy.
    pub fn is_healthy(&self) -> bool {
        self.status == "healthy" || self.status == "ok"
    }
}

/// Health check client that polls the gateway's health endpoint.
pub struct HealthCheck {
    url: String,
    timeout_ms: u64,
}

impl HealthCheck {
    /// Create a health checker pointed at the given gateway URL.
    pub fn new(gateway_url: &str) -> Self {
        Self {
            url: format!("{}/api/v1/health", gateway_url.trim_end_matches('/')),
            timeout_ms: 5000,
        }
    }

    /// Set the HTTP timeout for health check requests.
    pub fn with_timeout_ms(mut self, ms: u64) -> Self {
        self.timeout_ms = ms;
        self
    }

    /// Probe the health endpoint once.
    ///
    /// Returns the parsed `HealthStatus` or an error if unreachable / unhealthy.
    pub async fn probe(&self) -> Result<HealthStatus, DaemonError> {
        // Use a minimal TCP connect + HTTP GET to avoid pulling in reqwest.
        // Parse the URL to extract host and port.
        let url = &self.url;

        let response = tokio::time::timeout(
            std::time::Duration::from_millis(self.timeout_ms),
            Self::http_get(url),
        )
        .await
        .map_err(|_| DaemonError::HealthCheckFailed {
            detail: format!("timeout after {}ms", self.timeout_ms),
        })?
        .map_err(|e| DaemonError::HealthCheckFailed {
            detail: e.to_string(),
        })?;

        let status: HealthStatus = serde_json::from_str(&response).map_err(|e| {
            DaemonError::HealthCheckFailed {
                detail: format!("invalid JSON: {e}"),
            }
        })?;

        if status.is_healthy() {
            Ok(status)
        } else {
            Err(DaemonError::HealthCheckFailed {
                detail: format!("unhealthy: {}", status.status),
            })
        }
    }

    /// Wait for the health endpoint to become healthy, polling up to `max_attempts`.
    pub async fn wait_healthy(
        &self,
        max_attempts: usize,
        interval_ms: u64,
    ) -> Result<HealthStatus, DaemonError> {
        for attempt in 1..=max_attempts {
            match self.probe().await {
                Ok(status) => {
                    tracing::info!(attempt, "health check passed");
                    return Ok(status);
                }
                Err(e) => {
                    tracing::debug!(attempt, %e, "health check failed, retrying");
                    if attempt < max_attempts {
                        tokio::time::sleep(std::time::Duration::from_millis(interval_ms)).await;
                    }
                }
            }
        }
        Err(DaemonError::HealthCheckFailed {
            detail: format!("failed after {max_attempts} attempts"),
        })
    }

    /// Minimal HTTP GET using tokio TCP (avoids heavy deps like reqwest).
    async fn http_get(url: &str) -> Result<String, std::io::Error> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpStream;

        // Parse URL: http://host:port/path
        let url = url.strip_prefix("http://").unwrap_or(url);
        let (host_port, path) = url.split_once('/').unwrap_or((url, ""));
        let path = format!("/{path}");

        let stream = TcpStream::connect(host_port).await?;
        let (mut reader, mut writer) = stream.into_split();

        let host = host_port.split(':').next().unwrap_or("localhost");
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
        );
        writer.write_all(request.as_bytes()).await?;
        writer.shutdown().await?;

        let mut response = String::new();
        reader.read_to_string(&mut response).await?;

        // Extract body (after \r\n\r\n).
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, b)| b.to_string())
            .unwrap_or(response);

        Ok(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_status_healthy() {
        let s = HealthStatus {
            status: "healthy".into(),
            uptime_secs: 100,
            version: "0.1.0".into(),
            channels: Default::default(),
            agents: 2,
            active_sessions: 5,
            memory_mb: 128,
            sochdb: "ok".into(),
            last_heartbeat: "2026-03-05T10:00:00Z".into(),
        };
        assert!(s.is_healthy());
    }

    #[test]
    fn health_status_unhealthy() {
        let s = HealthStatus {
            status: "degraded".into(),
            uptime_secs: 0,
            version: "0.1.0".into(),
            channels: Default::default(),
            agents: 0,
            active_sessions: 0,
            memory_mb: 0,
            sochdb: "error".into(),
            last_heartbeat: String::new(),
        };
        assert!(!s.is_healthy());
    }

    #[test]
    fn parse_health_json() {
        let json = r#"{
            "status": "healthy",
            "uptime_secs": 86423,
            "version": "0.1.0",
            "channels": {"telegram": "connected"},
            "agents": 4,
            "active_sessions": 12,
            "memory_mb": 145,
            "sochdb": "ok",
            "last_heartbeat": "2026-03-05T10:30:00Z"
        }"#;
        let status: HealthStatus = serde_json::from_str(json).unwrap();
        assert!(status.is_healthy());
        assert_eq!(status.agents, 4);
        assert_eq!(status.uptime_secs, 86423);
    }
}
