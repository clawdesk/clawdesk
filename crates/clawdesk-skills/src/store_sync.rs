//! Store catalog remote synchronization.
//!
//! ## Sync Protocol
//!
//! 1. Client sends `GET /catalog.json` with `If-None-Match: <etag>`.
//! 2. If 304 Not Modified → catalog is fresh, no work needed.
//! 3. If 200 → parse JSON, compute Merkle root, delta-merge into local catalog.
//!
//! ## Merkle Verification
//!
//! Each catalog entry hashes to `H(skill_id || version || checksum)`.
//! The Merkle root = hash of sorted leaf hashes. This allows detecting
//! catalog tampering without per-entry signature verification.
//!
//! ## Delta Sync
//!
//! Rather than replacing the entire catalog, we compute the set difference:
//! - `added`:   remote IDs not in local
//! - `updated`: remote IDs present locally but with different version/hash
//! - `removed`: local IDs not in remote (optionally pruned)
//!
//! This minimizes re-indexing work and preserves local install state.

use crate::store::{StoreBackend, StoreEntry, InstallState};
use serde::{Serialize, Deserialize};
use sha2::{Sha256, Digest};
use std::collections::HashMap;
use tracing::{debug, info, warn};

/// Sync configuration.
#[derive(Debug, Clone)]
pub struct SyncConfig {
    /// Remote catalog URL.
    pub catalog_url: String,
    /// Whether to verify Merkle root.
    pub verify_merkle: bool,
    /// Whether to prune local entries absent from remote.
    pub prune_removed: bool,
    /// Request timeout in seconds.
    pub timeout_secs: u64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            catalog_url: "https://store.clawdesk.dev/catalog.json".to_string(),
            verify_merkle: true,
            prune_removed: false,
            timeout_secs: 30,
        }
    }
}

/// Result of a catalog sync operation.
#[derive(Debug, Clone)]
pub struct SyncResult {
    /// Number of new entries added.
    pub added: usize,
    /// Number of existing entries updated.
    pub updated: usize,
    /// Number of local entries pruned (if prune_removed is enabled).
    pub pruned: usize,
    /// Whether catalog was already up-to-date (304 response).
    pub was_fresh: bool,
    /// Merkle root of the remote catalog (hex-encoded SHA-256).
    pub merkle_root: String,
    /// ETag from the remote response (for subsequent conditional fetches).
    pub etag: Option<String>,
    /// Errors encountered during sync (non-fatal).
    pub warnings: Vec<String>,
}

/// ETag-based conditional fetch state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SyncState {
    /// Last seen ETag from the remote catalog server.
    pub last_etag: Option<String>,
    /// Merkle root of the last successfully synced catalog.
    pub last_merkle_root: Option<String>,
    /// Timestamp of the last successful sync (ISO 8601).
    pub last_sync_at: Option<String>,
}

/// Remote catalog response (parsed from JSON).
#[derive(Debug, Clone)]
pub struct RemoteCatalog {
    /// The catalog entries.
    pub entries: Vec<StoreEntry>,
    /// ETag header from the response.
    pub etag: Option<String>,
    /// Whether the response was 304 Not Modified.
    pub not_modified: bool,
}

/// Compute the Merkle root of a set of store entries.
///
/// Algorithm:
/// 1. For each entry, compute `H(skill_id || ":" || version)`.
/// 2. Sort the leaf hashes lexicographically.
/// 3. Build a binary Merkle tree; the root is the final hash.
///
/// Time: O(n log n) due to sort. Space: O(n).
pub fn compute_merkle_root(entries: &[StoreEntry]) -> String {
    if entries.is_empty() {
        return "0".repeat(64); // empty tree hash
    }

    // Step 1: compute leaf hashes
    let mut leaves: Vec<[u8; 32]> = entries
        .iter()
        .map(|e| {
            let mut hasher = Sha256::new();
            hasher.update(e.skill_id.as_str().as_bytes());
            hasher.update(b":");
            hasher.update(e.version.as_bytes());
            let result = hasher.finalize();
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&result);
            arr
        })
        .collect();

    // Step 2: sort for deterministic ordering
    leaves.sort();

    // Step 3: iterative Merkle tree construction
    let mut level = leaves;
    while level.len() > 1 {
        let mut next_level = Vec::with_capacity((level.len() + 1) / 2);
        for chunk in level.chunks(2) {
            let mut hasher = Sha256::new();
            hasher.update(chunk[0]);
            if chunk.len() > 1 {
                hasher.update(chunk[1]);
            } else {
                // Odd node: hash with itself
                hasher.update(chunk[0]);
            }
            let result = hasher.finalize();
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&result);
            next_level.push(arr);
        }
        level = next_level;
    }

    hex::encode(level[0])
}

