//! # clawdesk-voice
//!
//! Multi-provider voice engine with TTS, STT, and Voice Activity Detection.
//!
//! ## TTS Providers
//! - **ElevenLabs**: High-quality neural TTS with stability/similarity controls
//! - **OpenAI TTS**: Fast, affordable TTS with multiple voices
//! - **Edge TTS**: Free Microsoft Edge TTS (no API key required)
//!
//! ## STT (Speech-to-Text)
//! - **Local Whisper**: Privacy-first, on-device inference via whisper.cpp
//! - **Cloud Whisper**: OpenAI Whisper API fallback for accuracy
//!
//! ## VAD (Voice Activity Detection)
//! - Energy-threshold with hysteresis for speech segmentation
//! - Adaptive threshold via exponential moving average
//! - Ring buffer for retroactive pre-speech capture
//!
//! ## Architecture
//! ```text
//! Microphone → VAD → STT → Gateway → Agent → TTS → Speaker
//! ```

pub mod duplex;
pub mod edge_tts;
pub mod elevenlabs;
pub mod openai_tts;
pub mod provider;
pub mod stt;
pub mod tts_core;
pub mod vad;
pub mod voice_call;

pub use provider::{TtsProvider, TtsProviderConfig, TtsRequest, TtsChunk, AudioFormat};
pub use tts_core::{TtsEngine, TtsEngineConfig, Stability, Speed};
pub use voice_call::{VoiceCallPlugin, VoiceCallConfig, CallState};
pub use elevenlabs::ElevenLabsProvider;
pub use openai_tts::OpenAiTtsProvider;
pub use edge_tts::EdgeTtsProvider;
pub use stt::{SttEngine, SttConfig, TranscriptionResult, SttError};
pub use vad::{VoiceActivityDetector, VadConfig, VadState, VadEvent};
pub use duplex::{VoiceStateMachine, VoiceState, VoiceEvent, VoiceAction, EchoCanceller};
