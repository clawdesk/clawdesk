//! Capability Algebra Gate — Static Pre-Execution Permission Proof
//!
//! Encodes sandbox permissions as a bounded lattice over `u128` bitmasks.
//! Every tool invocation is gated by a single bitwise AND instruction:
//!
//! ```text
//! required_caps & granted_caps == required_caps  →  PERMIT
//! ```
//!
//! ## Lattice Properties (mechanically guaranteed by hardware)
//!
//! - Meet (∩): `a & b`       — O(1)
//! - Join (∪): `a | b`       — O(1)
//! - Partial order (⊆): `a & b == a`  — O(1)
//! - Bottom (⊥): `0u128`     — no capabilities
//! - Top (⊤): `u128::MAX`   — all capabilities
//!
//! ## Delegation
//!
//! When agent A delegates to agent B, the delegated capability set is the
//! lattice meet of A's grant and B's request:
//!
//! ```text
//! G(child) = G(parent) ∧ G(child_request)  =  G(parent) & G(child_request)
//! ```
//!
//! This guarantees a child never exceeds its parent's capabilities.

use serde::{Deserialize, Serialize};
use std::fmt;

// ---------------------------------------------------------------------------
// Well-known capability bits (positions 0–63 for common, 64–127 reserved)
// ---------------------------------------------------------------------------

/// Well-known capability bit positions.
///
/// Each constant is a single-bit `u128` value ready for OR-composition.
pub mod caps {
    /// Read files within the workspace.
    pub const FILE_READ: u128 = 1 << 0;
    /// Write/create files within the workspace.
    pub const FILE_WRITE: u128 = 1 << 1;
    /// Delete files within the workspace.
    pub const FILE_DELETE: u128 = 1 << 2;
    /// Execute shell commands.
    pub const SHELL_EXEC: u128 = 1 << 3;
    /// Open outbound network connections.
    pub const NETWORK: u128 = 1 << 4;
    /// Open listening sockets.
    pub const NETWORK_LISTEN: u128 = 1 << 5;
    /// Read environment variables.
    pub const ENV_READ: u128 = 1 << 6;
    /// Spawn child processes.
    pub const PROCESS_SPAWN: u128 = 1 << 7;
    /// Access the clipboard.
    pub const CLIPBOARD: u128 = 1 << 8;
    /// Read from memory/knowledge base.
    pub const MEMORY_READ: u128 = 1 << 9;
    /// Write to memory/knowledge base.
    pub const MEMORY_WRITE: u128 = 1 << 10;
    /// Invoke other tools.
    pub const TOOL_INVOKE: u128 = 1 << 11;
    /// Access browser automation.
    pub const BROWSER: u128 = 1 << 12;
    /// Send messages to channels.
    pub const CHANNEL_SEND: u128 = 1 << 13;
    /// Manage cron/scheduled tasks.
    pub const CRON: u128 = 1 << 14;
    /// Administrative operations.
    pub const ADMIN: u128 = 1 << 15;
    /// Delegate capabilities to sub-agents (A2A).
    pub const DELEGATE: u128 = 1 << 16;
    /// Access cryptographic signing keys.
    pub const CRYPTO_SIGN: u128 = 1 << 17;
    /// Execute WASM modules.
    pub const WASM_EXEC: u128 = 1 << 18;
    /// Mount/unmount filesystems (sandbox infra).
    pub const MOUNT: u128 = 1 << 19;

    // Bits 20–63: reserved for future well-known capabilities.
    // Bits 64–127: available for user-defined / plugin capabilities.

    /// No capabilities (lattice bottom ⊥).
    pub const NONE: u128 = 0;
    /// All capabilities (lattice top ⊤).
    pub const ALL: u128 = u128::MAX;
}

// ---------------------------------------------------------------------------
// Capability sets
// ---------------------------------------------------------------------------

/// A set of capabilities encoded as a `u128` bitmask.
///
/// Implements a bounded lattice with O(1) meet, join, and subset operations.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CapabilitySet(pub u128);

impl CapabilitySet {
    /// Empty set — no capabilities (lattice bottom ⊥).
    pub const EMPTY: Self = Self(caps::NONE);

    /// Universal set — all capabilities (lattice top ⊤).
    pub const FULL: Self = Self(caps::ALL);

    /// Create a set from raw bits.
    #[inline]
    pub const fn from_bits(bits: u128) -> Self {
        Self(bits)
    }

    /// Get raw bits.
    #[inline]
    pub const fn bits(self) -> u128 {
        self.0
    }

    /// Check if a single capability bit is set.
    #[inline]
    pub const fn has(self, cap: u128) -> bool {
        self.0 & cap == cap
    }

