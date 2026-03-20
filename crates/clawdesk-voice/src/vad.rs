//! Voice Activity Detection (VAD) — energy-threshold with hysteresis.
//!
//! Segments speech from silence using adaptive energy detection.
//!
//! ## Algorithm
//!
//! ```text
//! E(frame) = (1/N) Σ|xᵢ|²          — frame energy
//! θₜ = α × θₜ₋₁ + (1-α) × E(frame) — adaptive threshold (EMA, α=0.95)
//! Start: E > 1.5θ for 3 frames (90ms)
//! End:   E < 0.7θ for 15 frames (450ms)
//! ```
//!
//! Ring buffer: 16 KB at 16 kHz/16-bit = 0.5s retroactive capture.

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// VAD configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VadConfig {
    /// Frame size in samples (default: 480 = 30ms at 16kHz)
    pub frame_samples: usize,
    /// Sample rate (expected: 16000)
    pub sample_rate: u32,
    /// EMA smoothing factor for threshold (default: 0.95)
    pub ema_alpha: f64,
    /// Start threshold multiplier (default: 1.5)
    pub start_multiplier: f64,
    /// End threshold multiplier (default: 0.7)
    pub end_multiplier: f64,
    /// Frames above threshold to start (default: 3 = 90ms)
    pub start_frames: usize,
    /// Frames below threshold to end (default: 15 = 450ms)
    pub end_frames: usize,
    /// Ring buffer size for retroactive capture (default: 8000 = 0.5s at 16kHz)
    pub ring_buffer_size: usize,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            frame_samples: 480, // 30ms at 16kHz
            sample_rate: 16000,
            ema_alpha: 0.95,
            start_multiplier: 1.5,
            end_multiplier: 0.7,
            start_frames: 3,
            end_frames: 15,
            ring_buffer_size: 8000, // 0.5s at 16kHz
        }
    }
}

/// VAD state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VadState {
    /// No speech detected
    Silence,
    /// Potentially starting speech (hysteresis debounce)
    PossibleStart,
    /// Speech detected and active
    Speech,
    /// Potentially ending speech (hysteresis debounce)
    PossibleEnd,
}

/// Event emitted by the VAD.
#[derive(Debug, Clone)]
pub enum VadEvent {
    /// Speech started — includes retroactive ring buffer content
    SpeechStart {
        /// Pre-speech audio from ring buffer (for context)
        pre_speech: Vec<i16>,
    },
    /// Speech data available (ongoing speech)
    SpeechData {
        samples: Vec<i16>,
    },
    /// Speech ended — all speech audio collected
    SpeechEnd {
        /// Complete utterance audio
        utterance: Vec<i16>,
        /// Duration in milliseconds
        duration_ms: u64,
    },
}

/// Voice Activity Detector with hysteresis.
pub struct VoiceActivityDetector {
    config: VadConfig,
    state: VadState,
    /// Adaptive energy threshold (EMA)
    threshold: f64,
    /// Counter for hysteresis debounce
    hysteresis_counter: usize,
    /// Ring buffer for retroactive pre-speech capture
    ring_buffer: VecDeque<i16>,
    /// Accumulated speech samples during an utterance
    utterance_buffer: Vec<i16>,
    /// Whether we've initialized the threshold
    initialized: bool,
}

impl VoiceActivityDetector {
    pub fn new(config: VadConfig) -> Self {
        Self {
            ring_buffer: VecDeque::with_capacity(config.ring_buffer_size),
            config,
            state: VadState::Silence,
            threshold: 0.0,
            hysteresis_counter: 0,
            utterance_buffer: Vec::new(),
            initialized: false,
        }
    }

