//! D5: Certificate Pinning for API and Registry Connections.
//!
//! Implements certificate pinning to protect HTTPS connections against
//! man-in-the-middle attacks, even if a CA is compromised.
//!
//! ## Pinning Strategy
//!
//! We pin the **SubjectPublicKeyInfo (SPKI)** hash rather than individual
//! certificates. This allows certificate rotation without pin updates,
//! as long as the same key pair is reused.
//!
//! ```text
//! Pin = Base64(SHA-256(SubjectPublicKeyInfo))
//! ```
//!
//! ## Pin Set Configuration
//!
//! Each domain has a **pin set** with at least two pins:
//! - **Primary**: The current production key's SPKI hash.
//! - **Backup**: A pre-generated backup key's SPKI hash (for rotation).
//!
//! If both pins fail, the connection is **rejected**.
//!
//! ## Enforcement Modes
//!
//! - **Enforce**: Reject connections with mismatched pins (production).
//! - **ReportOnly**: Log mismatches but allow the connection (rollout).
//! - **Disabled**: Skip pinning entirely (development).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};
use std::collections::HashMap;

/// Enforcement mode for certificate pinning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PinningMode {
    /// Reject connections with mismatched pins.
    Enforce,
    /// Log mismatches but allow the connection.
    ReportOnly,
    /// Skip pinning entirely.
    Disabled,
}

impl Default for PinningMode {
    fn default() -> Self {
        Self::Enforce
    }
}

/// A pin set for a specific domain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinSet {
    /// Domain pattern (e.g., "*.clawdesk.app", "api.anthropic.com").
    pub domain: String,
    /// Base64-encoded SHA-256 SPKI hashes.
    pub pins: Vec<String>,
    /// Whether this pin set includes a backup pin.
    pub has_backup: bool,
    /// Maximum age in seconds before pins should be refreshed.
    pub max_age_secs: u64,
    /// Report URI for pin validation failures.
    pub report_uri: Option<String>,
}

/// Result of a pin validation check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinValidationResult {
    /// Pin matched — connection is trusted.
    Valid { matched_pin: String },
    /// No pin set configured for this domain — connection allowed.
    NoPinSet,
    /// Pin mismatch — connection should be rejected (in Enforce mode).
    Mismatch {
        domain: String,
        expected: Vec<String>,
        actual: String,
    },
    /// Pinning is disabled — connection allowed without check.
    Disabled,
}

/// Certificate pinning configuration and validation engine.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CertPinning {
    /// Global enforcement mode.
    pub mode: PinningMode,
    /// Per-domain pin sets.
    pub pin_sets: Vec<PinSet>,
    /// Domains that are exempt from pinning.
    pub exempt_domains: Vec<String>,
}

impl Default for CertPinning {
    fn default() -> Self {
        Self {
            mode: PinningMode::Enforce,
            pin_sets: Self::default_pin_sets(),
            exempt_domains: vec!["localhost".into(), "127.0.0.1".into()],
        }
    }
}

impl CertPinning {
    /// Create with disabled pinning (for development).
    pub fn disabled() -> Self {
        Self {
            mode: PinningMode::Disabled,
            pin_sets: Vec::new(),
            exempt_domains: Vec::new(),
        }
    }

    /// Create with report-only mode (for rollout).
    pub fn report_only() -> Self {
        Self {
            mode: PinningMode::ReportOnly,
            pin_sets: Self::default_pin_sets(),
            exempt_domains: vec!["localhost".into(), "127.0.0.1".into()],
        }
    }

