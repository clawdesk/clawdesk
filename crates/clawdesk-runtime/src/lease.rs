//! Lease manager — distributed run ownership with fence tokens.
//!
//! Only one worker may actively execute a run at a time. A lease encodes
//! who owns it and when it expires. Fence tokens provide monotonic ordering
//! so a stale worker cannot corrupt a run's state.
//!
//! ## Storage
//!
//! ```text
//! runtime:leases:{run_id}  →  Lease (JSON)
//! ```

use crate::types::{Lease, RunId, RuntimeError};
use chrono::Utc;
use clawdesk_sochdb::SochStore;
use clawdesk_types::error::StorageError;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{debug, warn};

/// Global monotonic fence-token counter (process-scoped).
static FENCE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Manages per-run worker leases.
pub struct LeaseManager {
    store: Arc<SochStore>,
    default_ttl_secs: u64,
}

impl LeaseManager {
    pub fn new(store: Arc<SochStore>, default_ttl_secs: u64) -> Self {
        Self {
            store,
            default_ttl_secs,
        }
    }

    /// Attempt to acquire a lease for `run_id` on behalf of `worker_id`.
    ///
    /// Succeeds if no lease exists or the existing lease has expired.
    /// Returns the acquired Lease on success, or `RuntimeError::LeaseConflict`
    /// if a valid lease is held by another worker.
    pub async fn acquire(
        &self,
        run_id: &RunId,
        worker_id: &str,
    ) -> Result<Lease, RuntimeError> {
        let key = Self::lease_key(run_id);

        // Check for existing lease.
        if let Ok(Some(bytes)) = self.store.db().get(key.as_bytes()) {
            if let Ok(existing) = serde_json::from_slice::<Lease>(&bytes) {
                if existing.is_valid() && !existing.is_held_by(worker_id) {
                    return Err(RuntimeError::LeaseConflict {
                        run_id: run_id.clone(),
                        holder: existing.worker_id.clone(),
                    });
                }
                // If held by same worker, renew instead.
                if existing.is_valid() && existing.is_held_by(worker_id) {
                    return self.renew(run_id, worker_id, existing.fence_token).await;
                }
            }
        }

        // No lease or expired — grant new lease.
        let fence = FENCE_COUNTER.fetch_add(1, Ordering::SeqCst);
        let lease = Lease::new(run_id.clone(), worker_id.to_string(), self.default_ttl_secs, fence);

        let bytes = serde_json::to_vec(&lease).map_err(|e| StorageError::SerializationFailed {
            detail: e.to_string(),
        })?;
        self.store
            .db()
            .put(key.as_bytes(), &bytes)
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        debug!(%run_id, worker_id, fence, "lease acquired");
        Ok(lease)
    }

    /// Renew an existing lease. The caller must present the correct `fence_token`
    /// to prove they are the legitimate holder.
    pub async fn renew(
        &self,
        run_id: &RunId,
        worker_id: &str,
        expected_fence: u64,
    ) -> Result<Lease, RuntimeError> {
        let key = Self::lease_key(run_id);

        let bytes = self
            .store
            .db()
            .get(key.as_bytes())
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?
            .ok_or_else(|| RuntimeError::RunNotFound {
                run_id: run_id.clone(),
            })?;

        let existing = serde_json::from_slice::<Lease>(&bytes).map_err(|e| {
            StorageError::SerializationFailed {
                detail: e.to_string(),
            }
        })?;

        // Fence token check.
        if existing.fence_token != expected_fence {
            warn!(
                %run_id,
                expected = expected_fence,
                actual = existing.fence_token,
                "stale fence token on renew"
            );
            return Err(RuntimeError::StaleLease {
                run_id: run_id.clone(),
                expected_fence,
                actual_fence: existing.fence_token,
            });
        }

        if !existing.is_held_by(worker_id) {
            return Err(RuntimeError::LeaseConflict {
                run_id: run_id.clone(),
                holder: existing.worker_id.clone(),
            });
        }

        let renewed = existing.renew(self.default_ttl_secs);
        let new_bytes =
            serde_json::to_vec(&renewed).map_err(|e| StorageError::SerializationFailed {
                detail: e.to_string(),
            })?;
        self.store
            .db()
            .put(key.as_bytes(), &new_bytes)
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        debug!(%run_id, worker_id, "lease renewed");
        Ok(renewed)
    }

    /// Release a lease. The caller must present the correct fence_token.
    pub async fn release(
        &self,
        run_id: &RunId,
        worker_id: &str,
        fence_token: u64,
    ) -> Result<(), RuntimeError> {
        let key = Self::lease_key(run_id);

        if let Ok(Some(bytes)) = self.store.db().get(key.as_bytes()) {
            if let Ok(existing) = serde_json::from_slice::<Lease>(&bytes) {
                if existing.fence_token != fence_token {
                    return Err(RuntimeError::StaleLease {
                        run_id: run_id.clone(),
                        expected_fence: fence_token,
                        actual_fence: existing.fence_token,
                    });
                }
                if !existing.is_held_by(worker_id) {
                    return Err(RuntimeError::LeaseConflict {
                        run_id: run_id.clone(),
                        holder: existing.worker_id,
                    });
                }
            }
        }

        let _ = self.store.db().delete(key.as_bytes());
        debug!(%run_id, worker_id, "lease released");
        Ok(())
    }

