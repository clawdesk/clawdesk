//! Canonical isolation level for sandbox enforcement.
//!
//! Single definition used across security policy evaluation and runtime
//! enforcement. The `Ord` derivation encodes the lattice:
//! `None < PathScope < ProcessIsolation < FullSandbox`.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Isolation level for sandboxed execution.
///
/// The ordering encodes the security lattice — higher variants are more
/// restrictive. Use `max(policy_required, runtime_available)` to compute
/// the effective isolation level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum IsolationLevel {
    /// No isolation — full host access.
    None = 0,
    /// Path-scoped: agent can only access specified directories.
    PathScope = 1,
    /// Process isolation via subprocess sandboxing.
    ProcessIsolation = 2,
    /// Full sandbox (container or WASM).
    FullSandbox = 3,
}

impl IsolationLevel {
    /// Check if this level satisfies a required minimum.
    ///
    /// `self.satisfies(required)` is true when `self >= required`.
    pub fn satisfies(&self, required: IsolationLevel) -> bool {
        *self >= required
    }
}

impl fmt::Display for IsolationLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => write!(f, "none"),
            Self::PathScope => write!(f, "path_scope"),
            Self::ProcessIsolation => write!(f, "process_isolation"),
            Self::FullSandbox => write!(f, "full_sandbox"),
        }
    }
}

impl Default for IsolationLevel {
    fn default() -> Self {
        Self::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_encodes_lattice() {
        assert!(IsolationLevel::None < IsolationLevel::PathScope);
        assert!(IsolationLevel::PathScope < IsolationLevel::ProcessIsolation);
        assert!(IsolationLevel::ProcessIsolation < IsolationLevel::FullSandbox);
    }

    #[test]
    fn satisfies_check() {
        assert!(IsolationLevel::FullSandbox.satisfies(IsolationLevel::PathScope));
        assert!(!IsolationLevel::None.satisfies(IsolationLevel::PathScope));
        assert!(IsolationLevel::PathScope.satisfies(IsolationLevel::PathScope));
    }
}
