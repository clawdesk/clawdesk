//! Thread ownership manager for multi-agent collision prevention.
//!
//! Guarantees that at most one owner holds a lease for a given thread key
//! at a time (with TTL for liveness).

use std::collections::HashMap;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

#[derive(Debug, Clone)]
struct ThreadLease {
    owner_id: String,
    expires_at: Instant,
    acquired_at: Instant,
}

/// Result of attempting to acquire thread ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireResult {
    Acquired,
    AlreadyOwned,
    Busy {
        owner_id: String,
        retry_after_ms: u64,
    },
}

/// Lease-based thread ownership manager.
pub struct ThreadOwnershipManager {
    ttl: Duration,
    max_entries: usize,
    leases: RwLock<HashMap<String, ThreadLease>>,
}

impl ThreadOwnershipManager {
    pub fn new(ttl: Duration, max_entries: usize) -> Self {
        Self {
            ttl,
            max_entries,
            leases: RwLock::new(HashMap::new()),
        }
    }

    /// Attempt to acquire a lease for `thread_id`.
    pub async fn try_acquire(&self, thread_id: &str, owner_id: &str) -> AcquireResult {
        let mut leases = self.leases.write().await;
        let now = Instant::now();
        Self::prune_expired(&mut leases, now);

        if let Some(existing) = leases.get_mut(thread_id) {
            if existing.owner_id == owner_id {
                existing.expires_at = now + self.ttl;
                return AcquireResult::AlreadyOwned;
            }
            let retry_after_ms = existing
                .expires_at
                .saturating_duration_since(now)
                .as_millis() as u64;
            return AcquireResult::Busy {
                owner_id: existing.owner_id.clone(),
                retry_after_ms,
            };
        }

        Self::enforce_capacity(&mut leases, self.max_entries);
        leases.insert(
            thread_id.to_string(),
            ThreadLease {
                owner_id: owner_id.to_string(),
                acquired_at: now,
                expires_at: now + self.ttl,
            },
        );
        AcquireResult::Acquired
    }

    /// Renew an existing lease if owned by `owner_id`.
    pub async fn renew(&self, thread_id: &str, owner_id: &str) -> bool {
        let mut leases = self.leases.write().await;
        let now = Instant::now();
        Self::prune_expired(&mut leases, now);
        let Some(lease) = leases.get_mut(thread_id) else {
            return false;
        };
        if lease.owner_id != owner_id {
            return false;
        }
        lease.expires_at = now + self.ttl;
        true
    }

    /// Release a lease if owned by `owner_id`.
    pub async fn release(&self, thread_id: &str, owner_id: &str) -> bool {
        let mut leases = self.leases.write().await;
        let now = Instant::now();
        Self::prune_expired(&mut leases, now);
        let Some(lease) = leases.get(thread_id) else {
            return false;
        };
        if lease.owner_id != owner_id {
            return false;
        }
        leases.remove(thread_id);
        true
    }

    fn prune_expired(leases: &mut HashMap<String, ThreadLease>, now: Instant) {
        leases.retain(|_, lease| lease.expires_at > now);
    }

    fn enforce_capacity(leases: &mut HashMap<String, ThreadLease>, max_entries: usize) {
        if leases.len() < max_entries {
            return;
        }
        if let Some(oldest_key) = leases
            .iter()
            .min_by_key(|(_, lease)| lease.acquired_at)
            .map(|(k, _)| k.clone())
        {
            leases.remove(&oldest_key);
        }
    }
}

impl Default for ThreadOwnershipManager {
    fn default() -> Self {
        Self::new(Duration::from_secs(30), 10_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn acquires_and_releases_lease() {
        let manager = ThreadOwnershipManager::new(Duration::from_secs(1), 100);
        let acquired = manager.try_acquire("thread-a", "agent-1").await;
        assert_eq!(acquired, AcquireResult::Acquired);
        assert!(manager.release("thread-a", "agent-1").await);
    }

    #[tokio::test]
    async fn blocks_other_owner_while_lease_is_active() {
        let manager = ThreadOwnershipManager::new(Duration::from_secs(2), 100);
        assert_eq!(
            manager.try_acquire("thread-a", "agent-1").await,
            AcquireResult::Acquired
        );
        let second = manager.try_acquire("thread-a", "agent-2").await;
        match second {
            AcquireResult::Busy { owner_id, .. } => assert_eq!(owner_id, "agent-1"),
            _ => panic!("expected busy"),
        }
    }

    #[tokio::test]
    async fn lease_expires_and_next_owner_can_acquire() {
        let manager = ThreadOwnershipManager::new(Duration::from_millis(60), 100);
        assert_eq!(
            manager.try_acquire("thread-a", "agent-1").await,
            AcquireResult::Acquired
        );
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            manager.try_acquire("thread-a", "agent-2").await,
            AcquireResult::Acquired
        );
    }
}

