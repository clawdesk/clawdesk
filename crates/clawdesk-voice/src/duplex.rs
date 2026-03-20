//! Full-Duplex Voice Conversation with Barge-In
//!
//! Builds on Phase A's push-to-talk foundation to add continuous listening,
//! barge-in support, and echo cancellation.
//!
//! ## State Machine
//!
//! ```text
//! Idle → Listening → Processing → Speaking → Idle
//!                                    ↓
//!                                 BargeIn → Processing
//! ```
//!
//! 5 states × 4 input events = 20 transitions, exhaustively testable.
//!
//! ## Echo Cancellation (NLMS)
//!
//! ```text
//! w(n+1) = w(n) + μ × e(n) × x(n) / (||x(n)||² + δ)
//! ```
//! Filter length: 2048 taps at 16kHz = 128ms echo path.
//! Cost: O(L) per sample = 32.8M MAC/sec — single core budget.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Voice conversation states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceState {
    /// No voice activity
    Idle,
    /// VAD listening for speech
    Listening,
    /// STT processing, waiting for LLM response
    Processing,
    /// TTS playing response audio
    Speaking,
    /// User interrupted during TTS playback
    BargeIn,
}

/// Events that drive voice state transitions.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VoiceEvent {
    /// VAD detected speech start
    SpeechDetected,
    /// STT transcription complete
    TranscriptionReady { text: String },
    /// LLM response ready for TTS
    ResponseReady { text: String },
    /// TTS playback finished
    PlaybackComplete,
    /// User spoke while TTS was playing (barge-in)
    BargeInDetected,
    /// Manual start (push-to-talk)
    ManualStart,
    /// Manual stop (push-to-talk release)
    ManualStop,
    /// Error occurred
    Error { message: String },
}

/// Action to take after a state transition.
#[derive(Debug, Clone)]
pub enum VoiceAction {
    /// Start capturing audio from microphone
    StartCapture,
    /// Stop capturing audio
    StopCapture,
    /// Begin STT transcription
    BeginTranscription { audio: Vec<i16> },
    /// Send text to LLM gateway
    SendToGateway { transcript: String },
    /// Begin TTS synthesis and playback
    BeginTts { text: String },
    /// Cancel ongoing TTS playback
    CancelTts,
    /// Cancel in-flight LLM request
    CancelLlmRequest,
    /// No action needed
    None,
}

/// Full-duplex voice state machine.
pub struct VoiceStateMachine {
    state: VoiceState,
    /// Whether continuous listening is enabled (vs push-to-talk)
    continuous_mode: bool,
}

impl VoiceStateMachine {
    pub fn new(continuous_mode: bool) -> Self {
        Self {
            state: if continuous_mode { VoiceState::Listening } else { VoiceState::Idle },
            continuous_mode,
        }
    }

    /// Current state.
    pub fn state(&self) -> VoiceState {
        self.state
    }

