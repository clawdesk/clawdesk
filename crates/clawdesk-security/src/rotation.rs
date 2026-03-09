//! Secrets rotation and lifecycle management.
//!
//! Extends the credential vault with:
//! - Automatic API key rotation scheduling
//! - Key expiry detection and alerting
//! - Rotation history for audit compliance
//! - Graceful dual-key transition periods

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, Instant, SystemTime};
use tracing::{info, warn};

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

/// Rotation policy for a credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotationPolicy {
    /// How often to rotate this credential.
    pub rotation_interval: Duration,
    /// Grace period during which both old and new keys are valid.
    pub grace_period: Duration,
    /// Whether to alert before expiry.
    pub alert_before_expiry: bool,
    /// How long before expiry to send the alert.
    pub alert_lead_time: Duration,
}

impl Default for RotationPolicy {
    fn default() -> Self {
        Self {
            rotation_interval: Duration::from_secs(90 * 86400), // 90 days
            grace_period: Duration::from_secs(24 * 3600),        // 24 hours
            alert_before_expiry: true,
            alert_lead_time: Duration::from_secs(7 * 86400),    // 7 days
        }
    }
}

/// State of a managed secret.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SecretState {
    /// Active and in use.
    Active,
    /// Rotation requested, waiting for new key.
    PendingRotation,
    /// New key is active, old key still valid during grace period.
    GracePeriod,
    /// Expired — should not be used.
    Expired,
    /// Manually revoked.
    Revoked,
}

/// Record of a rotation event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RotationRecord {
    pub provider: String,
    pub credential_id: String,
    pub rotated_at: u64,
    pub previous_state: SecretState,
    pub new_state: SecretState,
    pub reason: String,
}

/// Managed secret with lifecycle tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManagedSecret {
    pub provider: String,
    pub credential_id: String,
    pub state: SecretState,
    pub created_at: u64,
    pub last_rotated_at: Option<u64>,
    pub expires_at: Option<u64>,
    pub policy: RotationPolicy,
    pub rotation_count: u32,
}

impl ManagedSecret {
    /// Check if this secret needs rotation.
    pub fn needs_rotation(&self, now: u64) -> bool {
        if self.state == SecretState::Revoked || self.state == SecretState::Expired {
            return false;
        }
        if let Some(expires_at) = self.expires_at {
            if now >= expires_at {
                return true;
            }
        }
        let last = self.last_rotated_at.unwrap_or(self.created_at);
        let elapsed_secs = now.saturating_sub(last);
        elapsed_secs >= self.policy.rotation_interval.as_secs()
    }

