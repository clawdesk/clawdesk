//! # clawdesk-voice
//!
//! Multi-provider TTS engine with compile-time parameter safety, real-time
//! audio streaming, VoiceWake keyword detection, and voice call integration.
//!
//! ## Providers
//! - **ElevenLabs**: High-quality neural TTS with stability/similarity controls
//! - **OpenAI TTS**: Fast, affordable TTS with multiple voices
//! - **Edge TTS**: Free Microsoft Edge TTS (no API key required)
//!
//! ## Architecture
//! ```text
//! TTS_Provider → Chunker → Channel_Encoder → Delivery
//! ```
//! Each stage is a `tokio::mpsc` channel for real-time streaming.

pub mod edge_tts;
pub mod elevenlabs;
pub mod openai_tts;
pub mod provider;
pub mod tts_core;
pub mod voice_call;

pub use provider::{TtsProvider, TtsProviderConfig, TtsRequest, TtsChunk, AudioFormat};
pub use tts_core::{TtsEngine, TtsEngineConfig, Stability, Speed};
pub use voice_call::{VoiceCallPlugin, VoiceCallConfig, CallState};
pub use elevenlabs::ElevenLabsProvider;
pub use openai_tts::OpenAiTtsProvider;
pub use edge_tts::EdgeTtsProvider;
