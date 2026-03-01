//! AES-256-GCM credential vault — encrypted credential storage at rest.
//!
//! Uses Argon2id for key derivation and AES-256-GCM for authenticated encryption.
//! Key material is zeroized on drop via the `zeroize` crate.

use crate::ExtensionError;
use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::Argon2;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Argon2id parameters
const ARGON2_T_COST: u32 = 3;
const ARGON2_M_COST: u32 = 65536; // 64 MiB
const ARGON2_P_COST: u32 = 4;
const SALT_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;

/// A master key derived from the user's password.
/// Zeroized on drop to prevent cold-boot attacks.
#[derive(ZeroizeOnDrop)]
struct MasterKey {
    bytes: [u8; KEY_LEN],
}

impl MasterKey {
    fn derive(password: &[u8], salt: &[u8]) -> Result<Self, ExtensionError> {
        let argon2 = Argon2::new(
            argon2::Algorithm::Argon2id,
            argon2::Version::V0x13,
            argon2::Params::new(ARGON2_M_COST, ARGON2_T_COST, ARGON2_P_COST, Some(KEY_LEN))
                .map_err(|e| ExtensionError::VaultError(format!("argon2 params: {}", e)))?,
        );

        let mut key = [0u8; KEY_LEN];
        argon2
            .hash_password_into(password, salt, &mut key)
            .map_err(|e| ExtensionError::VaultError(format!("key derivation: {}", e)))?;

        Ok(MasterKey { bytes: key })
    }
}

/// Encrypted credential entry stored in the vault file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptedEntry {
    /// Unique nonce for this entry (96-bit)
    pub nonce: Vec<u8>,
    /// AES-256-GCM ciphertext + 128-bit auth tag
    pub ciphertext: Vec<u8>,
}

/// Vault file format stored at `~/.clawdesk/vault.enc`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultFile {
    /// Vault format version
    pub version: u32,
    /// Salt for Argon2id key derivation
    pub salt: Vec<u8>,
    /// Encrypted entries: credential_name → encrypted data
    pub entries: HashMap<String, EncryptedEntry>,
}

impl VaultFile {
    fn new() -> Self {
        let mut salt = vec![0u8; SALT_LEN];
        rand::thread_rng().fill_bytes(&mut salt);
        Self {
            version: 1,
            salt,
            entries: HashMap::new(),
        }
    }
}

/// AES-256-GCM encrypted credential vault.
///
/// Provides:
/// - AES-256-GCM authenticated encryption (IND-CCA2 secure)
/// - Argon2id key derivation (memory-hard, GPU/ASIC resistant)
/// - Per-credential unique nonces (96-bit random)
/// - Zeroize-on-drop for key material
pub struct CredentialVault {
    /// Path to vault file
    vault_path: PathBuf,
    /// In-memory vault (loaded on unlock)
    vault_file: RwLock<Option<VaultFile>>,
    /// Derived master key (held in memory while unlocked)
    master_key: RwLock<Option<MasterKey>>,
    /// Plaintext cache for hot-path reads
    cache: RwLock<HashMap<String, String>>,
}

impl CredentialVault {
    /// Create a new vault at the default path.
    pub fn new() -> Self {
        let vault_path = directories::ProjectDirs::from("dev", "clawdesk", "clawdesk")
            .map(|d| d.data_dir().join("vault.enc"))
            .unwrap_or_else(|| PathBuf::from("~/.clawdesk/vault.enc"));

        Self::with_path(vault_path)
    }

    /// Create a vault at a custom path.
    pub fn with_path(vault_path: PathBuf) -> Self {
        Self {
            vault_path,
            vault_file: RwLock::new(None),
            master_key: RwLock::new(None),
            cache: RwLock::new(HashMap::new()),
        }
    }

    /// Check if the vault file exists.
    pub fn exists(&self) -> bool {
        self.vault_path.exists()
    }

    /// Check if the vault is unlocked.
    pub async fn is_unlocked(&self) -> bool {
        self.master_key.read().await.is_some()
    }

