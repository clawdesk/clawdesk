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
pub mod cert_pinning;
pub mod command_policy;
pub mod credential_vault;
pub mod crypto;
pub mod dm_pairing;
pub mod exec_approval;
pub mod group_policy;
pub mod identity;
pub mod oauth;
pub mod sandbox_policy;
pub mod scanner;
pub mod secret_ref;
pub mod tokens;

pub use acl::AclManager;
pub use audit::AuditLogger;
pub use allowlist::AllowlistManager;
pub use cert_pinning::{CertPinning, PinningMode, PinValidationResult};
pub use command_policy::{CommandDecision, CommandPolicyConfig, CommandPolicyEngine, PolicyRule, RiskLevel};
pub use dm_pairing::DmPairingManager;
pub use exec_approval::{ApprovalError, ApprovalRequest, ApprovalStatus, ExecApprovalManager};
pub use group_policy::GroupPolicyManager;
pub use identity::IdentityContract;
pub use scanner::CascadeScanner;
pub use secret_ref::{SecretRef, SecretDetector, resolve_or_passthrough, resolve_or_fallback, migrate_env_to_vault};
pub use oauth::{OAuthFlowManager, AuthProfile, AuthProfileManager, TokenSet, OAuthClientConfig};
pub use tokens::{AuthError, ScopedToken, ServerSecret, TokenScope};