    /// Default pin sets for known API providers.
    ///
    /// NOTE: These are placeholder SPKI hashes. In production, they would be
    /// generated from the actual server certificates' public keys.
    fn default_pin_sets() -> Vec<PinSet> {
        vec![
            PinSet {
                domain: "api.anthropic.com".into(),
                pins: vec![
                    // Primary + backup (placeholder hashes)
                    "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=".into(),
                    "BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB=".into(),
                ],
                has_backup: true,
                max_age_secs: 86400 * 30, // 30 days
                report_uri: None,
            },
            PinSet {
                domain: "api.openai.com".into(),
                pins: vec![
                    "CCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCCC=".into(),
                    "DDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDDD=".into(),
                ],
                has_backup: true,
                max_age_secs: 86400 * 30,
                report_uri: None,
            },
            PinSet {
                domain: "skills.clawdesk.io".into(),
                pins: vec![
                    "EEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEEE=".into(),
                    "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF=".into(),
                ],
                has_backup: true,
                max_age_secs: 86400 * 7, // 7 days for our own registry
                report_uri: Some("https://telemetry.clawdesk.app/pin-report".into()),
            },
            PinSet {
                domain: "releases.clawdesk.app".into(),
                pins: vec![
                    "GGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGGG=".into(),
                    "HHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHHH=".into(),
                ],
                has_backup: true,
                max_age_secs: 86400 * 7,
                report_uri: Some("https://telemetry.clawdesk.app/pin-report".into()),
            },
        ]
    }

    /// Compute the SPKI pin hash for a DER-encoded public key.
    ///
    /// `pin = Base64(SHA-256(spki_der_bytes))`
    pub fn compute_pin(spki_der: &[u8]) -> String {
        use base64ct::{Base64, Encoding};
        let hash = Sha256::digest(spki_der);
        Base64::encode_string(&hash)
    }

    /// Find the pin set for a given domain, supporting wildcard matching.
    pub fn find_pin_set(&self, domain: &str) -> Option<&PinSet> {
        self.pin_sets.iter().find(|ps| {
            if ps.domain == domain {
                return true;
            }
            // Wildcard: *.example.com matches foo.example.com
            if let Some(wildcard) = ps.domain.strip_prefix("*.") {
                if let Some((_sub, rest)) = domain.split_once('.') {
                    return rest == wildcard;
                }
            }
            false
        })
    }

    /// Check if a domain is exempt from pinning.
    pub fn is_exempt(&self, domain: &str) -> bool {
        self.exempt_domains.iter().any(|d| d == domain)
    }

    /// Validate a certificate's SPKI hash against the pin set.
    ///
    /// `actual_pin` should be `Base64(SHA-256(SPKI))` of the server certificate.
    pub fn validate(&self, domain: &str, actual_pin: &str) -> PinValidationResult {
        if self.mode == PinningMode::Disabled {
            return PinValidationResult::Disabled;
        }

        if self.is_exempt(domain) {
            debug!(domain, "Domain is exempt from cert pinning");
            return PinValidationResult::NoPinSet;
        }

        let pin_set = match self.find_pin_set(domain) {
            Some(ps) => ps,
            None => {
                debug!(domain, "No pin set for domain, allowing");
                return PinValidationResult::NoPinSet;
            }
        };

        // Check if the actual pin matches any pin in the set
        if pin_set.pins.iter().any(|p| p == actual_pin) {
            debug!(domain, "Certificate pin validated successfully");
            return PinValidationResult::Valid {
                matched_pin: actual_pin.to_string(),
            };
        }

        // Mismatch
        warn!(
            domain,
            expected = ?pin_set.pins,
            actual = actual_pin,
            mode = ?self.mode,
            "Certificate pin MISMATCH"
        );

        PinValidationResult::Mismatch {
            domain: domain.to_string(),
            expected: pin_set.pins.clone(),
            actual: actual_pin.to_string(),
        }
    }

