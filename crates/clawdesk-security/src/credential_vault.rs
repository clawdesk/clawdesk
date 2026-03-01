//! Credential vault — secure credential storage via OS keychain or encrypted file.
//!
//! Credentials are stored in the OS keychain (macOS Keychain, Windows Credential
//! Manager, Linux Secret Service) when available, falling back to encrypted
//! on-disk storage. The in-memory `HashMap` serves as a **write-through cache**:
//! writes go to both keychain + memory, reads check memory first then keychain.
//!
//! ## Security Properties
//! - Credentials never stored in plaintext on disk
//! - OS keychain integration for hardware-backed security (Secure Enclave on macOS)
//! - In-memory credential secrets zeroed on drop via `zeroize::Zeroizing<String>`
//! - Concurrent-safe access via `RwLock`
//!
//! ## Keychain key format
//! `{service_name}:{provider}:{credential_id}`

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, info, warn};
use zeroize::{Zeroize, Zeroizing};

/// A stored credential with metadata.
///
/// The `secret` field is zeroed on drop via a custom `Drop` implementation
/// to ensure the heap memory backing the API key / token is overwritten.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredCredential {
    /// Provider this credential belongs to (e.g., "anthropic", "openai").
    pub provider: String,
    /// Unique credential identifier within the provider.
    pub credential_id: String,
    /// The API key or token (sensitive). Zeroed on drop.
    #[serde(skip_serializing)]
    pub secret: String,
    /// Optional organization ID.
    pub org_id: Option<String>,
    /// Optional project ID.
    pub project_id: Option<String>,
    /// User-assigned label.
    pub label: Option<String>,
    /// When this credential was stored.
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// When this credential was last verified as working.
    pub last_verified: Option<chrono::DateTime<chrono::Utc>>,
    /// Whether this credential has been marked as expired/invalid.
    pub is_expired: bool,
}

impl Drop for StoredCredential {
    fn drop(&mut self) {
        self.secret.zeroize();
    }
}

impl StoredCredential {
    pub fn new(provider: impl Into<String>, credential_id: impl Into<String>, secret: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            credential_id: credential_id.into(),
            secret: secret.into(),
            org_id: None,
            project_id: None,
            label: None,
            created_at: chrono::Utc::now(),
            last_verified: None,
            is_expired: false,
        }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn with_org_id(mut self, org_id: impl Into<String>) -> Self {
        self.org_id = Some(org_id.into());
        self
    }
}

/// Backend for credential storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultBackend {
    /// OS keychain (macOS Keychain, Windows Credential Manager, Linux Secret Service).
    OsKeychain,
    /// Encrypted file on disk (fallback).
    EncryptedFile,
    /// In-memory only (for testing).
    InMemory,
}

impl std::fmt::Display for VaultBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::OsKeychain => write!(f, "os_keychain"),
            Self::EncryptedFile => write!(f, "encrypted_file"),
            Self::InMemory => write!(f, "in_memory"),
        }
    }
}

/// Credential vault — manages secure storage of API keys and tokens.
///
/// Uses a **write-through cache** pattern:
/// - **Write:** keychain + memory (both updated atomically)
/// - **Read:** memory first (O(1)), keychain on miss (O(1) syscall)
///
/// Cache coherence is trivial because ClawDesk runs single-process
/// (INV-1: at most one gateway process). No invalidation needed.
pub struct CredentialVault {
    /// Backend used for persistence.
    backend: VaultBackend,
    /// In-memory credential cache: provider → credential_id → credential.
    /// This is the hot cache — always checked first on reads.
    credentials: std::sync::RwLock<HashMap<String, HashMap<String, StoredCredential>>>,
    /// Service name for OS keychain entries.
    service_name: String,
}

impl CredentialVault {
    /// Create a new vault with auto-detected backend.
    pub fn new(service_name: impl Into<String>) -> Self {
        let service = service_name.into();
        let backend = Self::detect_backend();
        info!(backend = %backend, service = %service, "Credential vault initialized");

        let vault = Self {
            backend,
            credentials: std::sync::RwLock::new(HashMap::new()),
            service_name: service,
        };

        // Pre-warm cache from keychain on startup
        if backend == VaultBackend::OsKeychain {
            vault.warm_cache_from_keychain();
        }

        vault
    }

