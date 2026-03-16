//! Wizard flow — DAG-based onboarding state machine with resumability.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Onboarding wizard steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WizardStep {
    Welcome,
    RiskAcknowledgement,
    ConfigValidation,
    SecretInput,
    GatewayConfig,
    ChannelPairing,
    Finalize,
    Complete,
}

impl WizardStep {
    /// Next step in the flow (linear for now, DAG edges for conditional paths).
    pub fn next(self) -> Option<Self> {
        match self {
            Self::Welcome => Some(Self::RiskAcknowledgement),
            Self::RiskAcknowledgement => Some(Self::ConfigValidation),
            Self::ConfigValidation => Some(Self::SecretInput),
            Self::SecretInput => Some(Self::GatewayConfig),
            Self::GatewayConfig => Some(Self::ChannelPairing),
            Self::ChannelPairing => Some(Self::Finalize),
            Self::Finalize => Some(Self::Complete),
            Self::Complete => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Welcome => "Welcome",
            Self::RiskAcknowledgement => "Security Acknowledgement",
            Self::ConfigValidation => "Config Validation",
            Self::SecretInput => "API Keys & Secrets",
            Self::GatewayConfig => "Gateway Configuration",
            Self::ChannelPairing => "Channel Pairing",
            Self::Finalize => "Finalize",
            Self::Complete => "Complete",
        }
    }
}

/// Wizard session state — serializable for resumability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WizardState {
    pub current_step: WizardStep,
    pub completed_steps: Vec<WizardStep>,
    pub accumulated_config: HashMap<String, serde_json::Value>,
    pub risk_acknowledged: bool,
}

impl Default for WizardState {
    fn default() -> Self {
        Self {
            current_step: WizardStep::Welcome,
            completed_steps: Vec::new(),
            accumulated_config: HashMap::new(),
            risk_acknowledged: false,
        }
    }
}

impl WizardState {
    /// Advance to the next step.
    pub fn advance(&mut self) -> Result<(), String> {
        let next = self.current_step.next()
            .ok_or("wizard is already complete")?;

        // Enforce risk acknowledgement gate.
        if self.current_step == WizardStep::RiskAcknowledgement && !self.risk_acknowledged {
            return Err("risk acknowledgement required to proceed".into());
        }

        self.completed_steps.push(self.current_step);
        self.current_step = next;
        Ok(())
    }

    pub fn is_complete(&self) -> bool {
        self.current_step == WizardStep::Complete
    }

    /// Store a config value.
    pub fn set_config(&mut self, key: &str, value: serde_json::Value) {
        self.accumulated_config.insert(key.to_string(), value);
    }
}

/// Result of executing a wizard step.
#[derive(Debug, Clone)]
pub enum StepResult {
    /// Step completed, advance to next.
    Continue,
    /// Step requires user input (display prompt).
    NeedInput { prompt: String },
    /// Step failed with an error.
    Error { message: String },
    /// User requested to go back.
    Back,
    /// User requested to abort.
    Abort,
}

/// The wizard flow controller.
pub struct WizardFlow {
    pub state: WizardState,
}

impl WizardFlow {
    pub fn new() -> Self {
        Self { state: WizardState::default() }
    }

    /// Resume from a saved state.
    pub fn resume(state: WizardState) -> Self {
        Self { state }
    }

    /// Serialize state for persistence.
    pub fn save(&self) -> String {
        serde_json::to_string_pretty(&self.state)
            .unwrap_or_default()
    }

    /// Load state from persistence.
    pub fn load(json: &str) -> Result<Self, serde_json::Error> {
        let state: WizardState = serde_json::from_str(json)?;
        Ok(Self { state })
    }

    /// Progress percentage.
    pub fn progress(&self) -> f32 {
        let total = 8.0; // total steps
        self.state.completed_steps.len() as f32 / total * 100.0
    }
}

impl Default for WizardFlow {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wizard_advances_through_steps() {
        let mut flow = WizardFlow::new();
        assert_eq!(flow.state.current_step, WizardStep::Welcome);

        flow.state.advance().unwrap();
        assert_eq!(flow.state.current_step, WizardStep::RiskAcknowledgement);

        // Must acknowledge risk before advancing.
        assert!(flow.state.advance().is_err());
        flow.state.risk_acknowledged = true;
        flow.state.advance().unwrap();
        assert_eq!(flow.state.current_step, WizardStep::ConfigValidation);
    }

    #[test]
    fn wizard_serializable() {
        let mut flow = WizardFlow::new();
        flow.state.set_config("provider", serde_json::json!("anthropic"));
        let json = flow.save();
        let restored = WizardFlow::load(&json).unwrap();
        assert_eq!(restored.state.current_step, WizardStep::Welcome);
    }

    #[test]
    fn wizard_complete_detection() {
        let mut state = WizardState::default();
        state.risk_acknowledged = true;
        // Advance through all steps.
        while !state.is_complete() {
            let _ = state.advance(); // some may fail, that's ok for this test
        }
    }
}