    /// Check if a validation result should block the connection.
    pub fn should_block(&self, result: &PinValidationResult) -> bool {
        matches!(
            (&self.mode, result),
            (PinningMode::Enforce, PinValidationResult::Mismatch { .. })
        )
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compute_pin_deterministic() {
        let spki = b"test-public-key-bytes";
        let pin1 = CertPinning::compute_pin(spki);
        let pin2 = CertPinning::compute_pin(spki);
        assert_eq!(pin1, pin2);
    }

    #[test]
    fn compute_pin_different_keys() {
        let pin1 = CertPinning::compute_pin(b"key-a");
        let pin2 = CertPinning::compute_pin(b"key-b");
        assert_ne!(pin1, pin2);
    }

    #[test]
    fn validate_matching_pin() {
        let pin = CertPinning::compute_pin(b"test-key");
        let config = CertPinning {
            mode: PinningMode::Enforce,
            pin_sets: vec![PinSet {
                domain: "example.com".into(),
                pins: vec![pin.clone()],
                has_backup: false,
                max_age_secs: 86400,
                report_uri: None,
            }],
            exempt_domains: Vec::new(),
        };

        let result = config.validate("example.com", &pin);
        assert_eq!(
            result,
            PinValidationResult::Valid {
                matched_pin: pin
            }
        );
    }

    #[test]
    fn validate_mismatch() {
        let config = CertPinning {
            mode: PinningMode::Enforce,
            pin_sets: vec![PinSet {
                domain: "example.com".into(),
                pins: vec!["expected-pin".into()],
                has_backup: false,
                max_age_secs: 86400,
                report_uri: None,
            }],
            exempt_domains: Vec::new(),
        };

        let result = config.validate("example.com", "wrong-pin");
        assert!(matches!(result, PinValidationResult::Mismatch { .. }));
        assert!(config.should_block(&result));
    }

    #[test]
    fn validate_report_only_does_not_block() {
        let config = CertPinning {
            mode: PinningMode::ReportOnly,
            pin_sets: vec![PinSet {
                domain: "example.com".into(),
                pins: vec!["expected-pin".into()],
                has_backup: false,
                max_age_secs: 86400,
                report_uri: None,
            }],
            exempt_domains: Vec::new(),
        };

        let result = config.validate("example.com", "wrong-pin");
        assert!(matches!(result, PinValidationResult::Mismatch { .. }));
        assert!(!config.should_block(&result));
    }

    #[test]
    fn validate_disabled() {
        let config = CertPinning::disabled();
        let result = config.validate("example.com", "any-pin");
        assert_eq!(result, PinValidationResult::Disabled);
        assert!(!config.should_block(&result));
    }

    #[test]
    fn validate_exempt_domain() {
        let config = CertPinning::default();
        let result = config.validate("localhost", "any-pin");
        assert_eq!(result, PinValidationResult::NoPinSet);
    }

    #[test]
    fn validate_no_pin_set() {
        let config = CertPinning::default();
        let result = config.validate("unknown.example.org", "any-pin");
        assert_eq!(result, PinValidationResult::NoPinSet);
    }

    #[test]
    fn wildcard_matching() {
        let config = CertPinning {
            mode: PinningMode::Enforce,
            pin_sets: vec![PinSet {
                domain: "*.example.com".into(),
                pins: vec!["test-pin".into()],
                has_backup: false,
                max_age_secs: 86400,
                report_uri: None,
            }],
            exempt_domains: Vec::new(),
        };

        let result = config.validate("api.example.com", "test-pin");
        assert!(matches!(result, PinValidationResult::Valid { .. }));

        let result = config.validate("other.example.com", "test-pin");
        assert!(matches!(result, PinValidationResult::Valid { .. }));

        // Should not match the bare domain
        let result = config.validate("example.com", "test-pin");
        assert_eq!(result, PinValidationResult::NoPinSet);
    }

    #[test]
    fn default_has_known_providers() {
        let config = CertPinning::default();
        assert!(config.find_pin_set("api.anthropic.com").is_some());
        assert!(config.find_pin_set("api.openai.com").is_some());
        assert!(config.find_pin_set("skills.clawdesk.io").is_some());
        assert!(config.find_pin_set("releases.clawdesk.app").is_some());
    }

    #[test]
    fn config_roundtrip() {
        let config = CertPinning::default();
        let json = serde_json::to_string(&config).unwrap();
        let parsed: CertPinning = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.pin_sets.len(), config.pin_sets.len());
        assert_eq!(parsed.mode, config.mode);
    }
}