    /// Initialize a new vault with a master password.
    pub async fn initialize(&self, password: &str) -> Result<(), ExtensionError> {
        if self.exists() {
            return Err(ExtensionError::VaultError(
                "vault already exists — use unlock() instead".into(),
            ));
        }

        let vault_file = VaultFile::new();
        let master_key = MasterKey::derive(password.as_bytes(), &vault_file.salt)?;

        // Save empty vault to disk
        self.save_vault_file(&vault_file).await?;

        *self.vault_file.write().await = Some(vault_file);
        *self.master_key.write().await = Some(master_key);

        info!(path = %self.vault_path.display(), "vault initialized");
        Ok(())
    }

    /// Unlock an existing vault with the master password.
    pub async fn unlock(&self, password: &str) -> Result<(), ExtensionError> {
        let data = tokio::fs::read(&self.vault_path).await.map_err(|e| {
            ExtensionError::VaultError(format!("read vault: {}", e))
        })?;

        let vault_file: VaultFile = serde_json::from_slice(&data).map_err(|e| {
            ExtensionError::VaultError(format!("parse vault: {}", e))
        })?;

        let master_key = MasterKey::derive(password.as_bytes(), &vault_file.salt)?;

        // Verify password by attempting to decrypt all entries
        let mut cache = HashMap::new();
        for (name, entry) in &vault_file.entries {
            match self.decrypt_entry(&master_key, entry) {
                Ok(plaintext) => {
                    cache.insert(name.clone(), plaintext);
                }
                Err(_) => {
                    return Err(ExtensionError::VaultError(
                        "incorrect password or corrupted vault".into(),
                    ));
                }
            }
        }

        *self.vault_file.write().await = Some(vault_file);
        *self.master_key.write().await = Some(master_key);
        *self.cache.write().await = cache;

        let entry_count = self.cache.read().await.len();
        let vault_display = self.vault_path.display().to_string();
        info!(
            path = %vault_display,
            entries = entry_count,
            "vault unlocked"
        );
        Ok(())
    }

    /// Lock the vault — zeroize the master key and clear the plaintext cache.
    pub async fn lock(&self) {
        *self.master_key.write().await = None;
        self.cache.write().await.clear();
        info!("vault locked");
    }

    /// Store a credential in the vault.
    pub async fn store(&self, name: &str, value: &str) -> Result<(), ExtensionError> {
        let master_key = self.master_key.read().await;
        let master_key = master_key
            .as_ref()
            .ok_or_else(|| ExtensionError::VaultError("vault is locked".into()))?;

        let encrypted = self.encrypt_entry(master_key, value)?;

        // Update vault file
        {
            let mut vault_file = self.vault_file.write().await;
            let vf = vault_file
                .as_mut()
                .ok_or_else(|| ExtensionError::VaultError("no vault loaded".into()))?;
            vf.entries.insert(name.to_string(), encrypted);
            self.save_vault_file(vf).await?;
        }

        // Update cache
        self.cache
            .write()
            .await
            .insert(name.to_string(), value.to_string());

        debug!(name, "credential stored in vault");
        Ok(())
    }

    /// Retrieve a credential from the vault.
    pub async fn get(&self, name: &str) -> Result<Option<String>, ExtensionError> {
        if !self.is_unlocked().await {
            return Err(ExtensionError::VaultError("vault is locked".into()));
        }

        let cache = self.cache.read().await;
        Ok(cache.get(name).cloned())
    }

    /// Delete a credential from the vault.
    pub async fn delete(&self, name: &str) -> Result<bool, ExtensionError> {
        let existed;
        {
            let mut vault_file = self.vault_file.write().await;
            let vf = vault_file
                .as_mut()
                .ok_or_else(|| ExtensionError::VaultError("no vault loaded".into()))?;
            existed = vf.entries.remove(name).is_some();
            if existed {
                self.save_vault_file(vf).await?;
            }
        }

        self.cache.write().await.remove(name);
        Ok(existed)
    }

    /// List all credential names in the vault.
    pub async fn list_names(&self) -> Result<Vec<String>, ExtensionError> {
        if !self.is_unlocked().await {
            return Err(ExtensionError::VaultError("vault is locked".into()));
        }

        let cache = self.cache.read().await;
        Ok(cache.keys().cloned().collect())
    }