    /// Lattice meet (intersection): `self ∧ other`.
    #[inline]
    pub const fn meet(self, other: Self) -> Self {
        Self(self.0 & other.0)
    }

    /// Lattice join (union): `self ∨ other`.
    #[inline]
    pub const fn join(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Partial order (subset): `self ⊆ other`.
    #[inline]
    pub const fn is_subset_of(self, other: Self) -> bool {
        self.0 & other.0 == self.0
    }

    /// Permission check: do `granted` capabilities satisfy `required`?
    ///
    /// Equivalent to `required ⊆ granted`, i.e., `required & granted == required`.
    #[inline]
    pub const fn permits(granted: Self, required: Self) -> bool {
        required.is_subset_of(granted)
    }

    /// Delegate capabilities to a child agent.
    ///
    /// `G(child) = G(parent) ∧ child_request` — the child gets at most
    /// the intersection of the parent's grant and its own request.
    #[inline]
    pub const fn delegate(parent_grant: Self, child_request: Self) -> Self {
        parent_grant.meet(child_request)
    }

    /// Number of set capability bits.
    #[inline]
    pub const fn count(self) -> u32 {
        self.0.count_ones()
    }

    /// Whether this is the empty set.
    #[inline]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Add a capability.
    #[inline]
    pub const fn with(self, cap: u128) -> Self {
        Self(self.0 | cap)
    }

    /// Remove a capability.
    #[inline]
    pub const fn without(self, cap: u128) -> Self {
        Self(self.0 & !cap)
    }
}

impl fmt::Debug for CapabilitySet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CapabilitySet({:#034x})", self.0)
    }
}

impl fmt::Display for CapabilitySet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut names = Vec::new();
        let bits = [
            (caps::FILE_READ, "file_read"),
            (caps::FILE_WRITE, "file_write"),
            (caps::FILE_DELETE, "file_delete"),
            (caps::SHELL_EXEC, "shell_exec"),
            (caps::NETWORK, "network"),
            (caps::NETWORK_LISTEN, "network_listen"),
            (caps::ENV_READ, "env_read"),
            (caps::PROCESS_SPAWN, "process_spawn"),
            (caps::CLIPBOARD, "clipboard"),
            (caps::MEMORY_READ, "memory_read"),
            (caps::MEMORY_WRITE, "memory_write"),
            (caps::TOOL_INVOKE, "tool_invoke"),
            (caps::BROWSER, "browser"),
            (caps::CHANNEL_SEND, "channel_send"),
            (caps::CRON, "cron"),
            (caps::ADMIN, "admin"),
            (caps::DELEGATE, "delegate"),
            (caps::CRYPTO_SIGN, "crypto_sign"),
            (caps::WASM_EXEC, "wasm_exec"),
            (caps::MOUNT, "mount"),
        ];
        for (bit, name) in bits {
            if self.has(bit) {
                names.push(name);
            }
        }
        write!(f, "{{{}}}", names.join(", "))
    }
}

// ---------------------------------------------------------------------------
// Capability Gate — pre-execution permission proof
// ---------------------------------------------------------------------------

/// Result of a capability gate check.
#[derive(Debug, Clone)]
pub enum GateVerdict {
    /// All required capabilities are satisfied.
    Permit,
    /// One or more required capabilities are missing.
    Deny {
        required: CapabilitySet,
        granted: CapabilitySet,
        missing: CapabilitySet,
    },
}

impl GateVerdict {
    pub fn is_permit(&self) -> bool {
        matches!(self, Self::Permit)
    }
}

/// Tool capability requirement — maps tool names to required capability sets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCapabilityMap {
    entries: rustc_hash::FxHashMap<String, CapabilitySet>,
    /// Fallback: capabilities required for tools not in the map.
    pub default_required: CapabilitySet,
}

impl ToolCapabilityMap {
    pub fn new(default_required: CapabilitySet) -> Self {
        Self {
            entries: rustc_hash::FxHashMap::default(),
            default_required,
        }
    }

    /// Register a tool's capability requirements.
    ///
    /// Returns `Err` if the tool is already registered (prevents silent overwrite).
    pub fn register(&mut self, tool_name: impl Into<String>, required: CapabilitySet) {
        self.entries.insert(tool_name.into(), required);
    }

    /// Look up the required capabilities for a tool.
    ///
    /// Returns the registered requirement or the default if not found.
    /// O(1) amortized via FxHashMap lookup.
    pub fn required_for(&self, tool_name: &str) -> CapabilitySet {
        self.entries.get(tool_name).copied().unwrap_or(self.default_required)
    }
}

