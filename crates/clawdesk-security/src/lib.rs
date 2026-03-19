//! # clawdesk-security
//!
//! Security system — audit logging, content scanning, and access control.
//!
//! ## Architecture
//! - **AuditLogger**: Hash-chained tamper-evident audit log
//! - **CascadeScanner**: Multi-tier content scanning (regex → AST → semantic)
//! - **AclManager**: Resource-level access control with principals and permissions
//! - **Allowlist**: Per-channel and global allowlists for message routing

pub mod acl;
pub mod audit;
pub mod allowlist;
pub mod auth_profiles;
pub mod capabilities;
pub mod cert_pinning;
pub mod command_policy;
pub mod credential_vault;
pub mod crypto;
pub mod dm_pairing;
pub mod exec_approval;
pub mod group_policy;
pub mod identity;
pub mod injection;
pub mod keychain;
pub mod oauth;
pub mod rotation;
pub mod sandbox_policy;
pub mod safe_regex;
pub mod scanner;
pub mod secret_ref;
pub mod skill_verify;
pub mod tokens;
pub mod obfuscation;
pub mod url_sanitize;

pub use acl::AclManager;
pub use audit::AuditLogger;
pub use allowlist::AllowlistManager;
pub use capabilities::{CapabilityDecision, CapabilityGuard, CapabilityKind, CapabilityPolicy, PolicySet};
pub use cert_pinning::{CertPinning, PinningMode, PinValidationResult};
pub use command_policy::{CommandDecision, CommandPolicyConfig, CommandPolicyEngine, PolicyRule, RiskLevel};
pub use dm_pairing::DmPairingManager;
pub use exec_approval::{ApprovalError, ApprovalRequest, ApprovalStatus, ExecApprovalManager};
pub use group_policy::GroupPolicyManager;
pub use identity::IdentityContract;
pub use injection::{InjectionScanner, InjectionScannerConfig, InputSource, ScanAction, ScanResult};
pub use keychain::KeychainProvider;
pub use scanner::CascadeScanner;
pub use secret_ref::{SecretRef, SecretDetector, resolve_or_passthrough, resolve_or_fallback, migrate_env_to_vault};
pub use skill_verify::SkillVerifier;
pub use oauth::{OAuthFlowManager, AuthProfile, AuthProfileManager, TokenSet, OAuthClientConfig};
pub use rotation::{ManagedSecret, RotationPolicy, RotationRecord, SecretRotationManager, SecretState};
pub use safe_regex::{SafeRegexResult, RejectReason, safe_compile, safe_compile_case_insensitive};
pub use tokens::{AuthError, ScopedToken, ServerSecret, TokenScope};
pub use url_sanitize::{strip_url_userinfo, sanitize_embedded_urls};