    /// Load the current lease for a run (if any).
    pub async fn load_lease(
        &self,
        run_id: &RunId,
    ) -> Result<Option<Lease>, RuntimeError> {
        let key = Self::lease_key(run_id);
        match self.store.db().get(key.as_bytes()) {
            Ok(Some(bytes)) => {
                let lease = serde_json::from_slice(&bytes).map_err(|e| {
                    RuntimeError::CheckpointCorrupted {
                        detail: format!("lease deserialization: {e}"),
                    }
                })?;
                Ok(Some(lease))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(StorageError::OpenFailed {
                detail: e.to_string(),
            }
            .into()),
        }
    }

    /// Scan for all expired leases. Returns (run_id, lease) pairs.
    pub async fn scan_expired(&self) -> Result<Vec<(RunId, Lease)>, RuntimeError> {
        let prefix = "runtime:leases:";
        let entries = self
            .store
            .db()
            .scan(prefix.as_bytes())
            .map_err(|e| StorageError::OpenFailed {
                detail: e.to_string(),
            })?;

        let now = Utc::now();
        let mut expired = Vec::new();

        for (_key, val) in &entries {
            if let Ok(lease) = serde_json::from_slice::<Lease>(val) {
                if lease.expires_at < now {
                    expired.push((lease.run_id.clone(), lease));
                }
            }
        }

        Ok(expired)
    }

    /// Validate that a given fence_token is still current for a run.
    /// Returns the lease if valid, or an appropriate error.
    pub async fn validate_fence(
        &self,
        run_id: &RunId,
        fence_token: u64,
    ) -> Result<Lease, RuntimeError> {
        let lease = self
            .load_lease(run_id)
            .await?
            .ok_or_else(|| RuntimeError::RunNotFound {
                run_id: run_id.clone(),
            })?;

        if lease.fence_token != fence_token {
            return Err(RuntimeError::StaleLease {
                run_id: run_id.clone(),
                expected_fence: fence_token,
                actual_fence: lease.fence_token,
            });
        }

        if !lease.is_valid() {
            return Err(RuntimeError::StaleLease {
                run_id: run_id.clone(),
                expected_fence: fence_token,
                actual_fence: lease.fence_token,
            });
        }

        Ok(lease)
    }

    fn lease_key(run_id: &RunId) -> String {
        format!("runtime:leases:{}", run_id)
    }
}

// ── Tests ────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> Arc<SochStore> {
        Arc::new(SochStore::open_in_memory().expect("in-memory store"))
    }

    #[tokio::test]
    async fn acquire_and_release() {
        let store = test_store();
        let lm = LeaseManager::new(store, 30);
        let run_id = RunId::new();

        let lease = lm.acquire(&run_id, "worker-1").await.unwrap();
        assert!(lease.is_valid());
        assert!(lease.is_held_by("worker-1"));

        lm.release(&run_id, "worker-1", lease.fence_token)
            .await
            .unwrap();
        assert!(lm.load_lease(&run_id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn lease_conflict() {
        let store = test_store();
        let lm = LeaseManager::new(store, 30);
        let run_id = RunId::new();

        lm.acquire(&run_id, "worker-1").await.unwrap();

        // Another worker should be rejected.
        let result = lm.acquire(&run_id, "worker-2").await;
        assert!(matches!(result, Err(RuntimeError::LeaseConflict { .. })));
    }

    #[tokio::test]
    async fn renew_with_correct_fence() {
        let store = test_store();
        let lm = LeaseManager::new(store, 30);
        let run_id = RunId::new();

        let lease = lm.acquire(&run_id, "worker-1").await.unwrap();
        let renewed = lm
            .renew(&run_id, "worker-1", lease.fence_token)
            .await
            .unwrap();
        assert!(renewed.expires_at >= lease.expires_at);
        assert_eq!(renewed.fence_token, lease.fence_token);
    }

    #[tokio::test]
    async fn renew_with_stale_fence() {
        let store = test_store();
        let lm = LeaseManager::new(store, 30);
        let run_id = RunId::new();

        let _lease = lm.acquire(&run_id, "worker-1").await.unwrap();
        let result = lm.renew(&run_id, "worker-1", 9999).await;
        assert!(matches!(result, Err(RuntimeError::StaleLease { .. })));
    }

    #[tokio::test]
    async fn validate_fence_ok() {
        let store = test_store();
        let lm = LeaseManager::new(store, 30);
        let run_id = RunId::new();

        let lease = lm.acquire(&run_id, "worker-1").await.unwrap();
        let validated = lm.validate_fence(&run_id, lease.fence_token).await;
        assert!(validated.is_ok());
    }
}
