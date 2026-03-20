//! # clawdesk-media
//!
//! Media understanding pipeline — audio transcription, image analysis,
//! document parsing, TTS, with adaptive provider selection and concurrency control.
//!
//! ## Architecture
//! - **MediaProcessor**: Trait for processing media content
//! - **AdaptiveSelector**: Thompson Sampling for provider selection
//! - **MediaPipeline**: Orchestrates processing with concurrency control
//! - **ImageProcessor**: Vision API integration for image understanding
//! - **AudioProcessor**: Whisper / speech-to-text
//! - **DocumentProcessor**: PDF, DOCX, text extraction
//! - **TtsProcessor**: Text-to-speech synthesis
//! - **MediaCache**: Content-addressed cache with SHA256 keys

pub mod artifact_pipeline;
pub mod durable_artifact_index;
pub mod audio;
pub mod cache;
pub mod cache_pro;
pub mod dag;
pub mod document;
pub mod error;
pub mod format;
pub mod image;
pub mod pipeline;
pub mod processor;
pub mod recorder;
pub mod selector;
pub mod image_gen;
pub mod talk_mode;
pub mod tts;
pub mod understanding;
pub mod video;
pub mod voice;
pub mod whisper;
pub mod link_understanding;

pub use pipeline::MediaPipeline;
pub use processor::{MediaProcessor, ProcessorResult};
pub use selector::AdaptiveSelector;
pub use cache::MediaCache;
pub use artifact_pipeline::{ArtifactPipeline, AcpArtifactInput, AcpDataInput};
pub use video::{VideoProcessor, VideoMetadata, VideoFormat, FfmpegVideoProcessor, StubVideoProcessor, create_video_processor};
pub use voice::{VoicePipeline, VoiceActivityDetector, VoicePipelineConfig, VoiceEvent};
pub use talk_mode::{TalkModeController, TalkModeConfig, TalkPhase, TalkEvent, ActivationSource};
pub use link_understanding::{LinkUnderstanding, LinkPreview, LinkConfig};
pub use understanding::{UnderstandingDispatcher, UnderstandingProvider, MediaCapability, UnderstandingResult};
pub use image_gen::{
    ImageGenProvider, ImageGenRegistry, ImageGenRequest, ImageGenResponse,
    ImageGenCapabilities, ImageGenError, ImageModel, GeneratedImage,
};