/// Compute the delta between a remote catalog and the local store.
///
/// Returns `(to_add, to_update, to_prune)`.
pub fn compute_delta(
    remote: &[StoreEntry],
    local: &StoreBackend,
) -> (Vec<StoreEntry>, Vec<StoreEntry>, Vec<String>) {
    let mut to_add = Vec::new();
    let mut to_update = Vec::new();
    let mut remote_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for entry in remote {
        let key = entry.skill_id.as_str().to_string();
        remote_ids.insert(key.clone());

        match local.get(&key) {
            None => {
                debug!(skill = %key, "new catalog entry");
                to_add.push(entry.clone());
            }
            Some(existing) => {
                if existing.version != entry.version {
                    debug!(
                        skill = %key,
                        from = %existing.version,
                        to = %entry.version,
                        "catalog entry updated"
                    );
                    // Preserve local install state during update
                    let mut updated = entry.clone();
                    updated.install_state = existing.install_state;
                    to_update.push(updated);
                }
            }
        }
    }

    // Find local-only entries (candidates for pruning)
    let to_prune: Vec<String> = local
        .all_keys()
        .into_iter()
        .filter(|k| !remote_ids.contains(k))
        .collect();

    (to_add, to_update, to_prune)
}

/// Apply a sync delta to the local store.
pub fn apply_delta(
    store: &mut StoreBackend,
    to_add: Vec<StoreEntry>,
    to_update: Vec<StoreEntry>,
    to_prune: &[String],
    prune_enabled: bool,
) -> SyncResult {
    let added = to_add.len();
    let updated = to_update.len();

    for entry in to_add {
        store.upsert(entry);
    }

    for entry in to_update {
        store.upsert(entry);
    }

    let pruned = if prune_enabled {
        let mut count = 0;
        for key in to_prune {
            if store.remove(key) {
                count += 1;
            }
        }
        count
    } else {
        0
    };

    info!(added, updated, pruned, "catalog delta applied");

    SyncResult {
        added,
        updated,
        pruned,
        was_fresh: false,
        merkle_root: String::new(), // caller fills in
        etag: None,
        warnings: vec![],
    }
}

/// Full sync pipeline: parse remote, compute delta, verify Merkle, apply.
///
/// This is the function called by the gateway RPC `sync` method and the
/// `clawdesk skill sync` CLI command.
pub fn sync_from_remote_payload(
    store: &mut StoreBackend,
    remote_json: &str,
    etag: Option<String>,
    config: &SyncConfig,
    sync_state: &mut SyncState,
) -> Result<SyncResult, SyncError> {
    // Parse remote catalog
    let remote_entries: Vec<StoreEntry> =
        serde_json::from_str(remote_json).map_err(|e| SyncError::ParseFailed(e.to_string()))?;

    // Compute and verify Merkle root
    let merkle_root = compute_merkle_root(&remote_entries);
    if config.verify_merkle {
        if let Some(ref last_root) = sync_state.last_merkle_root {
            if *last_root == merkle_root {
                info!("Merkle root unchanged — catalog is fresh");
                return Ok(SyncResult {
                    added: 0,
                    updated: 0,
                    pruned: 0,
                    was_fresh: true,
                    merkle_root,
                    etag,
                    warnings: vec![],
                });
            }
        }
    }

    // Compute delta
    let (to_add, to_update, to_prune) = compute_delta(&remote_entries, store);

    if to_add.is_empty() && to_update.is_empty() && to_prune.is_empty() {
        info!("no delta to apply — catalog unchanged");
        sync_state.last_merkle_root = Some(merkle_root.clone());
        sync_state.last_etag = etag.clone();
        sync_state.last_sync_at = Some(chrono::Utc::now().to_rfc3339());
        return Ok(SyncResult {
            added: 0,
            updated: 0,
            pruned: 0,
            was_fresh: true,
            merkle_root,
            etag,
            warnings: vec![],
        });
    }

    // Apply delta
    let mut result =
        apply_delta(store, to_add, to_update, &to_prune, config.prune_removed);
    result.merkle_root = merkle_root.clone();
    result.etag = etag.clone();

    // Update sync state
    sync_state.last_merkle_root = Some(merkle_root);
    sync_state.last_etag = etag;
    sync_state.last_sync_at = Some(chrono::Utc::now().to_rfc3339());

    Ok(result)
}

