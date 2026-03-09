//! Extension ABI — versioned binary interface for dynamically loaded plugins.
//!
//! Defines the stable ABI boundary between the ClawDesk host and dynamically
//! loaded extension shared libraries (.so/.dylib/.dll).
//!
//! ## Versioning
//!
//! The ABI version is a `(major, minor)` pair:
//! - **Major**: Breaking changes — host refuses to load incompatible plugins.
//! - **Minor**: Backward-compatible additions — host loads but older plugins
//!   won't see new capabilities.
//!
//! Compatibility check:
//! ```text
//! compatible(host, plugin) ⟺ host.major == plugin.major ∧ host.minor ≥ plugin.minor
//! ```
//!
//! ## Capability Bitmap
//!
//! Each capability maps to a bit position in a `u64` bitmap. The host advertises
//! supported capabilities; the plugin declares required capabilities. Loading
//! succeeds iff `required & supported == required`.
//!
//! ## Safety
//!
//! All FFI functions use `#[repr(C)]` types for cross-language compatibility.
//! The host validates ABI version and capability bitmap before calling any
//! plugin functions.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Current ABI version exported by the host.
pub const ABI_VERSION_MAJOR: u32 = 1;
pub const ABI_VERSION_MINOR: u32 = 0;

// ─────────────────────────────────────────────────────────────────────────────
// ABI version
// ─────────────────────────────────────────────────────────────────────────────

/// ABI version pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(C)]
pub struct AbiVersion {
    pub major: u32,
    pub minor: u32,
}

impl AbiVersion {
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    /// The current host ABI version.
    pub const fn current() -> Self {
        Self::new(ABI_VERSION_MAJOR, ABI_VERSION_MINOR)
    }

    /// Check if a plugin's ABI version is compatible with this host version.
    ///
    /// `compatible(host, plugin) ⟺ host.major == plugin.major ∧ host.minor ≥ plugin.minor`
    pub fn is_compatible_with(&self, plugin: &AbiVersion) -> bool {
        self.major == plugin.major && self.minor >= plugin.minor
    }
}

impl fmt::Display for AbiVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Capability bitmap
// ─────────────────────────────────────────────────────────────────────────────

/// Capability flags — each maps to a single bit in a `u64`.
///
/// Max 64 capabilities. New capabilities are added at the end (higher bits)
/// to maintain backward compatibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u64)]
pub enum AbiCapability {
    /// Read messages from channels.
    ReadMessages = 1 << 0,
    /// Send messages to channels.
    SendMessages = 1 << 1,
    /// Filesystem access (scoped).
    FileSystem = 1 << 2,
    /// Outbound network access.
    Network = 1 << 3,
    /// Read configuration.
    ReadConfig = 1 << 4,
    /// Write configuration.
    WriteConfig = 1 << 5,
    /// Execute shell commands.
    ExecCommands = 1 << 6,
    /// Access agent context.
    AgentContext = 1 << 7,
    /// Register custom tools.
    RegisterTools = 1 << 8,
    /// Access memory/embedding store.
    MemoryAccess = 1 << 9,
    /// Access to the event bus.
    EventBus = 1 << 10,
    /// Create timers/cron jobs.
    Timers = 1 << 11,
    /// Spawn sub-processes.
    SpawnProcess = 1 << 12,
    /// Access clipboard.
    Clipboard = 1 << 13,
    /// System notifications.
    Notifications = 1 << 14,
    /// Access MCP servers.
    McpAccess = 1 << 15,
}

/// A set of capabilities stored as a bitmap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(C)]
pub struct CapabilityBitmap(pub u64);

impl CapabilityBitmap {
    pub const EMPTY: Self = Self(0);
    pub const ALL: Self = Self(u64::MAX);

    /// Create a bitmap from a single capability.
    pub const fn from_cap(cap: AbiCapability) -> Self {
        Self(cap as u64)
    }

    /// Check if a capability is set.
    pub const fn has(&self, cap: AbiCapability) -> bool {
        (self.0 & cap as u64) != 0
    }

    /// Set a capability.
    pub const fn with(self, cap: AbiCapability) -> Self {
        Self(self.0 | cap as u64)
    }

    /// Clear a capability.
    pub const fn without(self, cap: AbiCapability) -> Self {
        Self(self.0 & !(cap as u64))
    }

    /// Check if all required capabilities are present.
    ///
    /// `satisfied(supported, required) ⟺ required & supported == required`
    pub const fn satisfies(&self, required: &CapabilityBitmap) -> bool {
        (required.0 & self.0) == required.0
    }

    /// Count the number of set capability bits.
    pub const fn count(&self) -> u32 {
        self.0.count_ones()
    }

    /// List all set capabilities.
    pub fn iter(&self) -> impl Iterator<Item = AbiCapability> + '_ {
        ALL_CAPABILITIES
            .iter()
            .copied()
            .filter(|cap| self.has(*cap))
    }
}

impl Default for CapabilityBitmap {
    fn default() -> Self {
        Self::EMPTY
    }
}

impl fmt::Display for CapabilityBitmap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:016x} ({} caps)", self.0, self.count())
    }
}

/// All defined capabilities (for iteration).
const ALL_CAPABILITIES: &[AbiCapability] = &[
    AbiCapability::ReadMessages,
    AbiCapability::SendMessages,
    AbiCapability::FileSystem,
    AbiCapability::Network,
    AbiCapability::ReadConfig,
    AbiCapability::WriteConfig,
    AbiCapability::ExecCommands,
    AbiCapability::AgentContext,
    AbiCapability::RegisterTools,
    AbiCapability::MemoryAccess,
    AbiCapability::EventBus,
    AbiCapability::Timers,
    AbiCapability::SpawnProcess,
    AbiCapability::Clipboard,
    AbiCapability::Notifications,
    AbiCapability::McpAccess,
];