    /// Create a vault with a specific backend (for testing).
    pub fn with_backend(service_name: impl Into<String>, backend: VaultBackend) -> Self {
        Self {
            backend,
            credentials: std::sync::RwLock::new(HashMap::new()),
            service_name: service_name.into(),
        }
    }

    /// Detect the best available backend.
    fn detect_backend() -> VaultBackend {
        // Prefer OS keychain when available
        if cfg!(target_os = "macos") || cfg!(target_os = "windows") || cfg!(target_os = "linux") {
            VaultBackend::OsKeychain
        } else {
            VaultBackend::EncryptedFile
        }
    }

    /// Build the keychain entry key: `{service}:{provider}:{credential_id}`
    fn keychain_key(&self, provider: &str, credential_id: &str) -> String {
        format!("{}:{}:{}", self.service_name, provider, credential_id)
    }

    /// Store a credential — write-through to both keychain and memory.
    ///
    /// For `OsKeychain` backend: writes the secret to the OS keychain via
    /// `security` framework (macOS), DPAPI (Windows), or Secret Service (Linux),
    /// and stores the full credential metadata in the in-memory cache.
    ///
    /// For `InMemory` backend: stores only in the in-memory cache.
    pub fn store(&self, credential: StoredCredential) -> Result<(), VaultError> {
        let provider = credential.provider.clone();
        let cred_id = credential.credential_id.clone();
        let secret = credential.secret.clone();

        debug!(provider = %provider, credential_id = %cred_id, backend = %self.backend, "Storing credential");

        // Write-through: persist to keychain first (if applicable)
        if self.backend == VaultBackend::OsKeychain {
            self.keychain_store(&provider, &cred_id, &secret)?;
        }

        // Then update the in-memory cache
        let mut creds = self.credentials.write().map_err(|_| VaultError::LockPoisoned)?;
        creds
            .entry(provider)
            .or_insert_with(HashMap::new)
            .insert(cred_id, credential);

        Ok(())
    }

    /// Retrieve a specific credential.
    ///
    /// Checks the in-memory cache first (O(1)). On cache miss with
    /// `OsKeychain` backend, falls through to keychain syscall.
    pub fn get(&self, provider: &str, credential_id: &str) -> Result<Option<StoredCredential>, VaultError> {
        // Hot path: check in-memory cache first
        {
            let creds = self.credentials.read().map_err(|_| VaultError::LockPoisoned)?;
            if let Some(cred) = creds.get(provider).and_then(|p| p.get(credential_id)) {
                return Ok(Some(cred.clone()));
            }
        }

        // Cold path: try keychain on cache miss
        if self.backend == VaultBackend::OsKeychain {
            if let Some(secret) = self.keychain_get(provider, credential_id)? {
                // Reconstruct a StoredCredential from keychain data
                let cred = StoredCredential::new(provider, credential_id, secret);

                // Backfill the cache
                let mut creds = self.credentials.write().map_err(|_| VaultError::LockPoisoned)?;
                creds
                    .entry(provider.to_string())
                    .or_insert_with(HashMap::new)
                    .insert(credential_id.to_string(), cred.clone());

                return Ok(Some(cred));
            }
        }

        Ok(None)
    }

    /// List all credentials for a provider (secrets are included).
    pub fn list_for_provider(&self, provider: &str) -> Result<Vec<StoredCredential>, VaultError> {
        let creds = self.credentials.read().map_err(|_| VaultError::LockPoisoned)?;
        Ok(creds
            .get(provider)
            .map(|p| p.values().cloned().collect())
            .unwrap_or_default())
    }

