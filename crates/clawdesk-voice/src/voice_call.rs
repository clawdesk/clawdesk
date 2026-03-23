//! Voice call plugin — gateway integration for voice calls.
//!
//! ## Pipeline
//!
//! ```text
//! Audio In → VAD (50ms) → STT (300ms) → LLM (200ms) → TTS (150ms) → Audio Out
//! ```
//!
//! Total latency budget: ~800ms best case, ~1.3s typical.
//!
//! ## Barge-in Detection
//!
//! When VAD detects speech during TTS playback, the system:
//! 1. Stops TTS playback immediately
//! 2. Feeds new audio to STT
//! 3. Cancels pending LLM response

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use tracing::{debug, info};

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
    /// When the call became active.
    started_at: Option<Instant>,
    /// Voice-to-voice latency samples for statistics.
    latency_samples: Vec<Duration>,
    /// Whether barge-in (interrupt TTS with speech) is enabled.
    barge_in_enabled: bool,
}

impl VoiceCallPlugin {
    pub fn new(config: VoiceCallConfig) -> Self {
        Self {
            config,
            state: CallState::Idle,
            started_at: None,
            latency_samples: Vec::new(),
            barge_in_enabled: true,
        }
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
        self.started_at = Some(Instant::now());
        info!("Voice call answered");
        Ok(())
    }

    pub fn end_call(&mut self) {
        self.state = CallState::Ended;
        if let Some(start) = self.started_at {
            info!(
                duration_secs = start.elapsed().as_secs(),
                "Voice call ended"
            );
        }
    }

    /// Put the call on hold.
    pub fn hold(&mut self) -> Result<(), String> {
        if self.state != CallState::Active {
            return Err("call not active".into());
        }
        self.state = CallState::OnHold;
        Ok(())
    }

    /// Resume from hold.
    pub fn resume(&mut self) -> Result<(), String> {
        if self.state != CallState::OnHold {
            return Err("call not on hold".into());
        }
        self.state = CallState::Active;
        Ok(())
    }

    /// Get call duration.
    pub fn duration(&self) -> Option<Duration> {
        self.started_at.map(|t| t.elapsed())
    }

    /// Record a pipeline latency measurement (VAD→STT→LLM→TTS roundtrip).
    pub fn record_latency(&mut self, latency: Duration) {
        self.latency_samples.push(latency);
        // Keep last 100 samples
        if self.latency_samples.len() > 100 {
            self.latency_samples.remove(0);
        }
    }

    /// Average voice-to-voice latency.
    pub fn avg_latency(&self) -> Option<Duration> {
        if self.latency_samples.is_empty() {
            return None;
        }
        let total: Duration = self.latency_samples.iter().sum();
        Some(total / self.latency_samples.len() as u32)
    }

    /// Whether barge-in should interrupt current TTS.
    pub fn should_barge_in(&self) -> bool {
        self.barge_in_enabled && self.state == CallState::Active
    }

    /// Enable/disable barge-in detection.
    pub fn set_barge_in(&mut self, enabled: bool) {
        self.barge_in_enabled = enabled;
    }

    /// Get call statistics.
    pub fn stats(&self) -> CallStats {
        CallStats {
            state: self.state,
            duration: self.duration(),
            avg_latency: self.avg_latency(),
            latency_samples: self.latency_samples.len(),
            barge_in_enabled: self.barge_in_enabled,
        }
    }
}

/// Voice call latency budget breakdown.
#[derive(Debug, Clone, Serialize)]
pub struct LatencyBudget {
    /// VAD processing per frame (~50ms for 30ms frames).
    pub vad_ms: u64,
    /// STT partial result latency.
    pub stt_partial_ms: u64,
    /// STT final result latency.
    pub stt_final_ms: u64,
    /// LLM first-token latency.
    pub llm_first_token_ms: u64,
    /// TTS first-audio latency.
    pub tts_first_audio_ms: u64,
    /// Network RTT.
    pub rtt_ms: u64,
}

impl Default for LatencyBudget {
    fn default() -> Self {
        Self {
            vad_ms: 50,
            stt_partial_ms: 300,
            stt_final_ms: 800,
            llm_first_token_ms: 200,
            tts_first_audio_ms: 150,
            rtt_ms: 100,
        }
    }
}

impl LatencyBudget {
    /// Best-case end-to-end latency.
    pub fn best_case_ms(&self) -> u64 {
        self.vad_ms + self.stt_partial_ms + self.llm_first_token_ms + self.tts_first_audio_ms
    }

    /// Typical end-to-end latency.
    pub fn typical_ms(&self) -> u64 {
        self.vad_ms
            + self.stt_final_ms
            + self.llm_first_token_ms
            + self.tts_first_audio_ms
            + self.rtt_ms
    }
}

/// Call statistics snapshot.
#[derive(Debug, Clone, Serialize)]
pub struct CallStats {
    pub state: CallState,
    #[serde(skip)]
    pub duration: Option<Duration>,
    #[serde(skip)]
    pub avg_latency: Option<Duration>,
    pub latency_samples: usize,
    pub barge_in_enabled: bool,
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

    #[test]
    fn hold_and_resume() {
        let mut plugin = VoiceCallPlugin::new(VoiceCallConfig::default());
        plugin.start_call().unwrap();
        plugin.answer().unwrap();
        plugin.hold().unwrap();
        assert_eq!(plugin.state(), CallState::OnHold);
        plugin.resume().unwrap();
        assert!(plugin.is_active());
    }

    #[test]
    fn latency_tracking() {
        let mut plugin = VoiceCallPlugin::new(VoiceCallConfig::default());
        plugin.record_latency(Duration::from_millis(800));
        plugin.record_latency(Duration::from_millis(1200));
        let avg = plugin.avg_latency().unwrap();
        assert_eq!(avg.as_millis(), 1000);
    }

    #[test]
    fn barge_in_detection() {
        let mut plugin = VoiceCallPlugin::new(VoiceCallConfig::default());
        assert!(!plugin.should_barge_in()); // not active
        plugin.start_call().unwrap();
        plugin.answer().unwrap();
        assert!(plugin.should_barge_in()); // active + enabled
        plugin.set_barge_in(false);
        assert!(!plugin.should_barge_in()); // disabled
    }

    #[test]
    fn latency_budget() {
        let budget = LatencyBudget::default();
        assert!(budget.best_case_ms() < 1000);
        assert!(budget.typical_ms() < 2000);
    }

    #[test]
    fn call_stats() {
        let mut plugin = VoiceCallPlugin::new(VoiceCallConfig::default());
        plugin.start_call().unwrap();
        plugin.answer().unwrap();
        let stats = plugin.stats();
        assert_eq!(stats.state, CallState::Active);
        assert!(stats.duration.is_some());
    }
}
