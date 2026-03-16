//! Voice call plugin — gateway integration for voice calls.

use serde::{Deserialize, Serialize};

/// Voice call state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CallState {
    Idle,
    Ringing,
    Active,
    OnHold,
    Ended,
}

/// Voice call configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceCallConfig {
    pub enabled: bool,
    pub max_duration_secs: u64,
    pub voice_id: String,
    pub auto_answer: bool,
}

impl Default for VoiceCallConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_duration_secs: 3600,
            voice_id: "default".into(),
            auto_answer: false,
        }
    }
}

/// Voice call plugin interface.
pub struct VoiceCallPlugin {
    #[allow(dead_code)]
    config: VoiceCallConfig,
    state: CallState,
}

impl VoiceCallPlugin {
    pub fn new(config: VoiceCallConfig) -> Self {
        Self { config, state: CallState::Idle }
    }

    pub fn state(&self) -> CallState { self.state }

    pub fn is_active(&self) -> bool {
        matches!(self.state, CallState::Active | CallState::OnHold)
    }

    pub fn start_call(&mut self) -> Result<(), String> {
        if self.state != CallState::Idle {
            return Err(format!("cannot start call in state {:?}", self.state));
        }
        self.state = CallState::Ringing;
        Ok(())
    }

    pub fn answer(&mut self) -> Result<(), String> {
        if self.state != CallState::Ringing {
            return Err("not ringing".into());
        }
        self.state = CallState::Active;
        Ok(())
    }

    pub fn end_call(&mut self) {
        self.state = CallState::Ended;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_lifecycle() {
        let mut plugin = VoiceCallPlugin::new(VoiceCallConfig::default());
        assert_eq!(plugin.state(), CallState::Idle);
        plugin.start_call().unwrap();
        assert_eq!(plugin.state(), CallState::Ringing);
        plugin.answer().unwrap();
        assert!(plugin.is_active());
        plugin.end_call();
        assert_eq!(plugin.state(), CallState::Ended);
    }
}