    /// Check if an expiry alert should fire.
    pub fn should_alert(&self, now: u64) -> bool {
        if !self.policy.alert_before_expiry {
            return false;
        }
        if let Some(expires_at) = self.expires_at {
            let alert_at = expires_at.saturating_sub(self.policy.alert_lead_time.as_secs());
            return now >= alert_at && now < expires_at;
        }
        false
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Rotation manager
// ─────────────────────────────────────────────────────────────────────────────

/// Manages secret lifecycle and rotation across all providers.
pub struct SecretRotationManager {
    secrets: HashMap<String, ManagedSecret>,
    history: Vec<RotationRecord>,
}

impl SecretRotationManager {
    pub fn new() -> Self {
        Self {
            secrets: HashMap::new(),
            history: Vec::new(),
        }
    }

    /// Register a secret for lifecycle management.
    pub fn register(
        &mut self,
        provider: impl Into<String>,
        credential_id: impl Into<String>,
        policy: RotationPolicy,
        now: u64,
    ) {
        let provider = provider.into();
        let credential_id = credential_id.into();
        let key = format!("{provider}/{credential_id}");

        let managed = ManagedSecret {
            provider,
            credential_id,
            state: SecretState::Active,
            created_at: now,
            last_rotated_at: None,
            expires_at: Some(now + policy.rotation_interval.as_secs()),
            policy,
            rotation_count: 0,
        };

        self.secrets.insert(key, managed);
    }

    /// Get all secrets that need rotation.
    pub fn secrets_needing_rotation(&self, now: u64) -> Vec<&ManagedSecret> {
        self.secrets
            .values()
            .filter(|s| s.needs_rotation(now))
            .collect()
    }

    /// Get all secrets that should trigger alerts.
    pub fn secrets_needing_alert(&self, now: u64) -> Vec<&ManagedSecret> {
        self.secrets
            .values()
            .filter(|s| s.should_alert(now))
            .collect()
    }

    /// Mark a secret as rotated.
    pub fn mark_rotated(&mut self, provider: &str, credential_id: &str, now: u64) {
        let key = format!("{provider}/{credential_id}");
        if let Some(secret) = self.secrets.get_mut(&key) {
            let prev_state = secret.state;
            secret.state = SecretState::Active;
            secret.last_rotated_at = Some(now);
            secret.expires_at = Some(now + secret.policy.rotation_interval.as_secs());
            secret.rotation_count += 1;

            self.history.push(RotationRecord {
                provider: provider.into(),
                credential_id: credential_id.into(),
                rotated_at: now,
                previous_state: prev_state,
                new_state: SecretState::Active,
                reason: "scheduled rotation".into(),
            });

            info!(
                provider,
                credential_id,
                count = secret.rotation_count,
                "secret rotated"
            );
        }
    }

    /// Revoke a secret immediately.
    pub fn revoke(&mut self, provider: &str, credential_id: &str, now: u64, reason: &str) {
        let key = format!("{provider}/{credential_id}");
        if let Some(secret) = self.secrets.get_mut(&key) {
            let prev_state = secret.state;
            secret.state = SecretState::Revoked;

            self.history.push(RotationRecord {
                provider: provider.into(),
                credential_id: credential_id.into(),
                rotated_at: now,
                previous_state: prev_state,
                new_state: SecretState::Revoked,
                reason: reason.into(),
            });

            warn!(provider, credential_id, reason, "secret revoked");
        }
    }

    /// Get rotation history.
    pub fn history(&self) -> &[RotationRecord] {
        &self.history
    }

    /// Get a managed secret by key.
    pub fn get(&self, provider: &str, credential_id: &str) -> Option<&ManagedSecret> {
        let key = format!("{provider}/{credential_id}");
        self.secrets.get(&key)
    }
}

impl Default for SecretRotationManager {
    fn default() -> Self {
        Self::new()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_needed_after_interval() {
        let mut mgr = SecretRotationManager::new();
        let policy = RotationPolicy {
            rotation_interval: Duration::from_secs(100),
            ..Default::default()
        };
        mgr.register("openai", "key-1", policy, 1000);

        assert!(mgr.secrets_needing_rotation(1000).is_empty());
        assert_eq!(mgr.secrets_needing_rotation(1101).len(), 1);
    }

    #[test]
    fn mark_rotated_resets_timer() {
        let mut mgr = SecretRotationManager::new();
        let policy = RotationPolicy {
            rotation_interval: Duration::from_secs(100),
            ..Default::default()
        };
        mgr.register("openai", "key-1", policy, 1000);
        mgr.mark_rotated("openai", "key-1", 1101);

        let secret = mgr.get("openai", "key-1").unwrap();
        assert_eq!(secret.rotation_count, 1);
        assert_eq!(secret.state, SecretState::Active);
        assert!(mgr.secrets_needing_rotation(1101).is_empty());
    }

    #[test]
    fn revoke_prevents_rotation() {
        let mut mgr = SecretRotationManager::new();
        mgr.register("openai", "key-1", RotationPolicy::default(), 1000);
        mgr.revoke("openai", "key-1", 1001, "compromised");

        let secret = mgr.get("openai", "key-1").unwrap();
        assert_eq!(secret.state, SecretState::Revoked);
        assert!(!secret.needs_rotation(999999));
    }

    #[test]
    fn alert_before_expiry() {
        let mut mgr = SecretRotationManager::new();
        let policy = RotationPolicy {
            rotation_interval: Duration::from_secs(1000),
            alert_before_expiry: true,
            alert_lead_time: Duration::from_secs(100),
            ..Default::default()
        };
        mgr.register("anthropic", "key-1", policy, 0);

        // Not yet in alert window
        assert!(mgr.secrets_needing_alert(800).is_empty());
        // In alert window (< 100 secs before expiry at t=1000)
        assert_eq!(mgr.secrets_needing_alert(910).len(), 1);
        // Past expiry
        assert!(mgr.secrets_needing_alert(1001).is_empty());
    }

    #[test]
    fn history_tracking() {
        let mut mgr = SecretRotationManager::new();
        mgr.register("openai", "key-1", RotationPolicy::default(), 0);
        mgr.mark_rotated("openai", "key-1", 100);
        mgr.revoke("openai", "key-1", 200, "leaked");

        assert_eq!(mgr.history().len(), 2);
        assert_eq!(mgr.history()[1].new_state, SecretState::Revoked);
    }
}
