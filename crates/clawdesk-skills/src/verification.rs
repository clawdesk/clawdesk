//! Skill signature verification — Ed25519 cryptographic validation.
//!
//! Verifies that skill manifests were signed by a trusted publisher before
//! activation. Uses Ed25519 (RFC 8032) via `ed25519-dalek`.
//!
//! ## Trust hierarchy
//!
//! ```text
//! Builtin          → implicit trust (compiled into binary)
//! Signed(trusted)  → publisher key in trusted keyring
//! Signed(unknown)  → valid signature but unknown publisher
//! Unsigned         → no signature present
//! ```
//!
//! The gateway enforces a minimum trust level per deployment mode:
//! - Production: requires `Signed(trusted)` or `Builtin`
//! - Development: allows `Unsigned` with a warning

use crate::definition::{SkillManifest, SkillSource};
use ed25519_dalek::{Signature, VerifyingKey, Verifier};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use tracing::{debug, warn};

/// Trust level assigned after signature verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TrustLevel {
    /// No signature present.
    Unsigned = 0,
    /// Valid signature but publisher key not in trusted keyring.
    SignedUntrusted = 1,
    /// Valid signature and publisher key is trusted.
    SignedTrusted = 2,
    /// Compiled into the binary — implicit full trust.
    Builtin = 3,
}

impl std::fmt::Display for TrustLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TrustLevel::Unsigned => write!(f, "unsigned"),
            TrustLevel::SignedUntrusted => write!(f, "signed(untrusted)"),
            TrustLevel::SignedTrusted => write!(f, "signed(trusted)"),
            TrustLevel::Builtin => write!(f, "builtin"),
        }
    }
}

/// Result of skill verification.
#[derive(Debug, Clone)]
pub struct VerificationResult {
    pub trust_level: TrustLevel,
    pub publisher_key: Option<String>,
    pub error: Option<String>,
}

/// Verifier configuration.
pub struct SkillVerifier {
    /// Set of hex-encoded Ed25519 public keys trusted by this deployment.
    trusted_keys: HashSet<String>,
    /// Minimum trust level required to load a skill.
    min_trust: TrustLevel,
}

impl SkillVerifier {
    /// Create a new verifier with trusted keys and minimum trust level.
    pub fn new(trusted_keys: HashSet<String>, min_trust: TrustLevel) -> Self {
        Self {
            trusted_keys,
            min_trust,
        }
    }

    /// Create a permissive verifier for development (allows unsigned).
    pub fn development() -> Self {
        Self {
            trusted_keys: HashSet::new(),
            min_trust: TrustLevel::Unsigned,
        }
    }

    /// Verify a skill manifest's signature.
    ///
    /// Returns the trust level and whether the skill should be allowed to load.
    pub fn verify(&self, manifest: &SkillManifest, source: &SkillSource) -> VerificationResult {
        // Builtin skills are always trusted.
        if *source == SkillSource::Builtin {
            return VerificationResult {
                trust_level: TrustLevel::Builtin,
                publisher_key: None,
                error: None,
            };
        }

        // Check if signature and publisher key are present.
        let (sig_hex, pub_key_hex) = match (&manifest.signature, &manifest.publisher_key) {
            (Some(s), Some(k)) => (s.clone(), k.clone()),
            (None, _) | (_, None) => {
                debug!(skill = %manifest.id, "no signature present");
                return VerificationResult {
                    trust_level: TrustLevel::Unsigned,
                    publisher_key: None,
                    error: None,
                };
            }
        };

        // Decode the public key (32 bytes, hex-encoded = 64 chars).
        let pub_key_bytes = match hex::decode(&pub_key_hex) {
            Ok(b) if b.len() == 32 => b,
            Ok(b) => {
                return VerificationResult {
                    trust_level: TrustLevel::Unsigned,
                    publisher_key: Some(pub_key_hex),
                    error: Some(format!(
                        "invalid public key length: expected 32 bytes, got {}",
                        b.len()
                    )),
                };
            }
            Err(e) => {
                return VerificationResult {
                    trust_level: TrustLevel::Unsigned,
                    publisher_key: Some(pub_key_hex),
                    error: Some(format!("invalid public key hex: {e}")),
                };
            }
        };

        // Decode the signature (64 bytes, hex-encoded = 128 chars).
        let sig_bytes = match hex::decode(&sig_hex) {
            Ok(b) if b.len() == 64 => b,
            Ok(b) => {
                return VerificationResult {
                    trust_level: TrustLevel::Unsigned,
                    publisher_key: Some(pub_key_hex),
                    error: Some(format!(
                        "invalid signature length: expected 64 bytes, got {}",
                        b.len()
                    )),
                };
            }
            Err(e) => {
                return VerificationResult {
                    trust_level: TrustLevel::Unsigned,
                    publisher_key: Some(pub_key_hex),
                    error: Some(format!("invalid signature hex: {e}")),
                };
            }
        };

        // Construct verifying key and signature.
        let verifying_key = match VerifyingKey::from_bytes(
            pub_key_bytes.as_slice().try_into().unwrap(),
        ) {
            Ok(k) => k,
            Err(e) => {
                return VerificationResult {
                    trust_level: TrustLevel::Unsigned,
                    publisher_key: Some(pub_key_hex),
                    error: Some(format!("invalid Ed25519 public key: {e}")),
                };
            }
        };

        let sig_array: &[u8; 64] = match sig_bytes.as_slice().try_into() {
            Ok(a) => a,
            Err(_) => {
                return VerificationResult {
                    trust_level: TrustLevel::Unsigned,
                    publisher_key: Some(pub_key_hex),
                    error: Some("invalid Ed25519 signature length".into()),
                };
            }
        };
        let signature = Signature::from_bytes(sig_array);

        // Build canonical signing payload (manifest fields excluding signature).
        let canonical = canonical_manifest_bytes(manifest);

        // Verify the signature.
        if verifying_key.verify(&canonical, &signature).is_err() {
            warn!(skill = %manifest.id, "signature verification FAILED");
            return VerificationResult {
                trust_level: TrustLevel::Unsigned,
                publisher_key: Some(pub_key_hex),
                error: Some("Ed25519 signature verification failed".to_string()),
            };
        }

        // Signature is valid — check if publisher is trusted.
        let trusted = self.trusted_keys.contains(&pub_key_hex);
        let trust_level = if trusted {
            TrustLevel::SignedTrusted
        } else {
            TrustLevel::SignedUntrusted
        };

        debug!(
            skill = %manifest.id,
            trust = %trust_level,
            publisher = %pub_key_hex,
            "signature verified"
        );

        VerificationResult {
            trust_level,
            publisher_key: Some(pub_key_hex),
            error: None,
        }
    }