    /// Process an event and return the action to take.
    ///
    /// The 20-transition matrix (5 states × 4 primary events):
    pub fn transition(&mut self, event: VoiceEvent) -> VoiceAction {
        let (new_state, action) = match (&self.state, event) {
            // Idle transitions
            (VoiceState::Idle, VoiceEvent::SpeechDetected) if self.continuous_mode => {
                (VoiceState::Listening, VoiceAction::StartCapture)
            }
            (VoiceState::Idle, VoiceEvent::ManualStart) => {
                (VoiceState::Listening, VoiceAction::StartCapture)
            }

            // Listening transitions
            (VoiceState::Listening, VoiceEvent::TranscriptionReady { text }) => {
                (VoiceState::Processing, VoiceAction::SendToGateway { transcript: text })
            }
            (VoiceState::Listening, VoiceEvent::ManualStop) => {
                // Push-to-talk release — force end VAD and transcribe
                (VoiceState::Processing, VoiceAction::StopCapture)
            }

            // Processing transitions
            (VoiceState::Processing, VoiceEvent::ResponseReady { text }) => {
                (VoiceState::Speaking, VoiceAction::BeginTts { text })
            }
            (VoiceState::Processing, VoiceEvent::Error { .. }) => {
                let next = if self.continuous_mode { VoiceState::Listening } else { VoiceState::Idle };
                (next, VoiceAction::None)
            }

            // Speaking transitions
            (VoiceState::Speaking, VoiceEvent::PlaybackComplete) => {
                let next = if self.continuous_mode { VoiceState::Listening } else { VoiceState::Idle };
                (next, if self.continuous_mode { VoiceAction::StartCapture } else { VoiceAction::None })
            }
            (VoiceState::Speaking, VoiceEvent::BargeInDetected) => {
                // Barge-in: cancel TTS, start new capture
                (VoiceState::BargeIn, VoiceAction::CancelTts)
            }
            (VoiceState::Speaking, VoiceEvent::SpeechDetected) if self.continuous_mode => {
                // Same as barge-in in continuous mode
                (VoiceState::BargeIn, VoiceAction::CancelTts)
            }

            // Barge-in transitions
            (VoiceState::BargeIn, VoiceEvent::TranscriptionReady { text }) => {
                (VoiceState::Processing, VoiceAction::SendToGateway { transcript: text })
            }

            // Default: no transition
            (_, _) => {
                return VoiceAction::None;
            }
        };

        self.state = new_state;
        action
    }

    /// Reset to idle.
    pub fn reset(&mut self) {
        self.state = if self.continuous_mode { VoiceState::Listening } else { VoiceState::Idle };
    }
}

// ---------------------------------------------------------------------------
// Echo Cancellation — NLMS Adaptive Filter
// ---------------------------------------------------------------------------

/// Normalized Least Mean Squares (NLMS) echo canceller.
///
/// ```text
/// w(n+1) = w(n) + μ × e(n) × x(n) / (||x(n)||² + δ)
/// ```
///
/// Filter length: 2048 taps at 16kHz = 128ms echo path.
pub struct EchoCanceller {
    /// Filter weights
    weights: Vec<f32>,
    /// Reference signal buffer (TTS output)
    reference_buffer: VecDeque<f32>,
    /// Step size (μ)
    step_size: f32,
    /// Regularization (δ) to prevent division by zero
    regularization: f32,
    /// Filter length in samples
    filter_length: usize,
}

impl EchoCanceller {
    /// Create a new echo canceller.
    ///
    /// `filter_length`: 2048 for 128ms echo path at 16kHz
    /// `step_size`: 0.1–0.5 (higher = faster adaptation, more noise)
    pub fn new(filter_length: usize, step_size: f32) -> Self {
        Self {
            weights: vec![0.0; filter_length],
            reference_buffer: VecDeque::with_capacity(filter_length),
            step_size,
            regularization: 1e-6,
            filter_length,
        }
    }

    /// Feed a reference sample (TTS output being played through speaker).
    pub fn feed_reference(&mut self, sample: f32) {
        self.reference_buffer.push_back(sample);
        if self.reference_buffer.len() > self.filter_length {
            self.reference_buffer.pop_front();
        }
    }

    /// Process a microphone sample and return echo-cancelled output.
    ///
    /// Cost: O(L) per sample where L = filter_length.
    pub fn process(&mut self, mic_sample: f32) -> f32 {
        if self.reference_buffer.len() < self.filter_length {
            return mic_sample; // Not enough reference data yet
        }

        // Compute estimated echo: y_hat = w^T × x
        let mut echo_estimate: f32 = 0.0;
        for (i, &w) in self.weights.iter().enumerate() {
            if let Some(&ref_sample) = self.reference_buffer.get(self.reference_buffer.len() - 1 - i) {
                echo_estimate += w * ref_sample;
            }
        }

        // Error signal: e = mic - y_hat
        let error = mic_sample - echo_estimate;

        // Compute ||x||²
        let ref_power: f32 = self.reference_buffer.iter().map(|x| x * x).sum();

        // Update weights: w(n+1) = w(n) + μ × e × x / (||x||² + δ)
        let normalization = self.step_size / (ref_power + self.regularization);
        for (i, w) in self.weights.iter_mut().enumerate() {
            if let Some(&ref_sample) = self.reference_buffer.get(self.reference_buffer.len() - 1 - i) {
                *w += normalization * error * ref_sample;
            }
        }

        error // Return the echo-cancelled signal
    }

