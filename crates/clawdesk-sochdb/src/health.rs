//! Storage health monitoring and reporting.
//!
//! Provides a unified view of all storage subsystems' health, used by both
//! the Tauri IPC layer (for UI banners) and internal diagnostics.
//!
//! ## Health States
//!
//! - **Healthy**: Store is open on durable persistent storage, all operations work.
//! - **Degraded**: Store is running on ephemeral/temp storage. Data won't survive restart.
//! - **Failed**: Store failed to initialize entirely.
//!
//! ## Usage
//!
//! ```rust,ignore
//! let health = StorageHealth::check(&soch_store, Some(&thread_store));
//! if health.any_ephemeral() {
//!     // Show warning banner in UI
//! }
//! ```

use crate::SochStore;
use serde::{Deserialize, Serialize};
use tracing::warn;

/// Health status of a single storage subsystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StoreStatus {
    /// Fully operational on persistent storage.
    Healthy,
    /// Running on ephemeral (temp) storage — data won't survive restart.
    Ephemeral,
    /// Store failed to initialize.
    Failed,
}

impl StoreStatus {
    pub fn is_healthy(&self) -> bool {
        matches!(self, StoreStatus::Healthy)
    }
}

/// Per-store health detail.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreHealth {
    /// Name of the store subsystem.
    pub name: String,
    /// Current health status.
    pub status: StoreStatus,
    /// Filesystem path (if known).
    pub path: Option<String>,
    /// Number of keys (if cheaply available).
    pub key_count: Option<u64>,
    /// Human-readable detail message.
    pub detail: String,
}

/// Aggregated health of all storage subsystems.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageHealth {
    /// Overall health: healthy only if ALL stores are healthy.
    pub overall: StoreStatus,
    /// Per-store details.
    pub stores: Vec<StoreHealth>,
    /// True if any store is running on ephemeral storage.
    pub any_ephemeral: bool,
    /// Recommendations for the user (if any).
    pub recommendations: Vec<String>,
    /// Timestamp of this health check (RFC 3339).
    pub checked_at: String,
}

impl StorageHealth {
    /// Perform a health check on all storage subsystems.
    pub fn check(
        soch_store: &SochStore,
        thread_store: Option<&clawdesk_threads::ThreadStore>,
    ) -> Self {
        let mut stores = Vec::new();
        let mut recommendations = Vec::new();
        let mut any_ephemeral = false;

        // ── SochStore (main KV) ─────────────────────────────────────
        let soch_status = if soch_store.is_ephemeral() {
            any_ephemeral = true;
            recommendations.push(
                "SochDB is running on ephemeral storage. Sessions, memory, and settings \
                 will NOT survive a restart. Check disk permissions and free space at \
                 ~/.clawdesk/sochdb/".to_string()
            );
            StoreStatus::Ephemeral
        } else {
            StoreStatus::Healthy
        };

        // GAP-11: Estimate key count from per-prefix bounded scans instead of
        // scanning the ENTIRE database (scan("") allocates O(K×V) memory just
        // to return .len()). Sum counts of known key prefixes with bounded scans.
        let soch_key_count = {
            let known_prefixes = [
                "sessions/", "chats/", "config/", "agents/", "turns/",
                "graph/", "trace/", "memory/", "vectors/", "idx/",
            ];
            let mut total = 0u64;
            for prefix in &known_prefixes {
                if let Ok(entries) = soch_store.scan(prefix) {
                    total += entries.len() as u64;
                }
            }
            Some(total)
        };

        stores.push(StoreHealth {
            name: "SochStore".to_string(),
            status: soch_status,
            path: Some(soch_store.store_path().display().to_string()),
            key_count: soch_key_count,
            detail: if soch_status.is_healthy() {
                "ACID storage active — data is durable".to_string()
            } else {
                "Ephemeral mode — data loss on restart!".to_string()
            },
        });

        // ── ThreadStore ─────────────────────────────────────────────
        if let Some(ts) = thread_store {
            let ts_status = if ts.is_ephemeral() {
                any_ephemeral = true;
                recommendations.push(
                    "ThreadStore is running on temp storage. Chat threads and messages \
                     will NOT survive a restart. Check ~/.clawdesk/threads/".to_string()
                );
                StoreStatus::Ephemeral
            } else {
                StoreStatus::Healthy
            };

            let ts_key_count = ts.thread_count().ok().map(|n| n as u64);

            stores.push(StoreHealth {
                name: "ThreadStore".to_string(),
                status: ts_status,
                path: Some(ts.store_path().display().to_string()),
                key_count: ts_key_count,
                detail: if ts_status.is_healthy() {
                    "Thread persistence active".to_string()
                } else {
                    "Temp fallback — thread history may be lost!".to_string()
                },
            });
        }

        // ── Overall ─────────────────────────────────────────────────
        let overall = if stores.iter().any(|s| s.status == StoreStatus::Failed) {
            StoreStatus::Failed
        } else if any_ephemeral {
            StoreStatus::Ephemeral
        } else {
            StoreStatus::Healthy
        };

        if any_ephemeral {
            recommendations.push(
                "Some storage is ephemeral. Long-term memory, learned preferences, and chat \
                 history may not persist. Resolve the underlying storage issue to enable \
                 full persistence.".to_string()
            );
        }

        if any_ephemeral {
            warn!(
                overall_status = ?overall,
                ephemeral_stores = stores.iter().filter(|s| s.status == StoreStatus::Ephemeral).count(),
                "Storage health check: DEGRADED — some stores are ephemeral"
            );
        }

        StorageHealth {
            overall,
            stores,
            any_ephemeral,
            recommendations,
            checked_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}