    /// Check if a verification result meets the minimum trust requirement.
    pub fn meets_minimum(&self, result: &VerificationResult) -> bool {
        result.trust_level >= self.min_trust
    }

    /// Verify and check in one step. Returns error message if rejected.
    pub fn verify_and_gate(
        &self,
        manifest: &SkillManifest,
        source: &SkillSource,
    ) -> Result<VerificationResult, String> {
        let result = self.verify(manifest, source);
        if !self.meets_minimum(&result) {
            return Err(format!(
                "skill '{}' has trust level {} but minimum {} required",
                manifest.id, result.trust_level, self.min_trust,
            ));
        }
        if let Some(ref err) = result.error {
            warn!(skill = %manifest.id, error = %err, "verification warning");
        }
        Ok(result)
    }
}

/// Build canonical bytes for signing/verification.
///
/// Deterministic serialization: fields in fixed order, no signature field.
/// Uses the format: `{id}:{version}:{author}:{description}:{hash_of_tools}`
fn canonical_manifest_bytes(manifest: &SkillManifest) -> Vec<u8> {
    let author = manifest.author.as_deref().unwrap_or("");
    let tools = manifest.required_tools.join(",");
    let deps: Vec<String> = manifest.dependencies.iter().map(|d| d.0.clone()).collect();
    let dep_str = deps.join(",");

    let canonical = format!(
        "clawdesk-skill-v1:{}:{}:{}:{}:{}:{}",
        manifest.id, manifest.version, author, manifest.description, tools, dep_str,
    );

    canonical.into_bytes()
}

