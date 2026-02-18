//! Human-in-the-loop execution approval manager.
//!
//! Requests are created in `pending` state and transition to:
//! - `approved`
//! - `denied`
//! - `timed_out`
//!
//! The manager supports asynchronous wait with deadline enforcement.

use crate::command_policy::RiskLevel;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Notify, RwLock};
use uuid::Uuid;

/// Current state of an execution approval request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved { by: String, at: DateTime<Utc> },
    Denied {
        by: String,
        at: DateTime<Utc>,
        reason: Option<String>,
    },
    TimedOut { at: DateTime<Utc> },
}

impl ApprovalStatus {
    pub fn is_terminal(&self) -> bool {
        !matches!(self, Self::Pending)
    }
}

/// A single approval request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: Uuid,
    pub tool_name: String,
    pub command: String,
    pub risk: RiskLevel,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub context: Option<String>,
}

#[derive(Debug)]
struct ApprovalEntry {
    request: ApprovalRequest,
    status: ApprovalStatus,
    notify: Arc<Notify>,
}

/// Errors returned by approval operations.
#[derive(Debug, thiserror::Error)]
pub enum ApprovalError {
    #[error("approval request not found: {0}")]
    NotFound(Uuid),
    #[error("approval request already finalized: {0}")]
    AlreadyFinalized(Uuid),
}

/// Manager for approval-gated execution requests.
pub struct ExecApprovalManager {
    entries: RwLock<HashMap<Uuid, ApprovalEntry>>,
    default_ttl: Duration,
}

impl ExecApprovalManager {
    pub fn new(default_ttl: Duration) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            default_ttl,
        }
    }

    /// Create a new pending approval request.
    pub async fn create_request(
        &self,
        tool_name: impl Into<String>,
        command: impl Into<String>,
        risk: RiskLevel,
        context: Option<String>,
    ) -> ApprovalRequest {
        let created_at = Utc::now();
        let ttl = chrono::Duration::from_std(self.default_ttl).unwrap_or_else(|_| chrono::Duration::seconds(60));
        let request = ApprovalRequest {
            id: Uuid::new_v4(),
            tool_name: tool_name.into(),
            command: command.into(),
            risk,
            created_at,
            expires_at: created_at + ttl,
            context,
        };

        let entry = ApprovalEntry {
            request: request.clone(),
            status: ApprovalStatus::Pending,
            notify: Arc::new(Notify::new()),
        };
        self.entries.write().await.insert(request.id, entry);
        request
    }

    /// Read current status (or infer timeout if pending and expired).
    pub async fn status(&self, request_id: Uuid) -> Option<ApprovalStatus> {
        self.ensure_timeout_if_needed(request_id).await;
        self.entries
            .read()
            .await
            .get(&request_id)
            .map(|entry| entry.status.clone())
    }

    /// Approve a request.
    pub async fn approve(&self, request_id: Uuid, approver: impl Into<String>) -> Result<(), ApprovalError> {
        let mut entries = self.entries.write().await;
        let Some(entry) = entries.get_mut(&request_id) else {
            return Err(ApprovalError::NotFound(request_id));
        };
        if entry.status.is_terminal() {
            return Err(ApprovalError::AlreadyFinalized(request_id));
        }
        entry.status = ApprovalStatus::Approved {
            by: approver.into(),
            at: Utc::now(),
        };
        entry.notify.notify_waiters();
        Ok(())
    }

    /// Deny a request.
    pub async fn deny(
        &self,
        request_id: Uuid,
        approver: impl Into<String>,
        reason: Option<String>,
    ) -> Result<(), ApprovalError> {
        let mut entries = self.entries.write().await;
        let Some(entry) = entries.get_mut(&request_id) else {
            return Err(ApprovalError::NotFound(request_id));
        };
        if entry.status.is_terminal() {
            return Err(ApprovalError::AlreadyFinalized(request_id));
        }
        entry.status = ApprovalStatus::Denied {
            by: approver.into(),
            at: Utc::now(),
            reason,
        };
        entry.notify.notify_waiters();
        Ok(())
    }

    /// Wait until the request is approved/denied/timed_out.
    pub async fn wait_for_decision(&self, request_id: Uuid) -> Result<ApprovalStatus, ApprovalError> {
        loop {
            self.ensure_timeout_if_needed(request_id).await;
            let (status, notify, expires_at) = {
                let entries = self.entries.read().await;
                let Some(entry) = entries.get(&request_id) else {
                    return Err(ApprovalError::NotFound(request_id));
                };
                (
                    entry.status.clone(),
                    Arc::clone(&entry.notify),
                    entry.request.expires_at,
                )
            };

            if status.is_terminal() {
                return Ok(status);
            }

            let now = Utc::now();
            if now >= expires_at {
                self.ensure_timeout_if_needed(request_id).await;
                continue;
            }

            let remaining = (expires_at - now)
                .to_std()
                .unwrap_or_else(|_| Duration::from_millis(1));
            let _ = tokio::time::timeout(remaining, notify.notified()).await;
        }
    }

    /// Remove terminal requests older than the provided age.
    pub async fn cleanup_terminal_older_than(&self, age: Duration) {
        let age = chrono::Duration::from_std(age).unwrap_or_else(|_| chrono::Duration::seconds(60));
        let cutoff = Utc::now() - age;
        self.entries.write().await.retain(|_, entry| {
            match &entry.status {
                ApprovalStatus::Pending => true,
                ApprovalStatus::Approved { at, .. } => *at >= cutoff,
                ApprovalStatus::Denied { at, .. } => *at >= cutoff,
                ApprovalStatus::TimedOut { at } => *at >= cutoff,
            }
        });
    }

    async fn ensure_timeout_if_needed(&self, request_id: Uuid) {
        let mut entries = self.entries.write().await;
        let Some(entry) = entries.get_mut(&request_id) else {
            return;
        };
        if matches!(entry.status, ApprovalStatus::Pending) && Utc::now() >= entry.request.expires_at {
            entry.status = ApprovalStatus::TimedOut { at: Utc::now() };
            entry.notify.notify_waiters();
        }
    }
}

