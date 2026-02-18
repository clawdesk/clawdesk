//! Voice wake detection and forwarding.
//!
//! Listens for voice wake events from the OS audio subsystem (macOS dictation,
//! Android speech recognition, etc.) and forwards transcribed text to an agent.

use serde::{Deserialize, Serialize};
use tracing::{debug, info};

/// Voice wake configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceWakeConfig {
    /// Whether voice wake is enabled.
    pub enabled: bool,
    /// Wake phrase(s) that trigger listening.
    pub wake_phrases: Vec<String>,
    /// Agent to forward transcribed text to.
    pub target_agent: String,
    /// Command template (use `${text}` for transcribed text).
    pub command_template: String,
    /// Whether to play a confirmation sound.
    pub play_confirmation: bool,
    /// Silence timeout in seconds before finalizing input.
    pub silence_timeout_secs: u32,
}

impl Default for VoiceWakeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            wake_phrases: vec!["hey llama".to_string()],
            target_agent: "default".to_string(),
            command_template: r#"openclaw agent --message "${text}" --thinking low"#.to_string(),
            play_confirmation: true,
            silence_timeout_secs: 3,
        }
    }
}

/// Voice wake event.
#[derive(Debug, Clone)]
pub enum WakeEvent {
    /// Wake phrase detected, start listening.
    WakeDetected { phrase: String },
    /// Transcription in progress.
    Transcribing { partial: String },
    /// Final transcription ready.
    Transcribed { text: String },
    /// Error during recognition.
    Error { message: String },
}

/// Manages voice wake detection and forwarding.
pub struct VoiceWakeManager {
    config: VoiceWakeConfig,
}

impl VoiceWakeManager {
    pub fn new(config: VoiceWakeConfig) -> Self {
        Self { config }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Check if text matches a wake phrase.
    pub fn is_wake_phrase(&self, text: &str) -> bool {
        let lower = text.to_lowercase();
        self.config
            .wake_phrases
            .iter()
            .any(|p| lower.contains(&p.to_lowercase()))
    }

    /// Build the command to execute for a transcribed message.
    pub fn build_command(&self, text: &str) -> String {
        self.config.command_template.replace("${text}", text)
    }

    /// Process a wake event.
    pub fn handle_event(&self, event: WakeEvent) -> Option<String> {
        match event {
            WakeEvent::WakeDetected { phrase } => {
                info!(phrase = %phrase, "voice wake detected");
                None
            }
            WakeEvent::Transcribing { partial } => {
                debug!(partial = %partial, "transcription in progress");
                None
            }
            WakeEvent::Transcribed { text } => {
                if text.trim().is_empty() {
                    debug!("empty transcription, ignoring");
                    return None;
                }
                let cmd = self.build_command(&text);
                info!(text = %text, cmd = %cmd, "forwarding voice command");
                Some(cmd)
            }
            WakeEvent::Error { message } => {
                tracing::warn!(error = %message, "voice wake error");
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wake_phrase_detection() {
        let mgr = VoiceWakeManager::new(VoiceWakeConfig::default());
        assert!(mgr.is_wake_phrase("Hey Llama, what's the weather?"));
        assert!(mgr.is_wake_phrase("HEY LLAMA"));
        assert!(!mgr.is_wake_phrase("hello world"));
    }

    #[test]
    fn test_command_building() {
        let mgr = VoiceWakeManager::new(VoiceWakeConfig::default());
        let cmd = mgr.build_command("what's the weather");
        assert!(cmd.contains("what's the weather"));
        assert!(cmd.contains("openclaw agent --message"));
    }

    #[test]
    fn test_transcription_event() {
        let mgr = VoiceWakeManager::new(VoiceWakeConfig {
            enabled: true,
            ..Default::default()
        });
        let result = mgr.handle_event(WakeEvent::Transcribed {
            text: "turn off the lights".to_string(),
        });
        assert!(result.is_some());
        assert!(result.unwrap().contains("turn off the lights"));
    }

    #[test]
    fn test_empty_transcription_ignored() {
        let mgr = VoiceWakeManager::new(VoiceWakeConfig::default());
        let result = mgr.handle_event(WakeEvent::Transcribed {
            text: "  ".to_string(),
        });
        assert!(result.is_none());
    }
}