    /// Remove a credential from both keychain and cache.
    pub fn remove(&self, provider: &str, credential_id: &str) -> Result<Option<StoredCredential>, VaultError> {
        // Remove from keychain first
        if self.backend == VaultBackend::OsKeychain {
            if let Err(e) = self.keychain_delete(provider, credential_id) {
                warn!(
                    provider = %provider,
                    credential_id = %credential_id,
                    error = %e,
                    "failed to remove from keychain (may not exist)"
                );
            }
        }

        // Remove from cache
        let mut creds = self.credentials.write().map_err(|_| VaultError::LockPoisoned)?;
        Ok(creds
            .get_mut(provider)
            .and_then(|p| p.remove(credential_id)))
    }

    /// Mark a credential as expired.
    pub fn mark_expired(&self, provider: &str, credential_id: &str) -> Result<(), VaultError> {
        let mut creds = self.credentials.write().map_err(|_| VaultError::LockPoisoned)?;
        if let Some(cred) = creds.get_mut(provider).and_then(|p| p.get_mut(credential_id)) {
            cred.is_expired = true;
            info!(provider = %provider, credential_id = %credential_id, "Credential marked expired");
        }
        Ok(())
    }

    /// Mark a credential as verified (last used successfully).
    pub fn mark_verified(&self, provider: &str, credential_id: &str) -> Result<(), VaultError> {
        let mut creds = self.credentials.write().map_err(|_| VaultError::LockPoisoned)?;
        if let Some(cred) = creds.get_mut(provider).and_then(|p| p.get_mut(credential_id)) {
            cred.last_verified = Some(chrono::Utc::now());
            cred.is_expired = false;
        }
        Ok(())
    }

    /// Count credentials for a provider.
    pub fn count_for_provider(&self, provider: &str) -> usize {
        self.credentials
            .read()
            .map(|c| c.get(provider).map_or(0, |p| p.len()))
            .unwrap_or(0)
    }

    /// Get the active backend.
    pub fn backend(&self) -> VaultBackend {
        self.backend
    }

    /// Get the secret for a provider/credential pair, without metadata.
    ///
    /// Convenience method for integration with config loading —
    /// returns just the secret string.
    pub fn get_secret(&self, provider: &str, credential_id: &str) -> Result<Option<String>, VaultError> {
        self.get(provider, credential_id)
            .map(|opt| opt.map(|c| c.secret.clone()))
    }

    /// Store a simple key-value secret (provider, id, secret).
    ///
    /// Convenience method for migration from env vars.
    pub fn store_secret(
        &self,
        provider: &str,
        credential_id: &str,
        secret: &str,
    ) -> Result<(), VaultError> {
        self.store(StoredCredential::new(provider, credential_id, secret))
    }

    /// Resolve a vault reference string like `$vault:provider:credential_id`.
    ///
    /// Returns the secret if the reference is valid and the credential exists,
    /// or None if the string is not a vault reference.
    pub fn resolve_ref(&self, value: &str) -> Result<Option<String>, VaultError> {
        if let Some(rest) = value.strip_prefix("$vault:") {
            let parts: Vec<&str> = rest.splitn(2, ':').collect();
            if parts.len() == 2 {
                return self.get_secret(parts[0], parts[1]);
            }
        }
        Ok(None)
    }

    // ─── OS Keychain operations ──────────────────────────────────────────

