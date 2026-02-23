//! WhatsApp Signal Protocol storage backend using rusqlite.
//!
//! Provides a self-contained SQLite-based storage layer for the WhatsApp
//! Web protocol's cryptographic state. This includes:
//!
//! - **Identity keys** — Signal Protocol identity verification
//! - **Sessions** — Encrypted session state per contact
//! - **Pre-keys** — One-time key exchange material
//! - **Signed pre-keys** — Long-term key exchange material
//! - **Sender keys** — Group messaging encryption
//! - **App state** — WhatsApp app sync (contacts, settings, etc.)
//! - **Device registry** — Multi-device tracking
//!
//! ## Design
//!
//! All tables are partitioned by `device_id` to support multi-device
//! scenarios. The database uses WAL mode for better read concurrency.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use clawdesk_channels::whatsapp_storage::WhatsAppStore;
//!
//! let store = WhatsAppStore::new("/path/to/whatsapp.db").unwrap();
//! let key_bytes = vec![0u8; 33];
//! store.put_identity("user@s.whatsapp.net", &key_bytes).await.unwrap();
//! ```

use rusqlite::{params, Connection};
use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Error type for WhatsApp storage operations.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("store error: {0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// WhatsApp Signal Protocol storage backend.
///
/// Uses rusqlite directly, avoiding Diesel/libsqlite3-sys conflicts.
/// All operations are partitioned by `device_id` for multi-device support.
#[derive(Clone)]
pub struct WhatsAppStore {
    /// Database file path.
    db_path: String,
    /// SQLite connection (thread-safe via async Mutex).
    conn: Arc<Mutex<Connection>>,
    /// Device ID for this session.
    device_id: i32,
}

impl WhatsAppStore {
    /// Create a new WhatsApp storage backend.
    ///
    /// Opens (or creates) a SQLite database at `db_path` and initializes
    /// the schema. Uses WAL mode for better concurrency.
    pub fn new<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        let db_path = db_path.as_ref().to_string_lossy().to_string();

        // Create parent directory if needed
        if let Some(parent) = Path::new(&db_path).parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&db_path)?;

        // WAL mode for better concurrency + NORMAL sync for performance
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;",
        )?;

        let store = Self {
            db_path,
            conn: Arc::new(Mutex::new(conn)),
            device_id: 1,
        };

        store.init_schema_sync()?;
        Ok(store)
    }

    /// Create with a specific device ID.
    pub fn with_device_id<P: AsRef<Path>>(db_path: P, device_id: i32) -> Result<Self> {
        let mut store = Self::new(db_path)?;
        store.device_id = device_id;
        Ok(store)
    }

    /// Get the database file path.
    pub fn db_path(&self) -> &str {
        &self.db_path
    }

    /// Get the current device ID.
    pub fn device_id(&self) -> i32 {
        self.device_id
    }

    /// Initialize all 13 database tables.
    fn init_schema_sync(&self) -> Result<()> {
        // We need sync access during construction
        let conn = self.conn.try_lock().map_err(|_| {
            StoreError::Other("Failed to acquire lock during schema init".to_string())
        })?;

        conn.execute_batch(
            "-- Main device table
            CREATE TABLE IF NOT EXISTS device (
                id INTEGER PRIMARY KEY,
                lid TEXT,
                pn TEXT,
                registration_id INTEGER NOT NULL,
                noise_key BLOB NOT NULL,
                identity_key BLOB NOT NULL,
                signed_pre_key BLOB NOT NULL,
                signed_pre_key_id INTEGER NOT NULL,
                signed_pre_key_signature BLOB NOT NULL,
                adv_secret_key BLOB NOT NULL,
                account BLOB,
                push_name TEXT NOT NULL,
                app_version_primary INTEGER NOT NULL,
                app_version_secondary INTEGER NOT NULL,
                app_version_tertiary INTEGER NOT NULL,
                app_version_last_fetched_ms INTEGER NOT NULL,
                edge_routing_info BLOB,
                props_hash TEXT
            );

            -- Signal identity keys
            CREATE TABLE IF NOT EXISTS identities (
                address TEXT NOT NULL,
                key BLOB NOT NULL,
                device_id INTEGER NOT NULL,
                PRIMARY KEY (address, device_id)
            );

            -- Signal protocol sessions
            CREATE TABLE IF NOT EXISTS sessions (
                address TEXT NOT NULL,
                record BLOB NOT NULL,
                device_id INTEGER NOT NULL,
                PRIMARY KEY (address, device_id)
            );

            -- Pre-keys for key exchange
            CREATE TABLE IF NOT EXISTS prekeys (
                id INTEGER NOT NULL,
                key BLOB NOT NULL,
                uploaded INTEGER NOT NULL DEFAULT 0,
                device_id INTEGER NOT NULL,
                PRIMARY KEY (id, device_id)
            );

            -- Signed pre-keys
            CREATE TABLE IF NOT EXISTS signed_prekeys (
                id INTEGER NOT NULL,
                record BLOB NOT NULL,
                device_id INTEGER NOT NULL,
                PRIMARY KEY (id, device_id)
            );

            -- Sender keys for group messaging
            CREATE TABLE IF NOT EXISTS sender_keys (
                address TEXT NOT NULL,
                record BLOB NOT NULL,
                device_id INTEGER NOT NULL,
                PRIMARY KEY (address, device_id)
            );

            -- App state sync keys
            CREATE TABLE IF NOT EXISTS app_state_keys (
                key_id BLOB NOT NULL,
                key_data BLOB NOT NULL,
                device_id INTEGER NOT NULL,
                PRIMARY KEY (key_id, device_id)
            );

            -- App state versions
            CREATE TABLE IF NOT EXISTS app_state_versions (
                name TEXT NOT NULL,
                state_data BLOB NOT NULL,
                device_id INTEGER NOT NULL,
                PRIMARY KEY (name, device_id)
            );

            -- App state mutation MACs
            CREATE TABLE IF NOT EXISTS app_state_mutation_macs (
                name TEXT NOT NULL,
                version INTEGER NOT NULL,
                index_mac BLOB NOT NULL,
                value_mac BLOB NOT NULL,
                device_id INTEGER NOT NULL,
                PRIMARY KEY (name, index_mac, device_id)
            );

            -- LID to phone number mapping
            CREATE TABLE IF NOT EXISTS lid_pn_mapping (
                lid TEXT NOT NULL,
                phone_number TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                learning_source TEXT NOT NULL,
                updated_at INTEGER NOT NULL,
                device_id INTEGER NOT NULL,
                PRIMARY KEY (lid, device_id)
            );

            -- SKDM recipients tracking
            CREATE TABLE IF NOT EXISTS skdm_recipients (
                group_jid TEXT NOT NULL,
                device_jid TEXT NOT NULL,
                device_id INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (group_jid, device_jid, device_id)
            );

            -- Device registry for multi-device
            CREATE TABLE IF NOT EXISTS device_registry (
                user_id TEXT NOT NULL,
                devices_json TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                phash TEXT,
                device_id INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (user_id, device_id)
            );

            -- Base keys for collision detection
            CREATE TABLE IF NOT EXISTS base_keys (
                address TEXT NOT NULL,
                message_id TEXT NOT NULL,
                base_key BLOB NOT NULL,
                device_id INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                PRIMARY KEY (address, message_id, device_id)
            );

            -- Sender key status for lazy deletion
            CREATE TABLE IF NOT EXISTS sender_key_status (
                group_jid TEXT NOT NULL,
                participant TEXT NOT NULL,
                device_id INTEGER NOT NULL,
                marked_at INTEGER NOT NULL,
                PRIMARY KEY (group_jid, participant, device_id)
            );

            -- Trusted contact tokens
            CREATE TABLE IF NOT EXISTS tc_tokens (
                jid TEXT NOT NULL,
                token BLOB NOT NULL,
                token_timestamp INTEGER NOT NULL,
                sender_timestamp INTEGER,
                device_id INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (jid, device_id)
            );",
        )?;
        Ok(())
    }

    // ─── Identity operations ───────────────────────────────────

    /// Store an identity key for an address.
    pub async fn put_identity(&self, address: &str, key: &[u8]) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO identities (address, key, device_id)
             VALUES (?1, ?2, ?3)",
            params![address, key, self.device_id],
        )?;
        Ok(())
    }

    /// Load an identity key for an address.
    pub async fn load_identity(&self, address: &str) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT key FROM identities WHERE address = ?1 AND device_id = ?2",
            params![address, self.device_id],
            |row| row.get::<_, Vec<u8>>(0),
        );
        match result {
            Ok(key) => Ok(Some(key)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete an identity key.
    pub async fn delete_identity(&self, address: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM identities WHERE address = ?1 AND device_id = ?2",
            params![address, self.device_id],
        )?;
        Ok(())
    }

    // ─── Session operations ───────────────────────────────────

    /// Store a session record.
    pub async fn put_session(&self, address: &str, record: &[u8]) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO sessions (address, record, device_id)
             VALUES (?1, ?2, ?3)",
            params![address, record, self.device_id],
        )?;
        Ok(())
    }

    /// Load a session record.
    pub async fn get_session(&self, address: &str) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT record FROM sessions WHERE address = ?1 AND device_id = ?2",
            params![address, self.device_id],
            |row| row.get::<_, Vec<u8>>(0),
        );
        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete a session.
    pub async fn delete_session(&self, address: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM sessions WHERE address = ?1 AND device_id = ?2",
            params![address, self.device_id],
        )?;
        Ok(())
    }

    // ─── PreKey operations ─────────────────────────────────────

    /// Store a pre-key.
    pub async fn store_prekey(&self, id: u32, record: &[u8], uploaded: bool) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO prekeys (id, key, uploaded, device_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![id, record, uploaded, self.device_id],
        )?;
        Ok(())
    }

    /// Load a pre-key.
    pub async fn load_prekey(&self, id: u32) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT key FROM prekeys WHERE id = ?1 AND device_id = ?2",
            params![id, self.device_id],
            |row| row.get::<_, Vec<u8>>(0),
        );
        match result {
            Ok(key) => Ok(Some(key)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Remove a pre-key.
    pub async fn remove_prekey(&self, id: u32) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM prekeys WHERE id = ?1 AND device_id = ?2",
            params![id, self.device_id],
        )?;
        Ok(())
    }

    /// Count uploaded pre-keys.
    pub async fn count_uploaded_prekeys(&self) -> Result<u32> {
        let conn = self.conn.lock().await;
        let count: u32 = conn.query_row(
            "SELECT COUNT(*) FROM prekeys WHERE uploaded = 1 AND device_id = ?1",
            params![self.device_id],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    // ─── Signed pre-key operations ────────────────────────────

    /// Store a signed pre-key.
    pub async fn store_signed_prekey(&self, id: u32, record: &[u8]) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO signed_prekeys (id, record, device_id)
             VALUES (?1, ?2, ?3)",
            params![id, record, self.device_id],
        )?;
        Ok(())
    }

    /// Load a signed pre-key.
    pub async fn load_signed_prekey(&self, id: u32) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT record FROM signed_prekeys WHERE id = ?1 AND device_id = ?2",
            params![id, self.device_id],
            |row| row.get::<_, Vec<u8>>(0),
        );
        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // ─── Sender key operations (group messaging) ──────────────

    /// Store a sender key for group messaging.
    pub async fn put_sender_key(&self, address: &str, record: &[u8]) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO sender_keys (address, record, device_id)
             VALUES (?1, ?2, ?3)",
            params![address, record, self.device_id],
        )?;
        Ok(())
    }

    /// Load a sender key.
    pub async fn get_sender_key(&self, address: &str) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT record FROM sender_keys WHERE address = ?1 AND device_id = ?2",
            params![address, self.device_id],
            |row| row.get::<_, Vec<u8>>(0),
        );
        match result {
            Ok(record) => Ok(Some(record)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete a sender key.
    pub async fn delete_sender_key(&self, address: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM sender_keys WHERE address = ?1 AND device_id = ?2",
            params![address, self.device_id],
        )?;
        Ok(())
    }

    // ─── App state operations ─────────────────────────────────

    /// Store an app state sync key.
    pub async fn put_app_state_key(&self, key_id: &[u8], key_data: &[u8]) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO app_state_keys (key_id, key_data, device_id)
             VALUES (?1, ?2, ?3)",
            params![key_id, key_data, self.device_id],
        )?;
        Ok(())
    }

    /// Load an app state sync key.
    pub async fn get_app_state_key(&self, key_id: &[u8]) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT key_data FROM app_state_keys WHERE key_id = ?1 AND device_id = ?2",
            params![key_id, self.device_id],
            |row| row.get::<_, Vec<u8>>(0),
        );
        match result {
            Ok(data) => Ok(Some(data)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Store app state version data.
    pub async fn put_app_state_version(&self, name: &str, state_data: &[u8]) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO app_state_versions (name, state_data, device_id)
             VALUES (?1, ?2, ?3)",
            params![name, state_data, self.device_id],
        )?;
        Ok(())
    }

    /// Load app state version data.
    pub async fn get_app_state_version(&self, name: &str) -> Result<Option<Vec<u8>>> {
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT state_data FROM app_state_versions WHERE name = ?1 AND device_id = ?2",
            params![name, self.device_id],
            |row| row.get::<_, Vec<u8>>(0),
        );
        match result {
            Ok(data) => Ok(Some(data)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete all app state for a name.
    pub async fn delete_app_state(&self, name: &str) -> Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "DELETE FROM app_state_versions WHERE name = ?1 AND device_id = ?2",
            params![name, self.device_id],
        )?;
        conn.execute(
            "DELETE FROM app_state_mutation_macs WHERE name = ?1 AND device_id = ?2",
            params![name, self.device_id],
        )?;
        Ok(())
    }

    // ─── LID/Phone mapping ────────────────────────────────────

    /// Store a LID → phone number mapping.
    pub async fn put_lid_mapping(
        &self,
        lid: &str,
        phone_number: &str,
        source: &str,
    ) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO lid_pn_mapping
             (lid, phone_number, created_at, learning_source, updated_at, device_id)
             VALUES (?1, ?2, ?3, ?4, ?3, ?5)",
            params![lid, phone_number, now, source, self.device_id],
        )?;
        Ok(())
    }

    /// Look up a phone number by LID.
    pub async fn get_phone_by_lid(&self, lid: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT phone_number FROM lid_pn_mapping WHERE lid = ?1 AND device_id = ?2",
            params![lid, self.device_id],
            |row| row.get::<_, String>(0),
        );
        match result {
            Ok(pn) => Ok(Some(pn)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // ─── Device registry ──────────────────────────────────────

    /// Store device list for a user.
    pub async fn put_device_list(&self, user_id: &str, devices_json: &str) -> Result<()> {
        let now = chrono::Utc::now().timestamp();
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR REPLACE INTO device_registry
             (user_id, devices_json, timestamp, device_id, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?3)",
            params![user_id, devices_json, now, self.device_id],
        )?;
        Ok(())
    }

    /// Get device list for a user.
    pub async fn get_device_list(&self, user_id: &str) -> Result<Option<String>> {
        let conn = self.conn.lock().await;
        let result = conn.query_row(
            "SELECT devices_json FROM device_registry WHERE user_id = ?1 AND device_id = ?2",
            params![user_id, self.device_id],
            |row| row.get::<_, String>(0),
        );
        match result {
            Ok(json) => Ok(Some(json)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    // ─── Cleanup ──────────────────────────────────────────────

    /// Delete all data for this device.
    pub async fn clear_device_data(&self) -> Result<()> {
        let conn = self.conn.lock().await;
        let tables = [
            "identities",
            "sessions",
            "prekeys",
            "signed_prekeys",
            "sender_keys",
            "app_state_keys",
            "app_state_versions",
            "app_state_mutation_macs",
            "lid_pn_mapping",
            "skdm_recipients",
            "device_registry",
            "base_keys",
            "sender_key_status",
            "tc_tokens",
        ];
        for table in tables {
            conn.execute(
                &format!("DELETE FROM {table} WHERE device_id = ?1"),
                params![self.device_id],
            )?;
        }
        Ok(())
    }
}

// ═══════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> WhatsAppStore {
        let dir = std::env::temp_dir().join(format!(
            "clawdesk_wa_test_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        WhatsAppStore::new(dir.join("test.db")).unwrap()
    }

    #[tokio::test]
    async fn identity_crud() {
        let store = temp_store();
        let key = vec![1u8; 32];

        // Put
        store.put_identity("user@s.whatsapp.net", &key).await.unwrap();

        // Load
        let loaded = store.load_identity("user@s.whatsapp.net").await.unwrap();
        assert_eq!(loaded, Some(key));

        // Delete
        store.delete_identity("user@s.whatsapp.net").await.unwrap();
        let deleted = store.load_identity("user@s.whatsapp.net").await.unwrap();
        assert_eq!(deleted, None);
    }

    #[tokio::test]
    async fn session_crud() {
        let store = temp_store();
        let record = vec![42u8; 100];

        store.put_session("user@s.whatsapp.net", &record).await.unwrap();

        let loaded = store.get_session("user@s.whatsapp.net").await.unwrap();
        assert_eq!(loaded, Some(record));

        store.delete_session("user@s.whatsapp.net").await.unwrap();
        assert_eq!(store.get_session("user@s.whatsapp.net").await.unwrap(), None);
    }

    #[tokio::test]
    async fn prekey_crud() {
        let store = temp_store();
        let key = vec![7u8; 64];

        store.store_prekey(1, &key, false).await.unwrap();
        assert_eq!(store.load_prekey(1).await.unwrap(), Some(key));
        assert_eq!(store.count_uploaded_prekeys().await.unwrap(), 0);

        store.store_prekey(2, &[0; 32], true).await.unwrap();
        assert_eq!(store.count_uploaded_prekeys().await.unwrap(), 1);

        store.remove_prekey(1).await.unwrap();
        assert_eq!(store.load_prekey(1).await.unwrap(), None);
    }

    #[tokio::test]
    async fn sender_key_crud() {
        let store = temp_store();
        let record = vec![99u8; 50];

        store.put_sender_key("group:device", &record).await.unwrap();
        assert_eq!(store.get_sender_key("group:device").await.unwrap(), Some(record));

        store.delete_sender_key("group:device").await.unwrap();
        assert_eq!(store.get_sender_key("group:device").await.unwrap(), None);
    }

    #[tokio::test]
    async fn app_state_crud() {
        let store = temp_store();
        let data = vec![0xAA; 128];

        store.put_app_state_version("critical_block", &data).await.unwrap();
        assert_eq!(
            store.get_app_state_version("critical_block").await.unwrap(),
            Some(data)
        );

        store.delete_app_state("critical_block").await.unwrap();
        assert_eq!(
            store.get_app_state_version("critical_block").await.unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn lid_mapping() {
        let store = temp_store();

        store.put_lid_mapping("lid123", "+1234567890", "message").await.unwrap();
        assert_eq!(
            store.get_phone_by_lid("lid123").await.unwrap(),
            Some("+1234567890".to_string())
        );
    }

    #[tokio::test]
    async fn device_isolation() {
        let dir = std::env::temp_dir().join(format!(
            "clawdesk_wa_test_iso_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = dir.join("test.db");

        let store1 = WhatsAppStore::new(&db).unwrap();
        let store2 = WhatsAppStore::with_device_id(&db, 2).unwrap();

        let key1 = vec![1u8; 32];
        let key2 = vec![2u8; 32];

        store1.put_identity("same_address", &key1).await.unwrap();
        store2.put_identity("same_address", &key2).await.unwrap();

        // Each device sees its own data
        assert_eq!(
            store1.load_identity("same_address").await.unwrap(),
            Some(key1)
        );
        assert_eq!(
            store2.load_identity("same_address").await.unwrap(),
            Some(key2)
        );
    }

    #[tokio::test]
    async fn clear_device_data() {
        let store = temp_store();

        store.put_identity("addr1", &[1; 32]).await.unwrap();
        store.put_session("addr1", &[2; 64]).await.unwrap();
        store.store_prekey(1, &[3; 32], false).await.unwrap();

        store.clear_device_data().await.unwrap();

        assert_eq!(store.load_identity("addr1").await.unwrap(), None);
        assert_eq!(store.get_session("addr1").await.unwrap(), None);
        assert_eq!(store.load_prekey(1).await.unwrap(), None);
    }

    #[tokio::test]
    async fn missing_key_returns_none() {
        let store = temp_store();
        assert_eq!(store.load_identity("nonexistent").await.unwrap(), None);
        assert_eq!(store.get_session("nonexistent").await.unwrap(), None);
        assert_eq!(store.load_prekey(999).await.unwrap(), None);
        assert_eq!(store.get_sender_key("nonexistent").await.unwrap(), None);
        assert_eq!(store.get_app_state_key(&[0; 16]).await.unwrap(), None);
        assert_eq!(store.get_phone_by_lid("nonexistent").await.unwrap(), None);
    }
}