impl Default for SkillVerifier {
    fn default() -> Self {
        Self::development()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// A3: Signing API — for skill publishers
// ═══════════════════════════════════════════════════════════════════════════

/// Sign a skill manifest with an Ed25519 signing key.
///
/// Mutates the manifest in-place to set `signature` and `publisher_key`.
///
/// ## Usage
///
/// ```ignore
/// let signing_key = SigningKey::from_bytes(&key_bytes);
/// sign_manifest(&mut manifest, &signing_key);
/// // manifest.signature and manifest.publisher_key are now set
/// ```
pub fn sign_manifest(
    manifest: &mut SkillManifest,
    signing_key: &ed25519_dalek::SigningKey,
) {
    use ed25519_dalek::Signer;

    let verifying_key = signing_key.verifying_key();
    let pub_hex = hex::encode(verifying_key.as_bytes());

    // Set publisher key before computing canonical bytes
    // (canonical bytes don't include signature, but may include publisher key for binding)
    manifest.publisher_key = Some(pub_hex);

    let canonical = canonical_manifest_bytes(manifest);
    let sig = signing_key.sign(&canonical);
    manifest.signature = Some(hex::encode(sig.to_bytes()));
}

/// Generate a deterministic Ed25519 keypair for testing.
///
/// Returns (signing_key_hex, public_key_hex).
/// Uses a fixed seed — NOT for production use.
#[cfg(test)]
pub fn generate_publisher_keypair() -> (String, String) {
    use ed25519_dalek::SigningKey;
    use sha2::{Digest, Sha256};

    // Derive a deterministic 32-byte seed from a test string
    let seed = Sha256::digest(b"clawdesk-test-publisher-keypair-seed");
    let seed_bytes: [u8; 32] = seed.into();
    let signing_key = SigningKey::from_bytes(&seed_bytes);
    let verifying_key = signing_key.verifying_key();

    (
        hex::encode(signing_key.to_bytes()),
        hex::encode(verifying_key.as_bytes()),
    )
}

/// Verify a content hash matches the expected value.
///
/// Used for integrity verification of downloaded skill packages.
pub fn verify_content_hash(content: &[u8], expected_hash: &str) -> bool {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(content);
    let actual = hex::encode(hasher.finalize());
    actual == expected_hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::{SkillId, SkillManifest, SkillSource, SkillTrigger};
    use ed25519_dalek::{Signer, SigningKey};

    fn test_manifest() -> SkillManifest {
        SkillManifest {
            id: SkillId::from("test/skill"),
            display_name: "Test Skill".into(),
            description: "A test skill".into(),
            version: "1.0.0".into(),
            author: Some("tester".into()),
            dependencies: vec![],
            required_tools: vec!["search".into()],
            parameters: vec![],
            triggers: vec![SkillTrigger::Always],
            estimated_tokens: 100,
            priority_weight: 1.0,
            tags: vec![],
            signature: None,
            publisher_key: None,
            content_hash: None,
            schema_version: 1,
        }
    }

    #[test]
    fn builtin_skills_are_always_trusted() {
        let verifier = SkillVerifier::new(HashSet::new(), TrustLevel::SignedTrusted);
        let manifest = test_manifest();
        let result = verifier.verify(&manifest, &SkillSource::Builtin);
        assert_eq!(result.trust_level, TrustLevel::Builtin);
        assert!(verifier.meets_minimum(&result));
    }

    #[test]
    fn unsigned_skill_gets_unsigned_level() {
        let verifier = SkillVerifier::development();
        let manifest = test_manifest();
        let source = SkillSource::Local {
            path: "/tmp/test".into(),
        };
        let result = verifier.verify(&manifest, &source);
        assert_eq!(result.trust_level, TrustLevel::Unsigned);
    }

    #[test]
    fn signed_skill_with_valid_signature() {
        // Generate a keypair.
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let verifying_key = signing_key.verifying_key();
        let pub_hex = hex::encode(verifying_key.as_bytes());

        // Create manifest and sign it.
        let mut manifest = test_manifest();
        manifest.publisher_key = Some(pub_hex.clone());

        let canonical = canonical_manifest_bytes(&manifest);
        let sig = signing_key.sign(&canonical);
        manifest.signature = Some(hex::encode(sig.to_bytes()));

        // Verify with trusted key.
        let mut trusted = HashSet::new();
        trusted.insert(pub_hex);
        let verifier = SkillVerifier::new(trusted, TrustLevel::Unsigned);
        let result = verifier.verify(
            &manifest,
            &SkillSource::Local {
                path: "/tmp".into(),
            },
        );
        assert_eq!(result.trust_level, TrustLevel::SignedTrusted);
        assert!(result.error.is_none());
    }

    #[test]
    fn signed_skill_untrusted_publisher() {
        let signing_key = SigningKey::from_bytes(&[99u8; 32]);
        let verifying_key = signing_key.verifying_key();
        let pub_hex = hex::encode(verifying_key.as_bytes());

        let mut manifest = test_manifest();
        manifest.publisher_key = Some(pub_hex);

        let canonical = canonical_manifest_bytes(&manifest);
        let sig = signing_key.sign(&canonical);
        manifest.signature = Some(hex::encode(sig.to_bytes()));

        // No trusted keys → SignedUntrusted.
        let verifier = SkillVerifier::new(HashSet::new(), TrustLevel::Unsigned);
        let result = verifier.verify(
            &manifest,
            &SkillSource::Local {
                path: "/tmp".into(),
            },
        );
        assert_eq!(result.trust_level, TrustLevel::SignedUntrusted);
    }

    #[test]
    fn tampered_manifest_fails_verification() {
        let signing_key = SigningKey::from_bytes(&[42u8; 32]);
        let verifying_key = signing_key.verifying_key();
        let pub_hex = hex::encode(verifying_key.as_bytes());

        let mut manifest = test_manifest();
        manifest.publisher_key = Some(pub_hex.clone());

        let canonical = canonical_manifest_bytes(&manifest);
        let sig = signing_key.sign(&canonical);
        manifest.signature = Some(hex::encode(sig.to_bytes()));

        // Tamper with the manifest.
        manifest.description = "Tampered description".into();

        let mut trusted = HashSet::new();
        trusted.insert(pub_hex);
        let verifier = SkillVerifier::new(trusted, TrustLevel::Unsigned);
        let result = verifier.verify(
            &manifest,
            &SkillSource::Local {
                path: "/tmp".into(),
            },
        );
        assert_eq!(result.trust_level, TrustLevel::Unsigned);
        assert!(result.error.is_some());
    }

    #[test]
    fn minimum_trust_gate() {
        let verifier = SkillVerifier::new(HashSet::new(), TrustLevel::SignedTrusted);
        let manifest = test_manifest();
        let source = SkillSource::Local {
            path: "/tmp".into(),
        };
        let err = verifier.verify_and_gate(&manifest, &source).unwrap_err();
        assert!(err.contains("minimum"));
    }
}
