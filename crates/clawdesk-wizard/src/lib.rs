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
pub mod setup_surface;
pub mod steps;
pub mod validation;

pub use flow::{WizardFlow, WizardStep, WizardState, StepResult};
pub use pairing::{PairingChallenge, SetupCode, PairingStore};
pub use setup_surface::{
    ChannelSetupSurface, CredentialStep, EnvShortcut, SetupNote, SetupState,
    SetupStatus, SetupSurfaceRegistry, TextInputStep,
};
pub use validation::{ConfigValidator, ValidationResult};