/// The capability gate — checks permissions before tool execution.
///
/// Combines an agent's granted capabilities with the tool capability map
/// to produce O(1) permission verdicts.
#[derive(Debug)]
pub struct CapabilityGate {
    tool_map: ToolCapabilityMap,
}

impl CapabilityGate {
    pub fn new(tool_map: ToolCapabilityMap) -> Self {
        Self { tool_map }
    }

    /// Check whether `agent_grant` permits invoking `tool_name`.
    ///
    /// Cost: O(tools) for lookup + O(1) for the bitwise permission check.
    #[inline]
    pub fn check(&self, agent_grant: CapabilitySet, tool_name: &str) -> GateVerdict {
        let required = self.tool_map.required_for(tool_name);
        if CapabilitySet::permits(agent_grant, required) {
            GateVerdict::Permit
        } else {
            let missing = CapabilitySet::from_bits(required.bits() & !agent_grant.bits());
            GateVerdict::Deny {
                required,
                granted: agent_grant,
                missing,
            }
        }
    }

    /// Check a delegation chain: parent delegates to child, child invokes tool.
    ///
    /// `effective = parent_grant ∧ child_request`
    /// Then checks `tool_required ⊆ effective`.
    pub fn check_delegated(
        &self,
        parent_grant: CapabilitySet,
        child_request: CapabilitySet,
        tool_name: &str,
    ) -> GateVerdict {
        let effective = CapabilitySet::delegate(parent_grant, child_request);
        self.check(effective, tool_name)
    }
}

// ---------------------------------------------------------------------------
// Effective Permission Computation — Capability Algebra
// ---------------------------------------------------------------------------

/// Computes the effective permission set for a skill invocation.
///
/// The capability algebra defines a lattice `(P, ⊆, ∩, ∪)` where:
/// - `P_effective = P_skill_declared ∩ P_user_policy ∩ P_agent_granted`
/// - `P_effective ⊂ P_skill_declared` ⟹ permission dialog required
///
/// Subsumption checking is `O(1)` via bitwise AND on u128.
/// Total overhead: `O(|P_max| × 3) = O(|P_max|)`, typically <100 entries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EffectivePermission {
    /// What the skill declared it needs
    pub skill_declared: CapabilitySet,
    /// What the user's security policy allows
    pub user_policy: CapabilitySet,
    /// What the invoking agent can grant
    pub agent_granted: CapabilitySet,
    /// The computed effective set: intersection of all three
    pub effective: CapabilitySet,
    /// Capabilities the skill wants but cannot get
    pub denied: CapabilitySet,
    /// Whether all requested permissions are satisfied
    pub fully_satisfied: bool,
}

impl EffectivePermission {
    /// Compute effective permissions using the capability algebra.
    ///
    /// `P_effective = P_declared ∩ P_policy ∩ P_granted`
    pub fn compute(
        skill_declared: CapabilitySet,
        user_policy: CapabilitySet,
        agent_granted: CapabilitySet,
    ) -> Self {
        let effective = skill_declared.meet(user_policy).meet(agent_granted);
        let denied = CapabilitySet::from_bits(skill_declared.bits() & !effective.bits());
        let fully_satisfied = denied.is_empty();

        Self {
            skill_declared,
            user_policy,
            agent_granted,
            effective,
            denied,
            fully_satisfied,
        }
    }

