//! # clawdesk-wizard
//!
//! Security-first interactive onboarding wizard with cryptographic device pairing.
//!
//! ## Wizard DAG
//! ```text
//! Welcome → RiskAck → ConfigValidation → SecretInput →
//! GatewayConfig → ChannelPairing → Finalize → Complete
//! ```

pub mod flow;
pub mod pairing;
pub mod steps;
pub mod validation;

pub use flow::{WizardFlow, WizardStep, WizardState, StepResult};
pub use pairing::{PairingChallenge, SetupCode, PairingStore};
pub use validation::{ConfigValidator, ValidationResult};
