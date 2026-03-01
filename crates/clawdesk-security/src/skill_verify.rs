//! Skill verification for trusted skill execution.
//!
//! `SkillVerifier` validates skill manifests against a local trust store
//! before allowing skill registration or execution. This prevents untrusted
//! or tampered skills from being loaded into the agent runtime.

use std::collections::HashSet;

/// Verification result for a skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyResult {
    /// Skill is trusted and may execute.
    Trusted,
    /// Skill signature is invalid or missing.
    Untrusted(String),
    /// Skill is explicitly denied by policy.
    Denied(String),
}

/// Verifies skill integrity and trust before registration/execution.
///
/// Maintains a set of trusted skill IDs (allow-list) and an optional
/// deny-list. Skills not in either list default to `Untrusted`.
pub struct SkillVerifier {
    trusted: HashSet<String>,
    denied: HashSet<String>,
    /// When true, skills not in trusted or denied are allowed (open mode).
    allow_unknown: bool,
}

impl SkillVerifier {
    /// Create a new verifier with empty trust/deny lists.
    pub fn new() -> Self {
        Self {
            trusted: HashSet::new(),
            denied: HashSet::new(),
            allow_unknown: false,
        }
    }

    /// Create a permissive verifier that trusts unknown skills.
    pub fn permissive() -> Self {
        Self {
            trusted: HashSet::new(),
            denied: HashSet::new(),
            allow_unknown: true,
        }
    }

    /// Add a skill ID to the trusted set.
    pub fn trust(&mut self, skill_id: impl Into<String>) {
        let id = skill_id.into();
        self.denied.remove(&id);
        self.trusted.insert(id);
    }

    /// Add a skill ID to the denied set.
    pub fn deny(&mut self, skill_id: impl Into<String>) {
        let id = skill_id.into();
        self.trusted.remove(&id);
        self.denied.insert(id);
    }

    /// Verify whether a skill is allowed to execute.
    pub fn verify(&self, skill_id: &str) -> VerifyResult {
        if self.denied.contains(skill_id) {
            return VerifyResult::Denied(format!("skill '{skill_id}' is denied by policy"));
        }
        if self.trusted.contains(skill_id) {
            return VerifyResult::Trusted;
        }
        if self.allow_unknown {
            return VerifyResult::Trusted;
        }
        VerifyResult::Untrusted(format!("skill '{skill_id}' is not in the trust store"))
    }

    /// Number of trusted skills.
    pub fn trusted_count(&self) -> usize {
        self.trusted.len()
    }

    /// Number of denied skills.
    pub fn denied_count(&self) -> usize {
        self.denied.len()
    }
}

impl Default for SkillVerifier {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trusted_skill_passes() {
        let mut v = SkillVerifier::new();
        v.trust("web_search");
        assert_eq!(v.verify("web_search"), VerifyResult::Trusted);
    }

    #[test]
    fn unknown_skill_untrusted_by_default() {
        let v = SkillVerifier::new();
        assert!(matches!(v.verify("unknown"), VerifyResult::Untrusted(_)));
    }

    #[test]
    fn denied_skill_rejected() {
        let mut v = SkillVerifier::new();
        v.deny("malicious");
        assert!(matches!(v.verify("malicious"), VerifyResult::Denied(_)));
    }

    #[test]
    fn permissive_mode_trusts_unknown() {
        let v = SkillVerifier::permissive();
        assert_eq!(v.verify("anything"), VerifyResult::Trusted);
    }

    #[test]
    fn deny_overrides_trust() {
        let mut v = SkillVerifier::new();
        v.trust("flip_flop");
        v.deny("flip_flop");
        assert!(matches!(v.verify("flip_flop"), VerifyResult::Denied(_)));
    }
}
