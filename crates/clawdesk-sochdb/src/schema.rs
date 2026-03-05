//! Schema versioning for persisted JSON blobs.
//!
//! # Problem
//!
//! All SochDB-persisted types (`Session`, `ThreadMeta`, `Message`, graph nodes,
//! trace data) use bare `serde_json` with `#[serde(default)]` for forward compat.
//! This works until you need to *transform* data — rename a field, change a type,
//! split a field into two — at which point silent deserialization failures begin.
//!
//! # Solution
//!
//! Wrap every persisted blob with a version envelope:
//!
//! ```json
//! { "v": 1, "data": { ... original fields ... } }
//! ```
//!
//! The `Versioned<T>` wrapper:
//! - Serializes: wraps T with `{"v": N, "data": T}`
//! - Deserializes: reads version, applies migrations if needed, then deserializes T
//! - Provides a migration registry for upgrading old versions
//!
//! # Usage
//!
//! ```rust,ignore
//! use clawdesk_sochdb::schema::{Versioned, MigrationRegistry};
//!
//! // Register migrations
//! let mut registry = MigrationRegistry::new();
//! registry.register(1, 2, |mut v| {
//!     // Rename "name" → "title"
//!     if let Some(name) = v.as_object_mut().unwrap().remove("name") {
//!         v["title"] = name;
//!     }
//!     Ok(v)
//! });
//!
//! // Serialize with version
//! let versioned = Versioned::new(2, &my_struct);
//! let bytes = versioned.to_bytes()?;
//!
//! // Deserialize with migration
//! let result: MyStruct = Versioned::from_bytes_with_migration(&bytes, 2, &registry)?;
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Version envelope for any persisted JSON blob.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Versioned<T> {
    /// Schema version number.
    pub v: u32,
    /// The actual data.
    pub data: T,
}

impl<T: Serialize> Versioned<T> {
    /// Wrap a value with the current version.
    pub fn new(version: u32, data: T) -> Self {
        Self { v: version, data }
    }

    /// Serialize to JSON bytes.
    pub fn to_bytes(&self) -> Result<Vec<u8>, String> {
        serde_json::to_vec(self).map_err(|e| format!("versioned serialize: {e}"))
    }
}

/// Raw envelope for reading version before deserializing the payload.
#[derive(Debug, Deserialize)]
struct RawEnvelope {
    v: u32,
    data: serde_json::Value,
}