    // --- Internal ---

    fn encrypt_entry(
        &self,
        key: &MasterKey,
        plaintext: &str,
    ) -> Result<EncryptedEntry, ExtensionError> {
        let cipher = Aes256Gcm::new_from_slice(&key.bytes)
            .map_err(|e| ExtensionError::VaultError(format!("cipher init: {}", e)))?;

        let mut nonce_bytes = [0u8; NONCE_LEN];
        rand::thread_rng().fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| ExtensionError::VaultError(format!("encryption failed: {}", e)))?;

        Ok(EncryptedEntry {
            nonce: nonce_bytes.to_vec(),
            ciphertext,
        })
    }

    fn decrypt_entry(
        &self,
        key: &MasterKey,
        entry: &EncryptedEntry,
    ) -> Result<String, ExtensionError> {
        let cipher = Aes256Gcm::new_from_slice(&key.bytes)
            .map_err(|e| ExtensionError::VaultError(format!("cipher init: {}", e)))?;

        let nonce = Nonce::from_slice(&entry.nonce);

        let plaintext = cipher
            .decrypt(nonce, entry.ciphertext.as_ref())
            .map_err(|_| {
                ExtensionError::VaultError("decryption failed — wrong password or corrupted".into())
            })?;

        String::from_utf8(plaintext)
            .map_err(|e| ExtensionError::VaultError(format!("invalid UTF-8: {}", e)))
    }

    async fn save_vault_file(&self, vault_file: &VaultFile) -> Result<(), ExtensionError> {
        if let Some(parent) = self.vault_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let data = serde_json::to_vec_pretty(vault_file)
            .map_err(|e| ExtensionError::VaultError(format!("serialize vault: {}", e)))?;

        tokio::fs::write(&self.vault_path, &data).await?;

        debug!(path = %self.vault_path.display(), "vault file saved");
        Ok(())
    }
}

impl Default for CredentialVault {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for CredentialVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CredentialVault")
            .field("path", &self.vault_path)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn vault_lifecycle() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_path = dir.path().join("test_vault.enc");
        let vault = CredentialVault::with_path(vault_path);

        // Initialize
        vault.initialize("test_password_123").await.unwrap();
        assert!(vault.is_unlocked().await);

        // Store
        vault.store("api_key", "sk-abc123").await.unwrap();
        vault.store("db_pass", "hunter2").await.unwrap();

        // Retrieve
        assert_eq!(vault.get("api_key").await.unwrap(), Some("sk-abc123".to_string()));
        assert_eq!(vault.get("db_pass").await.unwrap(), Some("hunter2".to_string()));
        assert_eq!(vault.get("nonexistent").await.unwrap(), None);

        // Lock
        vault.lock().await;
        assert!(!vault.is_unlocked().await);
        assert!(vault.get("api_key").await.is_err());

        // Unlock with correct password
        vault.unlock("test_password_123").await.unwrap();
        assert_eq!(vault.get("api_key").await.unwrap(), Some("sk-abc123".to_string()));

        // Delete
        assert!(vault.delete("api_key").await.unwrap());
        assert_eq!(vault.get("api_key").await.unwrap(), None);
    }

    #[tokio::test]
    async fn wrong_password_fails() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault_path = dir.path().join("test_vault2.enc");
        let vault = CredentialVault::with_path(vault_path.clone());

        vault.initialize("correct_password").await.unwrap();
        vault.store("secret", "value").await.unwrap();
        vault.lock().await;

        // Wrong password should fail
        let vault2 = CredentialVault::with_path(vault_path);
        let result = vault2.unlock("wrong_password").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn list_names() {
        let dir = tempfile::TempDir::new().unwrap();
        let vault = CredentialVault::with_path(dir.path().join("vault.enc"));
        vault.initialize("pass").await.unwrap();
        vault.store("key1", "val1").await.unwrap();
        vault.store("key2", "val2").await.unwrap();

        let mut names = vault.list_names().await.unwrap();
        names.sort();
        assert_eq!(names, vec!["key1", "key2"]);
    }
}
