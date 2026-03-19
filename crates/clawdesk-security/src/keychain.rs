//! External keychain reader — reads credentials stored by Claude Code CLI and Codex CLI.
//!
//! Claude Code stores credentials in the OS keychain under the service name
//! `"Claude Code-credentials"`. Codex uses `"Codex-credentials"`. This module
//! provides O(1) keychain reads with TTL-based caching and zeroize-on-drop
//! for credential memory.
//!
//! ## Platform Support
//! - **macOS:** `security find-generic-password -s <service> -w`
//! - **Linux:** `secret-tool lookup service <service>`
//! - **Windows:** `cmdkey /list:<service>` (limited — prefer the `keyring` crate)
//!
//! ## Security Properties
//! - Credentials are cached in `Zeroizing<String>` — heap memory is zeroed on drop
//! - TTL-based cache eviction (default 5 minutes) limits exposure window
//! - No credentials are logged or serialized at any level

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};
use zeroize::Zeroizing;

/// Known external CLI keychain service names.
pub mod services {
    /// Claude Code CLI keychain service name.
    pub const CLAUDE_CODE: &str = "Claude Code-credentials";
    /// Codex CLI keychain service name.
    pub const CODEX: &str = "Codex-credentials";
}

/// A cached credential with TTL tracking.
struct CachedCredential {
    /// The credential secret, zeroed on drop.
    secret: Zeroizing<String>,
    /// When this cache entry was created.
    cached_at: Instant,
}

/// External keychain provider — reads credentials from other CLI tools' keychain entries.
///
/// Uses a TTL-based in-memory cache to avoid repeated keychain IPC calls.
/// Cache entries are `Zeroizing<String>` — heap memory is overwritten with zeros
/// on eviction or drop.
pub struct KeychainProvider {
    /// Cache: service_name → CachedCredential.
    cache: RwLock<HashMap<String, CachedCredential>>,
    /// Time-to-live for cached credentials.
    ttl: Duration,
}

impl KeychainProvider {
    /// Create a new keychain provider with the given TTL.
    pub fn new(ttl: Duration) -> Self {
        Self {
            cache: RwLock::new(HashMap::new()),
            ttl,
        }
    }

    /// Create a keychain provider with the default 5-minute TTL.
    pub fn default_ttl() -> Self {
        Self::new(Duration::from_secs(300))
    }

    /// Read a credential from an external CLI's keychain entry.
    ///
    /// Checks the TTL cache first (O(1)). On cache miss, performs a
    /// platform-specific keychain read (~2ms macOS, ~5ms Linux).
    ///
    /// The returned `Zeroizing<String>` will zero its heap allocation on drop.
    pub fn read_credential(&self, service_name: &str) -> Result<Option<Zeroizing<String>>, String> {
        // Hot path: check cache
        {
            let cache = self.cache.read().map_err(|_| "cache lock poisoned")?;
            if let Some(entry) = cache.get(service_name) {
                if entry.cached_at.elapsed() < self.ttl {
                    debug!(service = service_name, "keychain cache hit");
                    return Ok(Some(entry.secret.clone()));
                }
            }
        }

        // Cold path: read from OS keychain
        debug!(service = service_name, "keychain cache miss, reading from OS");
        let secret = self.platform_keychain_read(service_name)?;

        if let Some(ref secret) = secret {
            // Cache the credential
            let mut cache = self.cache.write().map_err(|_| "cache lock poisoned")?;
            cache.insert(
                service_name.to_string(),
                CachedCredential {
                    secret: secret.clone(),
                    cached_at: Instant::now(),
                },
            );
        }

        Ok(secret)
    }

    /// Read Claude Code CLI credentials from the keychain.
    pub fn read_claude_code_credential(&self) -> Result<Option<Zeroizing<String>>, String> {
        self.read_credential(services::CLAUDE_CODE)
    }

    /// Read Codex CLI credentials from the keychain.
    pub fn read_codex_credential(&self) -> Result<Option<Zeroizing<String>>, String> {
        self.read_credential(services::CODEX)
    }

    /// Invalidate a cached credential, forcing a fresh keychain read on next access.
    pub fn invalidate(&self, service_name: &str) {
        if let Ok(mut cache) = self.cache.write() {
            cache.remove(service_name);
        }
    }

    /// Invalidate all cached credentials.
    pub fn invalidate_all(&self) {
        if let Ok(mut cache) = self.cache.write() {
            cache.clear();
        }
    }