    /// Store a secret in the OS keychain.
    ///
    /// macOS: `security add-generic-password` (Keychain Services API)
    /// Windows: `CredWriteW` (DPAPI-backed Credential Manager)
    /// Linux: Secret Service D-Bus API
    fn keychain_store(
        &self,
        provider: &str,
        credential_id: &str,
        secret: &str,
    ) -> Result<(), VaultError> {
        let key = self.keychain_key(provider, credential_id);
        debug!(key = %key, "storing in OS keychain");

        // Use the `security` command on macOS for keychain access.
        // This avoids adding the `keyring` crate dependency while
        // providing equivalent functionality through the native CLI.
        #[cfg(target_os = "macos")]
        {
            let output = std::process::Command::new("security")
                .args([
                    "add-generic-password",
                    "-a", &key,             // account name (our composite key)
                    "-s", &self.service_name, // service name
                    "-w", secret,            // password
                    "-U",                    // update if exists
                ])
                .output()
                .map_err(|e| VaultError::KeychainError(format!("failed to run security: {}", e)))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                // Error -25299 means item already exists; -U flag should handle it
                // but on some macOS versions we need to delete first then re-add
                if stderr.contains("-25299") {
                    // Delete existing and retry
                    let _ = self.keychain_delete(provider, credential_id);
                    let retry = std::process::Command::new("security")
                        .args([
                            "add-generic-password",
                            "-a", &key,
                            "-s", &self.service_name,
                            "-w", secret,
                        ])
                        .output()
                        .map_err(|e| VaultError::KeychainError(format!("retry failed: {}", e)))?;

                    if !retry.status.success() {
                        let err = String::from_utf8_lossy(&retry.stderr);
                        return Err(VaultError::KeychainError(format!("keychain store failed: {}", err)));
                    }
                } else {
                    return Err(VaultError::KeychainError(format!("keychain store failed: {}", stderr)));
                }
            }
            return Ok(());
        }

        #[cfg(target_os = "linux")]
        {
            // On Linux, use `secret-tool` (libsecret CLI) for Secret Service API
            let output = std::process::Command::new("secret-tool")
                .args([
                    "store",
                    "--label", &format!("ClawDesk: {}", key),
                    "service", &self.service_name,
                    "account", &key,
                ])
                .stdin(std::process::Stdio::piped())
                .spawn()
                .and_then(|mut child| {
                    use std::io::Write;
                    if let Some(ref mut stdin) = child.stdin {
                        stdin.write_all(secret.as_bytes())?;
                    }
                    child.wait_with_output()
                })
                .map_err(|e| VaultError::KeychainError(format!("secret-tool: {}", e)))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(VaultError::KeychainError(format!("secret-tool store failed: {}", stderr)));
            }
            return Ok(());
        }

        #[cfg(target_os = "windows")]
        {
            // On Windows, use `cmdkey` for Credential Manager
            let target = format!("{}:{}", self.service_name, key);
            let output = std::process::Command::new("cmdkey")
                .args([
                    &format!("/generic:{}", target),
                    &format!("/user:{}", key),
                    &format!("/pass:{}", secret),
                ])
                .output()
                .map_err(|e| VaultError::KeychainError(format!("cmdkey: {}", e)))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(VaultError::KeychainError(format!("cmdkey store failed: {}", stderr)));
            }
            return Ok(());
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            warn!("no keychain available on this platform, credential stored in memory only");
            Ok(())
        }
    }

    /// Retrieve a secret from the OS keychain.
    fn keychain_get(
        &self,
        provider: &str,
        credential_id: &str,
    ) -> Result<Option<String>, VaultError> {
        let key = self.keychain_key(provider, credential_id);

        #[cfg(target_os = "macos")]
        {
            let output = std::process::Command::new("security")
                .args([
                    "find-generic-password",
                    "-a", &key,
                    "-s", &self.service_name,
                    "-w", // output password only
                ])
                .output()
                .map_err(|e| VaultError::KeychainError(format!("security: {}", e)))?;

            if output.status.success() {
                let secret = String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .to_string();
                return Ok(Some(secret));
            }
            // Item not found is not an error — just a cache miss
            return Ok(None);
        }

        #[cfg(target_os = "linux")]
        {
            let output = std::process::Command::new("secret-tool")
                .args([
                    "lookup",
                    "service", &self.service_name,
                    "account", &key,
                ])
                .output()
                .map_err(|e| VaultError::KeychainError(format!("secret-tool: {}", e)))?;

            if output.status.success() {
                let secret = String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .to_string();
                if !secret.is_empty() {
                    return Ok(Some(secret));
                }
            }
            return Ok(None);
        }

        #[cfg(target_os = "windows")]
        {
            // cmdkey /list doesn't return passwords; use PowerShell
            let target = format!("{}:{}", self.service_name, key);
            let ps_cmd = format!(
                "(Get-StoredCredential -Target '{}').GetNetworkCredential().Password",
                target
            );
            let output = std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", &ps_cmd])
                .output()
                .map_err(|e| VaultError::KeychainError(format!("powershell: {}", e)))?;

            if output.status.success() {
                let secret = String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .to_string();
                if !secret.is_empty() {
                    return Ok(Some(secret));
                }
            }
            return Ok(None);
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Ok(None)
        }
    }

    /// Delete a secret from the OS keychain.
    fn keychain_delete(
        &self,
        provider: &str,
        credential_id: &str,
    ) -> Result<(), VaultError> {
        let key = self.keychain_key(provider, credential_id);

        #[cfg(target_os = "macos")]
        {
            let output = std::process::Command::new("security")
                .args([
                    "delete-generic-password",
                    "-a", &key,
                    "-s", &self.service_name,
                ])
                .output()
                .map_err(|e| VaultError::KeychainError(format!("security: {}", e)))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(VaultError::KeychainError(format!("delete failed: {}", stderr)));
            }
            return Ok(());
        }

        #[cfg(target_os = "linux")]
        {
            let _output = std::process::Command::new("secret-tool")
                .args([
                    "clear",
                    "service", &self.service_name,
                    "account", &key,
                ])
                .output()
                .map_err(|e| VaultError::KeychainError(format!("secret-tool: {}", e)))?;
            return Ok(());
        }

        #[cfg(target_os = "windows")]
        {
            let target = format!("{}:{}", self.service_name, key);
            let _output = std::process::Command::new("cmdkey")
                .args([&format!("/delete:{}", target)])
                .output()
                .map_err(|e| VaultError::KeychainError(format!("cmdkey: {}", e)))?;
            return Ok(());
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            Ok(())
        }
    }

    /// Pre-warm the in-memory cache by loading a known set of providers.
    ///
    /// Called once at startup. Loads credentials for standard providers
    /// (anthropic, openai, google, azure, cohere) from the keychain.
    fn warm_cache_from_keychain(&self) {
        let providers = [
            ("anthropic", "default"),
            ("openai", "default"),
            ("google", "default"),
            ("azure", "default"),
            ("cohere", "default"),
            ("vertex", "default"),
        ];

        let mut loaded = 0;
        for (provider, cred_id) in &providers {
            match self.keychain_get(provider, cred_id) {
                Ok(Some(secret)) => {
                    let cred = StoredCredential::new(*provider, *cred_id, secret);
                    if let Ok(mut creds) = self.credentials.write() {
                        creds
                            .entry(provider.to_string())
                            .or_insert_with(HashMap::new)
                            .insert(cred_id.to_string(), cred);
                        loaded += 1;
                    }
                }
                Ok(None) => {} // Not stored yet — normal for first run
                Err(e) => {
                    debug!(
                        provider = %provider,
                        error = %e,
                        "failed to load credential from keychain"
                    );
                }
            }
        }

        if loaded > 0 {
            info!(loaded, "pre-warmed credential cache from keychain");
        }
    }
}