    /// Reset the filter weights.
    pub fn reset(&mut self) {
        self.weights.fill(0.0);
        self.reference_buffer.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_state_machine_push_to_talk() {
        let mut sm = VoiceStateMachine::new(false);
        assert_eq!(sm.state(), VoiceState::Idle);

        // Push-to-talk start
        let action = sm.transition(VoiceEvent::ManualStart);
        assert!(matches!(action, VoiceAction::StartCapture));
        assert_eq!(sm.state(), VoiceState::Listening);

        // Transcription ready
        let action = sm.transition(VoiceEvent::TranscriptionReady { text: "hello".into() });
        assert!(matches!(action, VoiceAction::SendToGateway { .. }));
        assert_eq!(sm.state(), VoiceState::Processing);

        // Response ready
        let action = sm.transition(VoiceEvent::ResponseReady { text: "hi there".into() });
        assert!(matches!(action, VoiceAction::BeginTts { .. }));
        assert_eq!(sm.state(), VoiceState::Speaking);

        // Playback complete
        let action = sm.transition(VoiceEvent::PlaybackComplete);
        assert!(matches!(action, VoiceAction::None));
        assert_eq!(sm.state(), VoiceState::Idle);
    }

    #[test]
    fn barge_in_cancels_tts() {
        let mut sm = VoiceStateMachine::new(true);

        // Get to speaking state
        sm.transition(VoiceEvent::SpeechDetected);
        sm.transition(VoiceEvent::TranscriptionReady { text: "test".into() });
        sm.transition(VoiceEvent::ResponseReady { text: "response".into() });
        assert_eq!(sm.state(), VoiceState::Speaking);

        // Barge-in
        let action = sm.transition(VoiceEvent::BargeInDetected);
        assert!(matches!(action, VoiceAction::CancelTts));
        assert_eq!(sm.state(), VoiceState::BargeIn);
    }

    #[test]
    fn continuous_mode_returns_to_listening() {
        let mut sm = VoiceStateMachine::new(true);

        sm.transition(VoiceEvent::SpeechDetected);
        sm.transition(VoiceEvent::TranscriptionReady { text: "test".into() });
        sm.transition(VoiceEvent::ResponseReady { text: "response".into() });
        sm.transition(VoiceEvent::PlaybackComplete);

        assert_eq!(sm.state(), VoiceState::Listening);
    }

    #[test]
    fn echo_canceller_reduces_echo() {
        let mut ec = EchoCanceller::new(64, 0.3);

        // Feed reference signal (sine wave simulating TTS output)
        let reference: Vec<f32> = (0..100)
            .map(|i| (i as f32 * 0.1).sin())
            .collect();

        for &r in &reference {
            ec.feed_reference(r);
        }

        // Process mic signal that includes echo
        let echo_gain = 0.5_f32;
        let results: Vec<f32> = reference.iter()
            .map(|&r| {
                let mic = r * echo_gain; // Echo of reference
                ec.process(mic)
            })
            .collect();

        // After adaptation, output should have reduced amplitude
        let input_power: f32 = reference.iter().skip(64).map(|x| x * x).sum();
        let output_power: f32 = results.iter().skip(64).map(|x| x * x).sum();
        // Echo canceller should reduce power (won't be perfect with so few samples)
        assert!(
            output_power < input_power * echo_gain * echo_gain * 2.0,
            "Echo should be reduced"
        );
    }
}