impl Default for ExecApprovalManager {
    fn default() -> Self {
        Self::new(Duration::from_secs(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn approve_flow_completes() {
        let mgr = ExecApprovalManager::new(Duration::from_secs(2));
        let req = mgr
            .create_request("shell_exec", "ls -la", RiskLevel::Low, None)
            .await;
        mgr.approve(req.id, "alice").await.unwrap();
        let status = mgr.wait_for_decision(req.id).await.unwrap();
        match status {
            ApprovalStatus::Approved { by, .. } => assert_eq!(by, "alice"),
            _ => panic!("expected approved"),
        }
    }

    #[tokio::test]
    async fn deny_flow_completes() {
        let mgr = ExecApprovalManager::new(Duration::from_secs(2));
        let req = mgr
            .create_request("shell_exec", "rm -rf /tmp/x", RiskLevel::High, None)
            .await;
        mgr.deny(req.id, "bob", Some("unsafe".to_string()))
            .await
            .unwrap();
        let status = mgr.wait_for_decision(req.id).await.unwrap();
        match status {
            ApprovalStatus::Denied { by, reason, .. } => {
                assert_eq!(by, "bob");
                assert_eq!(reason.as_deref(), Some("unsafe"));
            }
            _ => panic!("expected denied"),
        }
    }

    #[tokio::test]
    async fn pending_request_times_out() {
        let mgr = ExecApprovalManager::new(Duration::from_millis(60));
        let req = mgr
            .create_request("shell_exec", "curl https://example.com", RiskLevel::Medium, None)
            .await;
        let status = mgr.wait_for_decision(req.id).await.unwrap();
        assert!(matches!(status, ApprovalStatus::TimedOut { .. }));
    }
}

