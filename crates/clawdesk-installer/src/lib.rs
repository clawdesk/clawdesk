//! # clawdesk-installer
//!
//! Single-binary installer with embedded Ollama bootstrap.
//!
//! Reduces install-to-first-AI-response time from >20 minutes to <90 seconds.
//! Zero terminal commands. Zero API key requirement on first run.
//!
//! ## Critical Path
//!
//! ```text
//! T_total = T_download + T_extract + T_model_load + T_first_inference
//! ```
//!
//! With pre-compiled Tauri binary (~25 MB), Ollama bootstrap (~15 MB),
//! and 1B Q4 model (~500 MB):
//! - 50 Mbps: ~77s perceived
//! - 10 Mbps: ~205s perceived (streams small model first)

pub mod bootstrap;
pub mod model_selector;
pub mod platform;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum InstallerError {
    #[error("platform not supported: {0}")]
    UnsupportedPlatform(String),

    #[error("Ollama bootstrap failed: {0}")]
    OllamaBootstrapFailed(String),

    #[error("model download failed: {0}")]
    ModelDownloadFailed(String),

    #[error("hardware detection failed: {0}")]
    HardwareDetectionFailed(String),

    #[error("insufficient resources: {0}")]
    InsufficientResources(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Network(#[from] reqwest::Error),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Installation progress event for UI updates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InstallProgress {
    /// Hardware detection phase
    DetectingHardware,
    /// Hardware detected
    HardwareDetected {
        ram_gb: f64,
        has_gpu: bool,
        gpu_name: Option<String>,
    },
    /// Checking for existing Ollama installation
    CheckingOllama,
    /// Downloading Ollama runtime
    DownloadingOllama { progress_pct: f32 },
    /// Ollama installed
    OllamaReady,
    /// Selecting model based on hardware
    SelectingModel,
    /// Downloading AI model
    DownloadingModel {
        model_name: String,
        size_mb: u64,
        progress_pct: f32,
    },
    /// Model ready for inference
    ModelReady { model_name: String },
    /// First inference test
    TestingInference,
    /// Installation complete
    Complete {
        model_name: String,
        total_time_secs: f64,
    },
    /// Error during installation
    Error { message: String },
}

/// Installation configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstallConfig {
    /// Override automatic model selection
    pub model_override: Option<String>,
    /// Skip Ollama installation if already present
    pub skip_ollama_if_present: bool,
    /// Maximum model size in GB (for bandwidth-constrained environments)
    pub max_model_size_gb: Option<f64>,
    /// Whether to run first inference test
    pub test_inference: bool,
}

impl Default for InstallConfig {
    fn default() -> Self {
        Self {
            model_override: None,
            skip_ollama_if_present: true,
            max_model_size_gb: None,
            test_inference: true,
        }
    }
}