/// Migration function: transforms a JSON value from one version to the next.
pub type MigrationFn = Box<dyn Fn(serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync>;

/// Registry of migration functions for a specific type.
///
/// Migrations are (from_version, to_version) pairs.  
/// The registry can chain migrations: v1 → v2 → v3 automatically.
pub struct MigrationRegistry {
    /// Key: (from_version, to_version)
    migrations: HashMap<(u32, u32), MigrationFn>,
}

impl MigrationRegistry {
    pub fn new() -> Self {
        Self {
            migrations: HashMap::new(),
        }
    }

    /// Register a migration from one version to the next.
    pub fn register(
        &mut self,
        from: u32,
        to: u32,
        f: impl Fn(serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync + 'static,
    ) {
        self.migrations.insert((from, to), Box::new(f));
    }

    /// Apply migrations to bring data from `from_version` to `target_version`.
    ///
    /// Chains single-step migrations: v1→v2→v3 etc.
    pub fn migrate(
        &self,
        mut data: serde_json::Value,
        from_version: u32,
        target_version: u32,
    ) -> Result<serde_json::Value, String> {
        let mut current = from_version;
        while current < target_version {
            let next = current + 1;
            let migration = self.migrations.get(&(current, next))
                .ok_or_else(|| format!(
                    "no migration registered for v{} → v{}",
                    current, next
                ))?;
            data = migration(data)?;
            current = next;
        }
        Ok(data)
    }
}

impl Default for MigrationRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Deserialize a versioned blob, applying migrations if needed.
///
/// This is the primary entry point for reading versioned data.
///
/// ## Backward compatibility
///
/// If the blob has NO version envelope (raw JSON), it's treated as version 1
/// and migrated to the target version. This ensures existing data without
/// version envelopes still works after the migration is introduced.
pub fn from_bytes_versioned<T: for<'de> Deserialize<'de>>(
    bytes: &[u8],
    target_version: u32,
    registry: &MigrationRegistry,
) -> Result<T, String> {
    // Try to parse as versioned envelope first
    let (version, data) = match serde_json::from_slice::<RawEnvelope>(bytes) {
        Ok(envelope) => (envelope.v, envelope.data),
        Err(_) => {
            // Not a versioned envelope — treat as v1 raw data
            let raw: serde_json::Value = serde_json::from_slice(bytes)
                .map_err(|e| format!("deserialize raw blob: {e}"))?;
            (1, raw)
        }
    };

    // Apply migrations if needed
    let migrated = if version < target_version {
        registry.migrate(data, version, target_version)?
    } else {
        data
    };

    // Deserialize to target type
    serde_json::from_value(migrated)
        .map_err(|e| format!("deserialize after migration (v{} → v{}): {e}", version, target_version))
}

/// Serialize a value with version envelope.
pub fn to_bytes_versioned<T: Serialize>(value: &T, version: u32) -> Result<Vec<u8>, String> {
    let envelope = Versioned::new(version, value);
    envelope.to_bytes()
}

/// Check the version of a persisted blob without deserializing the payload.
pub fn peek_version(bytes: &[u8]) -> Option<u32> {
    #[derive(Deserialize)]
    struct VersionOnly { v: u32 }

    serde_json::from_slice::<VersionOnly>(bytes).ok().map(|e| e.v)
}


// ══════════════════════════════════════════════════════════════════════════
// Well-known schema versions — central registry of current versions
// ══════════════════════════════════════════════════════════════════════════

/// Current schema versions for all persisted types.
///
/// Bump these when you change the schema of a persisted type,
/// and add a corresponding migration in `build_migrations()`.
pub mod versions {
    /// Session state blob (`sessions/{id}/state`)
    pub const SESSION: u32 = 1;
    /// Thread metadata (`threads/{id}`)
    pub const THREAD_META: u32 = 1;
    /// Chat message (`msgs/{thread_id}/{ts}/{msg_id}`)
    pub const MESSAGE: u32 = 1;
    /// Agent config (`agents/{id}`)
    pub const AGENT: u32 = 1;
    /// Trace run (`trace/runs/{id}`)
    pub const TRACE_RUN: u32 = 1;
    /// Span attributes (`trace/attrs/{trace_id}/{span_id}`)
    pub const SPAN_ATTRS: u32 = 1;
}

/// Build the global migration registry for all known types.
///
/// Call this once at startup. Currently all types are at v1, so no
/// migrations are registered yet. When you bump a version, add the
/// migration here.
///
/// ## Example: bumping SESSION to v2
///
/// ```rust,ignore
/// registry.register(1, 2, |mut v| {
///     // Add new field "workspace_id" with default
///     v["workspace_id"] = serde_json::json!("default");
///     Ok(v)
/// });
/// ```
pub fn build_migrations() -> MigrationRegistry {
    let registry = MigrationRegistry::new();

    // No migrations yet — all types at v1.
    // Add migrations here when schema changes are needed.

    registry
}

// ══════════════════════════════════════════════════════════════════════════
// GAP-12: Write-time schema validation
// ══════════════════════════════════════════════════════════════════════════

/// Validation function: returns Ok(()) if the value is valid, Err(reason) if not.
pub type ValidationFn = Box<dyn Fn(&serde_json::Value) -> Result<(), String> + Send + Sync>;

/// Schema validator registry for write-time validation.
///
/// Maps key prefix patterns to validation functions. When a value is written
/// to a key matching a pattern, the corresponding validator is called.
///
/// ## Usage
///
/// ```rust,ignore
/// let mut validator = SchemaValidator::new();
/// validator.register("sessions/", |v| {
///     let obj = v.as_object().ok_or("session must be an object")?;
///     if !obj.contains_key("id") {
///         return Err("session missing required field 'id'".into());
///     }
///     Ok(())
/// });
///
/// // Validate before writing
/// validator.validate("sessions/abc/state", &value)?;
/// ```
pub struct SchemaValidator {
    /// (prefix, validator) pairs — checked in order.
    validators: Vec<(String, ValidationFn)>,
}

impl SchemaValidator {
    pub fn new() -> Self {
        Self { validators: Vec::new() }
    }

    /// Register a validator for keys matching a prefix.
    pub fn register(
        &mut self,
        prefix: &str,
        f: impl Fn(&serde_json::Value) -> Result<(), String> + Send + Sync + 'static,
    ) {
        self.validators.push((prefix.to_string(), Box::new(f)));
    }

    /// Validate a JSON value being written to the given key.
    ///
    /// Returns `Ok(())` if valid or no validator matches.
    /// Returns `Err(reason)` if validation fails.
    pub fn validate(&self, key: &str, value: &serde_json::Value) -> Result<(), String> {
        for (prefix, validator) in &self.validators {
            if key.starts_with(prefix) {
                validator(value)?;
            }
        }
        Ok(())
    }

    /// Number of registered validators.
    pub fn count(&self) -> usize {
        self.validators.len()
    }
}

impl Default for SchemaValidator {
    fn default() -> Self {
        Self::new()
    }
}

/// Build the default schema validators for all known ClawDesk types.
///
/// These validators enforce structural invariants at write time, catching
/// bugs early instead of during read-time deserialization.
pub fn build_validators() -> SchemaValidator {
    let mut v = SchemaValidator::new();

    // Session state must have "id" field
    v.register("sessions/", |val| {
        if let Some(obj) = val.as_object() {
            if !obj.contains_key("id") && !obj.contains_key("session_id") {
                return Err("session state must contain 'id' or 'session_id' field".into());
            }
        }
        Ok(())
    });

    // Agent config must have "name" field
    v.register("agents/", |val| {
        if let Some(obj) = val.as_object() {
            if !obj.contains_key("name") {
                return Err("agent config must contain 'name' field".into());
            }
        }
        Ok(())
    });

    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
    struct TestData {
        title: String,
        count: u32,
    }

    #[test]
    fn roundtrip_versioned() {
        let data = TestData { title: "hello".into(), count: 42 };
        let bytes = to_bytes_versioned(&data, 1).unwrap();
        let registry = MigrationRegistry::new();
        let result: TestData = from_bytes_versioned(&bytes, 1, &registry).unwrap();
        assert_eq!(result, data);
    }

    #[test]
    fn migration_chain() {
        let mut registry = MigrationRegistry::new();

        // v1 → v2: rename "name" to "title"
        registry.register(1, 2, |mut v| {
            if let Some(obj) = v.as_object_mut() {
                if let Some(name) = obj.remove("name") {
                    obj.insert("title".to_string(), name);
                }
            }
            Ok(v)
        });

        // v2 → v3: add "count" field with default 0
        registry.register(2, 3, |mut v| {
            if let Some(obj) = v.as_object_mut() {
                if !obj.contains_key("count") {
                    obj.insert("count".to_string(), serde_json::json!(0));
                }
            }
            Ok(v)
        });

        // v1 data
        let v1_json = serde_json::json!({"v": 1, "data": {"name": "hello"}});
        let bytes = serde_json::to_vec(&v1_json).unwrap();

        let result: TestData = from_bytes_versioned(&bytes, 3, &registry).unwrap();
        assert_eq!(result.title, "hello");
        assert_eq!(result.count, 0);
    }

    #[test]
    fn raw_blob_treated_as_v1() {
        // Existing data without version envelope
        let raw = serde_json::json!({"title": "raw", "count": 7});
        let bytes = serde_json::to_vec(&raw).unwrap();

        let registry = MigrationRegistry::new();
        let result: TestData = from_bytes_versioned(&bytes, 1, &registry).unwrap();
        assert_eq!(result.title, "raw");
        assert_eq!(result.count, 7);
    }

    #[test]
    fn peek_version_works() {
        let versioned = serde_json::json!({"v": 3, "data": {}});
        let bytes = serde_json::to_vec(&versioned).unwrap();
        assert_eq!(peek_version(&bytes), Some(3));

        let raw = serde_json::json!({"title": "no version"});
        let bytes = serde_json::to_vec(&raw).unwrap();
        assert_eq!(peek_version(&bytes), None);
    }
}