    /// Process a frame of audio samples and return any events.
    ///
    /// Call this with consecutive frames of PCM audio (16-bit, 16kHz, mono).
    /// Frame size should match `config.frame_samples`.
    pub fn process_frame(&mut self, frame: &[i16]) -> Vec<VadEvent> {
        let energy = Self::frame_energy(frame);
        let mut events = Vec::new();

        // Initialize threshold on first frame
        if !self.initialized {
            self.threshold = energy;
            self.initialized = true;
        }

        // Update adaptive threshold: θₜ = α × θₜ₋₁ + (1-α) × E(frame)
        self.threshold = self.config.ema_alpha * self.threshold
            + (1.0 - self.config.ema_alpha) * energy;

        let is_above = energy > self.config.start_multiplier * self.threshold;
        let is_below = energy < self.config.end_multiplier * self.threshold;

        match self.state {
            VadState::Silence => {
                // Buffer audio for retroactive capture
                self.push_ring_buffer(frame);

                if is_above {
                    self.hysteresis_counter = 1;
                    self.state = VadState::PossibleStart;
                }
            }

            VadState::PossibleStart => {
                self.push_ring_buffer(frame);

                if is_above {
                    self.hysteresis_counter += 1;
                    if self.hysteresis_counter >= self.config.start_frames {
                        // Speech confirmed — emit start event with retroactive audio
                        let pre_speech: Vec<i16> = self.ring_buffer.iter().copied().collect();
                        events.push(VadEvent::SpeechStart { pre_speech: pre_speech.clone() });

                        // Start new utterance with ring buffer content
                        self.utterance_buffer.clear();
                        self.utterance_buffer.extend_from_slice(&pre_speech);
                        self.utterance_buffer.extend_from_slice(frame);
                        self.ring_buffer.clear();

                        self.state = VadState::Speech;
                        self.hysteresis_counter = 0;
                    }
                } else {
                    // False alarm — back to silence
                    self.state = VadState::Silence;
                    self.hysteresis_counter = 0;
                }
            }

            VadState::Speech => {
                self.utterance_buffer.extend_from_slice(frame);
                events.push(VadEvent::SpeechData {
                    samples: frame.to_vec(),
                });

                if is_below {
                    self.hysteresis_counter = 1;
                    self.state = VadState::PossibleEnd;
                }
            }

            VadState::PossibleEnd => {
                self.utterance_buffer.extend_from_slice(frame);

                if is_below {
                    self.hysteresis_counter += 1;
                    if self.hysteresis_counter >= self.config.end_frames {
                        // Speech ended
                        let duration_ms = (self.utterance_buffer.len() as u64 * 1000)
                            / self.config.sample_rate as u64;

                        events.push(VadEvent::SpeechEnd {
                            utterance: std::mem::take(&mut self.utterance_buffer),
                            duration_ms,
                        });

                        self.state = VadState::Silence;
                        self.hysteresis_counter = 0;
                    }
                } else {
                    // False end — continue speech
                    events.push(VadEvent::SpeechData {
                        samples: frame.to_vec(),
                    });
                    self.state = VadState::Speech;
                    self.hysteresis_counter = 0;
                }
            }
        }

        events
    }

    /// Compute frame energy: E(frame) = (1/N) Σ|xᵢ|²
    #[inline]
    fn frame_energy(frame: &[i16]) -> f64 {
        if frame.is_empty() {
            return 0.0;
        }
        let sum: f64 = frame.iter()
            .map(|&x| (x as f64) * (x as f64))
            .sum();
        sum / frame.len() as f64
    }

    /// Push samples to the ring buffer, maintaining max size.
    fn push_ring_buffer(&mut self, frame: &[i16]) {
        for &sample in frame {
            if self.ring_buffer.len() >= self.config.ring_buffer_size {
                self.ring_buffer.pop_front();
            }
            self.ring_buffer.push_back(sample);
        }
    }

    /// Current VAD state.
    pub fn state(&self) -> VadState {
        self.state
    }

    /// Current adaptive threshold.
    pub fn threshold(&self) -> f64 {
        self.threshold
    }