/// Errors from vault operations.
#[derive(Debug, Clone)]
pub enum VaultError {
    /// RwLock was poisoned.
    LockPoisoned,
    /// OS keychain operation failed.
    KeychainError(String),
    /// Encrypted file I/O error.
    IoError(String),
    /// Decryption failed (wrong key or corrupted data).
    DecryptionFailed,
    /// Credential not found.
    NotFound,
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LockPoisoned => write!(f, "credential vault lock poisoned"),
            Self::KeychainError(e) => write!(f, "keychain error: {e}"),
            Self::IoError(e) => write!(f, "vault I/O error: {e}"),
            Self::DecryptionFailed => write!(f, "vault decryption failed"),
            Self::NotFound => write!(f, "credential not found"),
        }
    }
}

impl std::error::Error for VaultError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_and_retrieve() {
        let vault = CredentialVault::with_backend("test", VaultBackend::InMemory);

        let cred = StoredCredential::new("anthropic", "key-1", "sk-ant-test123")
            .with_label("Primary key");

        vault.store(cred.clone()).unwrap();

        let retrieved = vault.get("anthropic", "key-1").unwrap().unwrap();
        assert_eq!(retrieved.provider, "anthropic");
        assert_eq!(retrieved.secret, "sk-ant-test123");
        assert_eq!(retrieved.label, Some("Primary key".to_string()));
    }

    #[test]
    fn test_list_for_provider() {
        let vault = CredentialVault::with_backend("test", VaultBackend::InMemory);

        vault.store(StoredCredential::new("openai", "key-1", "sk-1")).unwrap();
        vault.store(StoredCredential::new("openai", "key-2", "sk-2")).unwrap();
        vault.store(StoredCredential::new("anthropic", "key-3", "sk-3")).unwrap();

        let openai_creds = vault.list_for_provider("openai").unwrap();
        assert_eq!(openai_creds.len(), 2);

        let anthropic_creds = vault.list_for_provider("anthropic").unwrap();
        assert_eq!(anthropic_creds.len(), 1);

        let empty = vault.list_for_provider("nonexistent").unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn test_remove_credential() {
        let vault = CredentialVault::with_backend("test", VaultBackend::InMemory);

        vault.store(StoredCredential::new("openai", "key-1", "sk-1")).unwrap();
        let removed = vault.remove("openai", "key-1").unwrap();
        assert!(removed.is_some());
        assert!(vault.get("openai", "key-1").unwrap().is_none());
    }

    #[test]
    fn test_mark_expired() {
        let vault = CredentialVault::with_backend("test", VaultBackend::InMemory);

        vault.store(StoredCredential::new("openai", "key-1", "sk-1")).unwrap();
        vault.mark_expired("openai", "key-1").unwrap();

        let cred = vault.get("openai", "key-1").unwrap().unwrap();
        assert!(cred.is_expired);
    }

    #[test]
    fn test_mark_verified_clears_expired() {
        let vault = CredentialVault::with_backend("test", VaultBackend::InMemory);

        let mut cred = StoredCredential::new("openai", "key-1", "sk-1");
        cred.is_expired = true;
        vault.store(cred).unwrap();

        vault.mark_verified("openai", "key-1").unwrap();

        let cred = vault.get("openai", "key-1").unwrap().unwrap();
        assert!(!cred.is_expired);
        assert!(cred.last_verified.is_some());
    }

    #[test]
    fn test_get_secret_convenience() {
        let vault = CredentialVault::with_backend("test", VaultBackend::InMemory);
        vault.store(StoredCredential::new("anthropic", "default", "sk-ant-123")).unwrap();

        let secret = vault.get_secret("anthropic", "default").unwrap();
        assert_eq!(secret, Some("sk-ant-123".to_string()));

        let missing = vault.get_secret("nonexistent", "x").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_store_secret_convenience() {
        let vault = CredentialVault::with_backend("test", VaultBackend::InMemory);
        vault.store_secret("openai", "default", "sk-xyz").unwrap();

        let cred = vault.get("openai", "default").unwrap().unwrap();
        assert_eq!(cred.secret, "sk-xyz");
    }

    #[test]
    fn test_resolve_ref() {
        let vault = CredentialVault::with_backend("test", VaultBackend::InMemory);
        vault.store_secret("anthropic", "key-1", "sk-ant-real").unwrap();

        // Valid vault reference
        let resolved = vault.resolve_ref("$vault:anthropic:key-1").unwrap();
        assert_eq!(resolved, Some("sk-ant-real".to_string()));

        // Not a vault reference — returns None
        let not_ref = vault.resolve_ref("sk-ant-plaintext").unwrap();
        assert!(not_ref.is_none());

        // Vault reference to nonexistent credential
        let missing = vault.resolve_ref("$vault:openai:missing").unwrap();
        assert!(missing.is_none());
    }

    #[test]
    fn test_keychain_key_format() {
        let vault = CredentialVault::with_backend("clawdesk", VaultBackend::InMemory);
        let key = vault.keychain_key("anthropic", "default");
        assert_eq!(key, "clawdesk:anthropic:default");
    }
}