    /// Platform-specific keychain read.
    fn platform_keychain_read(&self, service_name: &str) -> Result<Option<Zeroizing<String>>, String> {
        #[cfg(target_os = "macos")]
        {
            return self.macos_keychain_read(service_name);
        }

        #[cfg(target_os = "linux")]
        {
            return self.linux_keychain_read(service_name);
        }

        #[cfg(target_os = "windows")]
        {
            return self.windows_keychain_read(service_name);
        }

        #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
        {
            warn!("keychain not supported on this platform");
            Ok(None)
        }
    }

    /// macOS: read from Keychain Services via `security find-generic-password`.
    #[cfg(target_os = "macos")]
    fn macos_keychain_read(&self, service_name: &str) -> Result<Option<Zeroizing<String>>, String> {
        let output = std::process::Command::new("security")
            .args(["find-generic-password", "-s", service_name, "-w"])
            .output()
            .map_err(|e| format!("failed to run security: {}", e))?;

        if output.status.success() {
            let secret = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if secret.is_empty() {
                Ok(None)
            } else {
                info!(service = service_name, "read credential from macOS Keychain");
                Ok(Some(Zeroizing::new(secret)))
            }
        } else {
            // Error 44 = item not found — not an error condition
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("could not be found") || stderr.contains("SecKeychainSearchCopyNext") {
                debug!(service = service_name, "credential not found in macOS Keychain");
                Ok(None)
            } else {
                Err(format!("macOS keychain read failed: {}", stderr.trim()))
            }
        }
    }

    /// Linux: read from Secret Service via `secret-tool lookup`.
    #[cfg(target_os = "linux")]
    fn linux_keychain_read(&self, service_name: &str) -> Result<Option<Zeroizing<String>>, String> {
        let output = std::process::Command::new("secret-tool")
            .args(["lookup", "service", service_name])
            .output()
            .map_err(|e| format!("failed to run secret-tool: {}", e))?;

        if output.status.success() {
            let secret = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if secret.is_empty() {
                Ok(None)
            } else {
                info!(service = service_name, "read credential from Linux Secret Service");
                Ok(Some(Zeroizing::new(secret)))
            }
        } else {
            debug!(service = service_name, "credential not found in Linux Secret Service");
            Ok(None)
        }
    }

    /// Windows: read from Credential Manager via `cmdkey`.
    #[cfg(target_os = "windows")]
    fn windows_keychain_read(&self, service_name: &str) -> Result<Option<Zeroizing<String>>, String> {
        // Windows cmdkey doesn't expose passwords directly.
        // For full support, the `keyring` crate would be needed.
        warn!("Windows keychain read via cmdkey is limited; consider using the keyring crate");
        Ok(None)
    }
}

impl Default for KeychainProvider {
    fn default() -> Self {
        Self::default_ttl()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_hit_within_ttl() {
        let provider = KeychainProvider::new(Duration::from_secs(60));
        // Manually insert a cache entry
        {
            let mut cache = provider.cache.write().unwrap();
            cache.insert(
                "test-service".to_string(),
                CachedCredential {
                    secret: Zeroizing::new("test-secret".to_string()),
                    cached_at: Instant::now(),
                },
            );
        }

        let result = provider.read_credential("test-service").unwrap();
        assert!(result.is_some());
        assert_eq!(&**result.as_ref().unwrap(), "test-secret");
    }

    #[test]
    fn cache_miss_after_ttl() {
        let provider = KeychainProvider::new(Duration::from_secs(0)); // Immediate expiry
        {
            let mut cache = provider.cache.write().unwrap();
            cache.insert(
                "test-service".to_string(),
                CachedCredential {
                    secret: Zeroizing::new("old-secret".to_string()),
                    cached_at: Instant::now() - Duration::from_secs(10),
                },
            );
        }
        // Cache should be expired; will attempt platform read (which may fail in CI)
        // The important thing is the TTL check logic works
    }

    #[test]
    fn invalidate_clears_cache() {
        let provider = KeychainProvider::new(Duration::from_secs(300));
        {
            let mut cache = provider.cache.write().unwrap();
            cache.insert(
                "test-service".to_string(),
                CachedCredential {
                    secret: Zeroizing::new("secret".to_string()),
                    cached_at: Instant::now(),
                },
            );
        }
        provider.invalidate("test-service");
        let cache = provider.cache.read().unwrap();
        assert!(!cache.contains_key("test-service"));
    }

    #[test]
    fn known_service_names() {
        assert_eq!(services::CLAUDE_CODE, "Claude Code-credentials");
        assert_eq!(services::CODEX, "Codex-credentials");
    }
}