    /// Reset the detector.
    pub fn reset(&mut self) {
        self.state = VadState::Silence;
        self.threshold = 0.0;
        self.hysteresis_counter = 0;
        self.ring_buffer.clear();
        self.utterance_buffer.clear();
        self.initialized = false;
    }

    /// Force end of speech (e.g., push-to-talk release).
    pub fn force_end(&mut self) -> Option<VadEvent> {
        if matches!(self.state, VadState::Speech | VadState::PossibleEnd) {
            let duration_ms = (self.utterance_buffer.len() as u64 * 1000)
                / self.config.sample_rate as u64;
            let event = VadEvent::SpeechEnd {
                utterance: std::mem::take(&mut self.utterance_buffer),
                duration_ms,
            };
            self.state = VadState::Silence;
            self.hysteresis_counter = 0;
            Some(event)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn silence_frame(len: usize) -> Vec<i16> {
        vec![0i16; len]
    }

    fn speech_frame(len: usize, amplitude: i16) -> Vec<i16> {
        (0..len).map(|i| {
            let t = i as f64 / 16000.0;
            (amplitude as f64 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()) as i16
        }).collect()
    }

    #[test]
    fn silence_stays_silent() {
        let config = VadConfig::default();
        let mut vad = VoiceActivityDetector::new(config);
        for _ in 0..100 {
            let events = vad.process_frame(&silence_frame(480));
            assert!(events.is_empty() || events.iter().all(|e| !matches!(e, VadEvent::SpeechStart { .. })));
        }
        assert_eq!(vad.state(), VadState::Silence);
    }

    #[test]
    fn speech_detected_after_threshold() {
        let config = VadConfig {
            start_frames: 2,
            end_frames: 3,
            ..VadConfig::default()
        };
        let mut vad = VoiceActivityDetector::new(config);

        // Feed some silence to establish baseline
        for _ in 0..20 {
            vad.process_frame(&silence_frame(480));
        }

        // Feed loud speech frames
        let mut got_start = false;
        for _ in 0..10 {
            let events = vad.process_frame(&speech_frame(480, 20000));
            if events.iter().any(|e| matches!(e, VadEvent::SpeechStart { .. })) {
                got_start = true;
                break;
            }
        }
        assert!(got_start, "Expected speech start event");
    }

    #[test]
    fn speech_end_after_silence() {
        let config = VadConfig {
            start_frames: 2,
            end_frames: 3,
            ..VadConfig::default()
        };
        let mut vad = VoiceActivityDetector::new(config);

        // Establish baseline
        for _ in 0..20 {
            vad.process_frame(&silence_frame(480));
        }

        // Start speech
        for _ in 0..10 {
            vad.process_frame(&speech_frame(480, 20000));
        }

        // End with silence
        let mut got_end = false;
        for _ in 0..20 {
            let events = vad.process_frame(&silence_frame(480));
            if events.iter().any(|e| matches!(e, VadEvent::SpeechEnd { .. })) {
                got_end = true;
                break;
            }
        }
        assert!(got_end, "Expected speech end event");
    }

    #[test]
    fn force_end_returns_utterance() {
        let config = VadConfig {
            start_frames: 1,
            ..VadConfig::default()
        };
        let mut vad = VoiceActivityDetector::new(config);

        // Establish baseline
        for _ in 0..20 {
            vad.process_frame(&silence_frame(480));
        }

        // Feed speech
        for _ in 0..5 {
            vad.process_frame(&speech_frame(480, 20000));
        }

        // Force end (push-to-talk release)
        let event = vad.force_end();
        assert!(event.is_some());
        if let Some(VadEvent::SpeechEnd { utterance, .. }) = event {
            assert!(!utterance.is_empty());
        }
    }

    #[test]
    fn frame_energy_computation() {
        let frame: Vec<i16> = vec![100, -100, 100, -100];
        let energy = VoiceActivityDetector::frame_energy(&frame);
        assert!((energy - 10000.0).abs() < 0.1);
    }
}