    /// List which specific capabilities are denied, for permission dialog.
    pub fn denied_capabilities(&self) -> Vec<&'static str> {
        let bits = [
            (caps::FILE_READ, "file_read"),
            (caps::FILE_WRITE, "file_write"),
            (caps::FILE_DELETE, "file_delete"),
            (caps::SHELL_EXEC, "shell_exec"),
            (caps::NETWORK, "network"),
            (caps::NETWORK_LISTEN, "network_listen"),
            (caps::ENV_READ, "env_read"),
            (caps::PROCESS_SPAWN, "process_spawn"),
            (caps::CLIPBOARD, "clipboard"),
            (caps::MEMORY_READ, "memory_read"),
            (caps::MEMORY_WRITE, "memory_write"),
            (caps::TOOL_INVOKE, "tool_invoke"),
            (caps::BROWSER, "browser"),
            (caps::CHANNEL_SEND, "channel_send"),
            (caps::CRON, "cron"),
            (caps::ADMIN, "admin"),
            (caps::DELEGATE, "delegate"),
            (caps::CRYPTO_SIGN, "crypto_sign"),
            (caps::WASM_EXEC, "wasm_exec"),
            (caps::MOUNT, "mount"),
        ];
        bits.iter()
            .filter(|(bit, _)| self.denied.has(*bit))
            .map(|(_, name)| *name)
            .collect()
    }

    /// List which capabilities are granted.
    pub fn granted_capabilities(&self) -> Vec<&'static str> {
        let bits = [
            (caps::FILE_READ, "file_read"),
            (caps::FILE_WRITE, "file_write"),
            (caps::FILE_DELETE, "file_delete"),
            (caps::SHELL_EXEC, "shell_exec"),
            (caps::NETWORK, "network"),
            (caps::NETWORK_LISTEN, "network_listen"),
            (caps::ENV_READ, "env_read"),
            (caps::PROCESS_SPAWN, "process_spawn"),
            (caps::CLIPBOARD, "clipboard"),
            (caps::MEMORY_READ, "memory_read"),
            (caps::MEMORY_WRITE, "memory_write"),
            (caps::TOOL_INVOKE, "tool_invoke"),
            (caps::BROWSER, "browser"),
            (caps::CHANNEL_SEND, "channel_send"),
            (caps::CRON, "cron"),
            (caps::ADMIN, "admin"),
            (caps::DELEGATE, "delegate"),
            (caps::CRYPTO_SIGN, "crypto_sign"),
            (caps::WASM_EXEC, "wasm_exec"),
            (caps::MOUNT, "mount"),
        ];
        bits.iter()
            .filter(|(bit, _)| self.effective.has(*bit))
            .map(|(_, name)| *name)
            .collect()
    }
}

/// Permission grant cache — caches per skill version.
///
/// If a skill updates and its permission set changes, the grant is invalidated
/// and the permission dialog re-appears.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionGrantCache {
    entries: rustc_hash::FxHashMap<String, CachedGrant>,
}

/// A cached permission grant keyed by skill_id + version hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedGrant {
    /// Skill version hash at the time of grant
    pub version_hash: String,
    /// The granted capability set
    pub granted: CapabilitySet,
    /// When the grant was issued (epoch seconds)
    pub granted_at: u64,
}

impl PermissionGrantCache {
    pub fn new() -> Self {
        Self {
            entries: rustc_hash::FxHashMap::default(),
        }
    }

    /// Look up a cached grant for a skill.
    ///
    /// Returns `None` if no grant cached or version hash changed (skill updated).
    pub fn lookup(&self, skill_id: &str, current_version_hash: &str) -> Option<CapabilitySet> {
        self.entries.get(skill_id).and_then(|grant| {
            if grant.version_hash == current_version_hash {
                Some(grant.granted)
            } else {
                None // Skill updated — re-prompt
            }
        })
    }

    /// Store a permission grant.
    pub fn store(&mut self, skill_id: String, version_hash: String, granted: CapabilitySet) {
        self.entries.insert(skill_id, CachedGrant {
            version_hash,
            granted,
            granted_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        });
    }

    /// Revoke a grant (e.g., user manually removes permission).
    pub fn revoke(&mut self, skill_id: &str) {
        self.entries.remove(skill_id);
    }

    /// Revoke all grants.
    pub fn revoke_all(&mut self) {
        self.entries.clear();
    }
}

impl Default for PermissionGrantCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Pre-defined capability profiles for common skill types.
///
/// These safe-default profiles provide a starting point — skills can
/// request additional capabilities which will trigger a permission dialog.
pub mod profiles {
    use super::{CapabilitySet, caps};

    /// Read-only skill: can read files and memory, nothing else.
    pub const READ_ONLY: CapabilitySet = CapabilitySet(
        caps::FILE_READ | caps::MEMORY_READ
    );

    /// Standard skill: read/write files, memory, invoke tools.
    pub const STANDARD: CapabilitySet = CapabilitySet(
        caps::FILE_READ | caps::FILE_WRITE | caps::MEMORY_READ
        | caps::MEMORY_WRITE | caps::TOOL_INVOKE
    );

    /// Network skill: standard + network access.
    pub const NETWORK_ENABLED: CapabilitySet = CapabilitySet(
        caps::FILE_READ | caps::FILE_WRITE | caps::MEMORY_READ
        | caps::MEMORY_WRITE | caps::TOOL_INVOKE | caps::NETWORK
    );

    /// Shell skill: standard + shell execution + process spawn.
    pub const SHELL_ENABLED: CapabilitySet = CapabilitySet(
        caps::FILE_READ | caps::FILE_WRITE | caps::MEMORY_READ
        | caps::MEMORY_WRITE | caps::TOOL_INVOKE | caps::SHELL_EXEC
        | caps::PROCESS_SPAWN
    );