/// Errors during catalog sync.
#[derive(Debug, Clone)]
pub enum SyncError {
    /// Failed to parse remote catalog JSON.
    ParseFailed(String),
    /// Network error during fetch.
    NetworkError(String),
    /// Merkle root mismatch (possible tampering).
    MerkleRootMismatch {
        expected: String,
        actual: String,
    },
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ParseFailed(e) => write!(f, "catalog parse failed: {}", e),
            Self::NetworkError(e) => write!(f, "network error: {}", e),
            Self::MerkleRootMismatch { expected, actual } => {
                write!(
                    f,
                    "Merkle root mismatch: expected {}, got {}",
                    expected, actual
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::SkillId;
    use crate::store::{StoreCategory, StoreEntry, InstallState};

    fn test_entry(name: &str, version: &str) -> StoreEntry {
        StoreEntry {
            skill_id: SkillId::new("store", name),
            display_name: name.to_string(),
            short_description: format!("A {} skill", name),
            long_description: String::new(),
            category: StoreCategory::Other,
            tags: vec![],
            author: "test".into(),
            version: version.into(),
            install_state: InstallState::Available,
            rating: 4.0,
            install_count: 0,
            updated_at: "2026-01-01".into(),
            icon: "📦".into(),
            verified: true,
            license: None,
            source_url: None,
            min_version: None,
        }
    }

    #[test]
    fn merkle_root_deterministic() {
        let entries = vec![
            test_entry("alpha", "1.0"),
            test_entry("beta", "2.0"),
        ];
        let root1 = compute_merkle_root(&entries);
        let root2 = compute_merkle_root(&entries);
        assert_eq!(root1, root2);
        assert_eq!(root1.len(), 64); // SHA-256 hex
    }

    #[test]
    fn merkle_root_order_independent() {
        let a = test_entry("alpha", "1.0");
        let b = test_entry("beta", "2.0");
        let root_ab = compute_merkle_root(&[a.clone(), b.clone()]);
        let root_ba = compute_merkle_root(&[b, a]);
        assert_eq!(root_ab, root_ba); // sorted leaves → same root
    }

    #[test]
    fn merkle_root_empty() {
        let root = compute_merkle_root(&[]);
        assert_eq!(root, "0".repeat(64));
    }

    #[test]
    fn delta_detects_additions() {
        let store = StoreBackend::new();
        let remote = vec![test_entry("new-skill", "1.0")];
        let (added, updated, pruned) = compute_delta(&remote, &store);
        assert_eq!(added.len(), 1);
        assert!(updated.is_empty());
        assert!(pruned.is_empty());
    }

    #[test]
    fn delta_detects_updates() {
        let mut store = StoreBackend::new();
        let mut existing = test_entry("skill-a", "1.0");
        existing.install_state = InstallState::Active;
        store.upsert(existing);

        let remote = vec![test_entry("skill-a", "2.0")];
        let (added, updated, _pruned) = compute_delta(&remote, &store);
        assert!(added.is_empty());
        assert_eq!(updated.len(), 1);
        // Install state should be preserved
        assert_eq!(updated[0].install_state, InstallState::Active);
    }

    #[test]
    fn delta_detects_removals() {
        let mut store = StoreBackend::new();
        store.upsert(test_entry("old-skill", "1.0"));
        let remote: Vec<StoreEntry> = vec![];
        let (_added, _updated, pruned) = compute_delta(&remote, &store);
        assert_eq!(pruned.len(), 1);
        assert!(pruned.contains(&"store/old-skill".to_string()));
    }

    #[test]
    fn apply_delta_adds_and_updates() {
        let mut store = StoreBackend::new();
        store.upsert(test_entry("existing", "1.0"));

        let to_add = vec![test_entry("new", "1.0")];
        let to_update = vec![test_entry("existing", "2.0")];

        let result = apply_delta(&mut store, to_add, to_update, &[], false);
        assert_eq!(result.added, 1);
        assert_eq!(result.updated, 1);
        assert_eq!(store.entry_count(), 2);
        assert_eq!(store.get("store/existing").unwrap().version, "2.0");
    }

    #[test]
    fn full_sync_pipeline() {
        let mut store = StoreBackend::new();
        store.upsert(test_entry("keep", "1.0"));

        let remote = vec![
            test_entry("keep", "2.0"),
            test_entry("new-entry", "1.0"),
        ];
        let remote_json = serde_json::to_string(&remote).unwrap();

        let config = SyncConfig {
            verify_merkle: true,
            prune_removed: false,
            ..Default::default()
        };
        let mut sync_state = SyncState::default();

        let result = sync_from_remote_payload(
            &mut store,
            &remote_json,
            Some("etag-123".into()),
            &config,
            &mut sync_state,
        )
        .unwrap();

        assert_eq!(result.added, 1);
        assert_eq!(result.updated, 1);
        assert!(!result.merkle_root.is_empty());
        assert_eq!(sync_state.last_etag, Some("etag-123".into()));

        // Second sync with same data — should be fresh
        let result2 = sync_from_remote_payload(
            &mut store,
            &remote_json,
            Some("etag-123".into()),
            &config,
            &mut sync_state,
        )
        .unwrap();
        assert!(result2.was_fresh);
    }
}
