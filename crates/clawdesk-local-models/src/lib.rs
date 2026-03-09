//! ClawDesk Local Models — Run LLMs locally without Ollama or LM Studio.
//!
//! This crate provides:
//! - **Hardware detection**: CPU, RAM, GPU (NVIDIA/AMD/Apple Silicon)
//! - **Model database**: Curated list of popular GGUF models with fit scoring
//! - **Inference server**: Manages llama-server (llama.cpp) processes
//! - **Model download**: Downloads GGUF files from HuggingFace with progress
//!
//! # Architecture
//!
//! ```text
//!                    ┌─────────────────────┐
//!                    │  LocalModelManager   │
//!                    │  (public API)        │
//!                    └─────┬───────────────┘
//!          ┌───────────────┼───────────────────┐
//!          ▼               ▼                   ▼
//!   ┌──────────┐   ┌────────────┐     ┌──────────────┐
//!   │ Hardware  │   │  Model DB  │     │  Server Mgr  │
//!   │ Detection │   │  + Fit     │     │  (llama.cpp) │
//!   └──────────┘   └────────────┘     └──────────────┘
//!                                            │
//!                                     ┌──────▼──────┐
//!                                     │ llama-server │
//!                                     │ process(es)  │
//!                                     └─────────────┘
//! ```

pub mod download;
pub mod hardware;
pub mod models;
pub mod server;

pub use download::{delete_model, download_model, DownloadEvent};
pub use hardware::{GpuBackend, GpuInfo, SystemSpecs};
pub use models::{builtin_models, fallback_runtime_context, recommended_runtime_context, FitLevel, LocalModel, ModelFit, RunMode, UseCase};
pub use server::{DownloadedModel, ProcessState, RunningModel, ScannedModel, ServerManager};

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

/// High-level manager for all local model operations.
///
/// This is the main entry point for the clawdesk-local-models crate.
/// It combines hardware detection, model recommendations, server management,
/// and model downloads into a single coordinated API.
pub struct LocalModelManager {
    pub system: SystemSpecs,
    pub server: Arc<ServerManager>,
    pub models_dir: PathBuf,
}

/// Summary of the local models subsystem state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalModelsStatus {
    pub system: SystemSpecs,
    pub llama_server_available: bool,
    pub models_dir: String,
    pub downloaded_models: Vec<DownloadedModel>,
    pub running_models: Vec<RunningModel>,
    pub recommended_models: Vec<ModelFit>,
}

impl LocalModelManager {
    /// Create a new local model manager, detecting hardware automatically.
    ///
    /// If `bundled_server` is provided, it will be used as the llama-server
    /// binary path (e.g., Tauri sidecar). Otherwise, the system PATH is checked.
    pub fn new(models_dir: PathBuf) -> Self {
        Self::new_with_server(models_dir, None)
    }

    /// Create a new local model manager with an explicit llama-server path.
    pub fn new_with_server(models_dir: PathBuf, bundled_server: Option<PathBuf>) -> Self {
        let system = SystemSpecs::detect();
        let server = Arc::new(ServerManager::new(models_dir.clone(), system.clone(), bundled_server));

        info!(
            ram_gb = system.total_ram_gb,
            gpu = ?system.gpu_name,
            backend = ?system.backend,
            models_dir = %models_dir.display(),
            "local model manager initialized"
        );

        Self {
            system,
            server,
            models_dir,
        }
    }

    /// Get the full status of local models subsystem.
    pub async fn status(&self) -> LocalModelsStatus {
        let downloaded = self.server.list_downloaded_models();
        let running = self.server.status().await;
        let recommended = self.recommend_models();
        let llama_server_available = self.server.is_llama_server_available().await;

        LocalModelsStatus {
            system: self.system.clone(),
            llama_server_available,
            models_dir: self.models_dir.display().to_string(),
            downloaded_models: downloaded,
            running_models: running,
            recommended_models: recommended,
        }
    }

    /// Get model recommendations scored against the current hardware.
    pub fn recommend_models(&self) -> Vec<ModelFit> {
        let downloaded_names: std::collections::HashSet<String> = self
            .server
            .list_downloaded_models()
            .iter()
            .map(|m| m.name.clone())
            .collect();

        let mut fits: Vec<ModelFit> = builtin_models()
            .iter()
            .map(|model| {
                let installed = downloaded_names
                    .iter()
                    .any(|d| d.to_lowercase().contains(&model.name.to_lowercase()));
                ModelFit::analyze(model, &self.system, installed)
            })
            .collect();

        // Sort by score descending, filter out models that are too tight
        fits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        fits
    }

    /// Get only models that can run on this hardware.
    pub fn runnable_models(&self) -> Vec<ModelFit> {
        self.recommend_models()
            .into_iter()
            .filter(|f| f.fit_level != FitLevel::TooTight)
            .collect()
    }

    /// Start a model by name (finds the GGUF file and launches llama-server).
    pub async fn start_model(&self, model_name: &str) -> Result<u16, String> {
        // Find the model file
        let downloaded = self.server.list_downloaded_models();
        let model_file = downloaded
            .iter()
            .find(|m| m.name.to_lowercase().contains(&model_name.to_lowercase()))
            .ok_or_else(|| {
                format!(
                    "Model '{}' not found in {}. Download it first.",
                    model_name,
                    self.models_dir.display()
                )
            })?;

        let requested_name = model_name.to_lowercase();
        let downloaded_name = model_file.name.to_lowercase();
        let context_length = builtin_models()
            .iter()
            .find(|candidate| {
                let candidate_name = candidate.name.to_lowercase();
                candidate_name == requested_name
                    || requested_name.contains(&candidate_name)
                    || downloaded_name.contains(&candidate_name)
            })
            .map(|candidate| recommended_runtime_context(candidate, &self.system))
            .unwrap_or_else(|| fallback_runtime_context(&self.system));

        self.server
            .start_model(&model_file.path, model_name, context_length)
            .await
    }

    /// Stop a running model.
    pub async fn stop_model(&self, model_name: &str) -> Result<(), String> {
        self.server.stop_model(model_name).await
    }

    /// Stop all running models.
    pub async fn stop_all(&self) {
        self.server.stop_all().await;
    }

    /// Get the OpenAI-compatible API base URL for a running model.
    pub async fn model_api_url(&self, model_name: &str) -> Option<String> {
        self.server
            .get_model_port(model_name)
            .await
            .map(|port| format!("http://127.0.0.1:{}/v1", port))
    }

    /// Scan an external directory for GGUF files.
    pub fn scan_directory(&self, dir: &std::path::Path) -> Vec<ScannedModel> {
        self.server.scan_directory(dir)
    }

    /// Import a GGUF model from an external path (symlink on Unix, copy on Windows).
    pub async fn import_model(&self, source: &std::path::Path) -> Result<String, String> {
        self.server.import_model(source).await
    }

    /// Start the background TTL reaper task that auto-unloads idle models.
    ///
    /// Spawns a dedicated thread with its own single-threaded Tokio runtime
    /// because this may be called before the main Tauri async runtime is
    /// available (e.g. during `.setup()`).
    pub fn start_ttl_reaper(server: Arc<ServerManager>) {
        std::thread::Builder::new()
            .name("ttl-reaper".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_time()
                    .build()
                    .expect("ttl-reaper tokio runtime");
                rt.block_on(async move {
                    let mut interval =
                        tokio::time::interval(std::time::Duration::from_secs(30));
                    loop {
                        interval.tick().await;
                        server.reap_idle_models().await;
                    }
                });
            })
            .expect("failed to spawn ttl-reaper thread");
    }
}