    /// Automation skill: network + shell + channel send + cron.
    pub const AUTOMATION: CapabilitySet = CapabilitySet(
        caps::FILE_READ | caps::FILE_WRITE | caps::MEMORY_READ
        | caps::MEMORY_WRITE | caps::TOOL_INVOKE | caps::NETWORK
        | caps::SHELL_EXEC | caps::PROCESS_SPAWN | caps::CHANNEL_SEND
        | caps::CRON
    );

    /// Browser skill: standard + network + browser.
    pub const BROWSER_ENABLED: CapabilitySet = CapabilitySet(
        caps::FILE_READ | caps::FILE_WRITE | caps::MEMORY_READ
        | caps::MEMORY_WRITE | caps::TOOL_INVOKE | caps::NETWORK
        | caps::BROWSER
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lattice_idempotency() {
        let a = CapabilitySet::from_bits(0b1010_1100);
        assert_eq!(a.meet(a), a);
        assert_eq!(a.join(a), a);
    }

    #[test]
    fn lattice_commutativity() {
        let a = CapabilitySet::from_bits(0xFF00);
        let b = CapabilitySet::from_bits(0x0FF0);
        assert_eq!(a.meet(b), b.meet(a));
        assert_eq!(a.join(b), b.join(a));
    }

    #[test]
    fn lattice_associativity() {
        let a = CapabilitySet::from_bits(0xFF);
        let b = CapabilitySet::from_bits(0xF0);
        let c = CapabilitySet::from_bits(0x0F);
        assert_eq!(a.meet(b).meet(c), a.meet(b.meet(c)));
        assert_eq!(a.join(b).join(c), a.join(b.join(c)));
    }

    #[test]
    fn lattice_absorption() {
        let a = CapabilitySet::from_bits(0xAB);
        let b = CapabilitySet::from_bits(0xCD);
        assert_eq!(a.meet(a.join(b)), a);
        assert_eq!(a.join(a.meet(b)), a);
    }

    #[test]
    fn permission_check_permits() {
        let granted = CapabilitySet::from_bits(caps::FILE_READ | caps::FILE_WRITE | caps::NETWORK);
        let required = CapabilitySet::from_bits(caps::FILE_READ | caps::FILE_WRITE);
        assert!(CapabilitySet::permits(granted, required));
    }

    #[test]
    fn permission_check_denies_missing() {
        let granted = CapabilitySet::from_bits(caps::FILE_READ);
        let required = CapabilitySet::from_bits(caps::FILE_READ | caps::NETWORK);
        assert!(!CapabilitySet::permits(granted, required));
    }

    #[test]
    fn delegation_restricts_child() {
        let parent = CapabilitySet::from_bits(caps::FILE_READ | caps::NETWORK | caps::SHELL_EXEC);
        let child_wants = CapabilitySet::from_bits(caps::FILE_READ | caps::ADMIN);
        let effective = CapabilitySet::delegate(parent, child_wants);
        // Child gets FILE_READ (in both) but NOT ADMIN (not in parent).
        assert!(effective.has(caps::FILE_READ));
        assert!(!effective.has(caps::ADMIN));
        assert!(!effective.has(caps::NETWORK)); // child didn't request it
    }

    #[test]
    fn gate_check_integration() {
        let mut tool_map = ToolCapabilityMap::new(CapabilitySet::from_bits(caps::TOOL_INVOKE));
        tool_map.register("web_search", CapabilitySet::from_bits(caps::NETWORK | caps::TOOL_INVOKE));
        tool_map.register("file_read", CapabilitySet::from_bits(caps::FILE_READ));
        let gate = CapabilityGate::new(tool_map);

        let agent = CapabilitySet::from_bits(caps::FILE_READ | caps::TOOL_INVOKE);
        assert!(gate.check(agent, "file_read").is_permit());
        assert!(!gate.check(agent, "web_search").is_permit()); // missing NETWORK
    }

    #[test]
    fn display_format() {
        let set = CapabilitySet::from_bits(caps::FILE_READ | caps::NETWORK);
        let s = format!("{}", set);
        assert!(s.contains("file_read"));
        assert!(s.contains("network"));
    }

    #[test]
    fn empty_and_full() {
        assert!(CapabilitySet::EMPTY.is_empty());
        assert_eq!(CapabilitySet::EMPTY.count(), 0);
        assert_eq!(CapabilitySet::FULL.count(), 128);
        assert!(CapabilitySet::EMPTY.is_subset_of(CapabilitySet::FULL));
        assert!(!CapabilitySet::FULL.is_subset_of(CapabilitySet::EMPTY));
    }
}