// ─────────────────────────────────────────────────────────────────────────────
// Plugin descriptor (C ABI)
// ─────────────────────────────────────────────────────────────────────────────

/// Plugin descriptor returned by the `clawdesk_plugin_describe` FFI export.
///
/// All fields use C-compatible types for cross-language FFI safety.
#[repr(C)]
pub struct PluginDescriptor {
    /// ABI version the plugin was compiled against.
    pub abi_version: AbiVersion,
    /// Required capabilities bitmap.
    pub required_capabilities: CapabilityBitmap,
    /// Plugin ID as a null-terminated C string.
    pub id: *const std::ffi::c_char,
    /// Plugin name as a null-terminated C string.
    pub name: *const std::ffi::c_char,
    /// Plugin version as a null-terminated C string.
    pub version: *const std::ffi::c_char,
}

// ─────────────────────────────────────────────────────────────────────────────
// ABI compatibility check
// ─────────────────────────────────────────────────────────────────────────────

/// Result of an ABI compatibility check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AbiCheckResult {
    /// Plugin is fully compatible.
    Compatible,
    /// Major version mismatch — cannot load.
    IncompatibleMajor {
        host_major: u32,
        plugin_major: u32,
    },
    /// Missing required capabilities.
    MissingCapabilities {
        missing: CapabilityBitmap,
    },
}

impl AbiCheckResult {
    pub fn is_compatible(&self) -> bool {
        matches!(self, Self::Compatible)
    }
}

/// Check ABI and capability compatibility between host and plugin.
pub fn check_compatibility(
    host_version: &AbiVersion,
    host_capabilities: &CapabilityBitmap,
    plugin_version: &AbiVersion,
    plugin_required: &CapabilityBitmap,
) -> AbiCheckResult {
    // Version check
    if !host_version.is_compatible_with(plugin_version) {
        return AbiCheckResult::IncompatibleMajor {
            host_major: host_version.major,
            plugin_major: plugin_version.major,
        };
    }

    // Capability check
    if !host_capabilities.satisfies(plugin_required) {
        let missing = CapabilityBitmap(plugin_required.0 & !host_capabilities.0);
        return AbiCheckResult::MissingCapabilities { missing };
    }

    AbiCheckResult::Compatible
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compatibility() {
        let host = AbiVersion::new(1, 2);

        // Same version — compatible
        assert!(host.is_compatible_with(&AbiVersion::new(1, 2)));
        // Plugin older minor — compatible
        assert!(host.is_compatible_with(&AbiVersion::new(1, 0)));
        assert!(host.is_compatible_with(&AbiVersion::new(1, 1)));
        // Plugin newer minor — incompatible
        assert!(!host.is_compatible_with(&AbiVersion::new(1, 3)));
        // Different major — incompatible
        assert!(!host.is_compatible_with(&AbiVersion::new(2, 0)));
        assert!(!host.is_compatible_with(&AbiVersion::new(0, 2)));
    }

    #[test]
    fn capability_bitmap() {
        let caps = CapabilityBitmap::EMPTY
            .with(AbiCapability::ReadMessages)
            .with(AbiCapability::Network);

        assert!(caps.has(AbiCapability::ReadMessages));
        assert!(caps.has(AbiCapability::Network));
        assert!(!caps.has(AbiCapability::FileSystem));
        assert_eq!(caps.count(), 2);
    }

    #[test]
    fn capability_satisfies() {
        let host = CapabilityBitmap::EMPTY
            .with(AbiCapability::ReadMessages)
            .with(AbiCapability::SendMessages)
            .with(AbiCapability::Network);

        let required = CapabilityBitmap::EMPTY
            .with(AbiCapability::ReadMessages)
            .with(AbiCapability::Network);

        assert!(host.satisfies(&required));

        let too_much = required.with(AbiCapability::FileSystem);
        assert!(!host.satisfies(&too_much));
    }

    #[test]
    fn full_compatibility_check() {
        let host_ver = AbiVersion::new(1, 2);
        let host_caps = CapabilityBitmap::EMPTY
            .with(AbiCapability::ReadMessages)
            .with(AbiCapability::Network);

        // Compatible plugin
        let result = check_compatibility(
            &host_ver,
            &host_caps,
            &AbiVersion::new(1, 1),
            &CapabilityBitmap::from_cap(AbiCapability::ReadMessages),
        );
        assert!(result.is_compatible());

        // Major version mismatch
        let result = check_compatibility(
            &host_ver,
            &host_caps,
            &AbiVersion::new(2, 0),
            &CapabilityBitmap::EMPTY,
        );
        assert_eq!(
            result,
            AbiCheckResult::IncompatibleMajor {
                host_major: 1,
                plugin_major: 2,
            }
        );

        // Missing capabilities
        let result = check_compatibility(
            &host_ver,
            &host_caps,
            &AbiVersion::new(1, 0),
            &CapabilityBitmap::from_cap(AbiCapability::FileSystem),
        );
        assert!(matches!(result, AbiCheckResult::MissingCapabilities { .. }));
    }

    #[test]
    fn capability_without() {
        let caps = CapabilityBitmap::EMPTY
            .with(AbiCapability::ReadMessages)
            .with(AbiCapability::Network)
            .without(AbiCapability::Network);

        assert!(caps.has(AbiCapability::ReadMessages));
        assert!(!caps.has(AbiCapability::Network));
    }
}
