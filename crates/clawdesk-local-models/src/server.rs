//! Local inference server manager.
//!
//! Manages llama-server (llama.cpp) processes for running local GGUF models.
//! Inspired by llama-swap's process lifecycle management with hot-swapping.

use crate::hardware::SystemSpecs;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::process::{Child, Command};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

/// State of a managed inference process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessState {
    Stopped,
    Starting,
    Ready,
    Stopping,
    Failed,
}

/// Configuration for a local model server instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelServerConfig {
    pub model_path: PathBuf,
    pub model_name: String,
    pub port: u16,
    pub context_length: u32,
    pub gpu_layers: Option<i32>,
    pub threads: Option<u32>,
    /// Auto-unload after this many seconds of inactivity (0 = never).
    pub ttl_secs: u64,
    /// Optional path to a draft model for speculative decoding (llama.cpp --draft).
    #[serde(default)]
    pub draft_model_path: Option<PathBuf>,
}

/// A running inference server process.
struct ManagedProcess {
    config: ModelServerConfig,
    process: Option<Child>,
    state: ProcessState,
    pid: Option<u32>,
    /// Last time a request was proxied through this model.
    last_used: std::time::Instant,
}

/// Manages local inference server processes (llama-server).
///
/// Handles process lifecycle, health checks, hot-swapping, TTL auto-unload,
/// and port allocation.
pub struct ServerManager {
    processes: Arc<RwLock<HashMap<String, ManagedProcess>>>,
    llama_server_path: Arc<RwLock<Option<PathBuf>>>,
    models_dir: PathBuf,
    next_port: Arc<RwLock<u16>>,
    system: SystemSpecs,
    /// Default TTL in seconds for idle model auto-unload (0 = never).
    default_ttl_secs: Arc<RwLock<u64>>,
}

impl ServerManager {
    /// Create a new server manager.
    ///
    /// If `bundled_path` is `Some`, it will be used as the llama-server binary
    /// (Tauri sidecar). Otherwise, falls back to detecting llama-server on
    /// the system PATH.
    pub fn new(models_dir: PathBuf, system: SystemSpecs, bundled_path: Option<PathBuf>) -> Self {
        let llama_server = if let Some(ref bp) = bundled_path {
            if bp.exists() {
                info!(path = ?bp, "using bundled llama-server sidecar");
                Some(bp.clone())
            } else {
                info!(path = ?bp, "bundled path does not exist, falling back to detection");
                detect_llama_server()
            }
        } else {
            detect_llama_server()
        };
        info!(path = ?llama_server, "llama-server detection");

        Self {
            processes: Arc::new(RwLock::new(HashMap::new())),
            llama_server_path: Arc::new(RwLock::new(llama_server)),
            models_dir,
            next_port: Arc::new(RwLock::new(39090)),
            system,
            default_ttl_secs: Arc::new(RwLock::new(0)), // 0 = never auto-unload
        }
    }

    /// Get the models directory.
    pub fn models_dir(&self) -> &Path {
        &self.models_dir
    }

    /// Set the path to llama-server binary.
    pub async fn set_llama_server_path(&self, path: PathBuf) {
        *self.llama_server_path.write().await = Some(path);
    }

    /// Check if llama-server is available.
    pub async fn is_llama_server_available(&self) -> bool {
        self.llama_server_path
            .read()
            .await
            .as_ref()
            .map(|path| is_valid_llama_server_path(path))
            .unwrap_or(false)
    }

    /// List all GGUF model files in the models directory.
    pub fn list_downloaded_models(&self) -> Vec<DownloadedModel> {
        let mut models = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&self.models_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("gguf") {
                    let name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let size_bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    models.push(DownloadedModel {
                        name,
                        path: path.clone(),
                        size_gb: size_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                    });
                }
            }
        }
        models
    }

    /// Start a model server for a given GGUF file.
    pub async fn start_model(&self, model_path: &Path, model_name: &str, context_length: u32) -> Result<u16, String> {
        let server_path = self
            .llama_server_path
            .read()
            .await
            .clone()
            .ok_or("llama-server not found. Install llama.cpp or set the path manually.")?;

        ensure_llama_server_path(&server_path)?;

        // Allocate port
        let port = {
            let mut p = self.next_port.write().await;
            let port = *p;
            *p += 1;
            port
        };

        // Determine GPU layers
        let gpu_layers = if self.system.has_gpu { Some(99) } else { Some(0) };

        let config = ModelServerConfig {
            model_path: model_path.to_path_buf(),
            model_name: model_name.to_string(),
            port,
            context_length,
            gpu_layers,
            threads: Some(std::cmp::min(self.system.total_cpu_cores as u32, 8)),
            ttl_secs: *self.default_ttl_secs.read().await,
            draft_model_path: None,
        };

        // Stop any existing process for this model
        self.stop_model(model_name).await.ok();

        // Build command
        let mut cmd = Command::new(&server_path);
        cmd.arg("--model").arg(&config.model_path);
        cmd.arg("--port").arg(port.to_string());
        cmd.arg("--ctx-size").arg(config.context_length.to_string());

        // Set library path so bundled dylibs/so files are found.
        // In dev mode libs are alongside the binary. In a Tauri .app bundle
        // the sidecar is in Contents/MacOS/ but resources go to Contents/Resources/.
        if let Some(parent) = server_path.parent() {
            let mut lib_dirs = vec![parent.to_string_lossy().to_string()];
            // Also add the sibling Resources dir for macOS .app bundles
            if let Some(grandparent) = parent.parent() {
                let resources = grandparent.join("Resources");
                if resources.is_dir() {
                    lib_dirs.push(resources.to_string_lossy().to_string());
                }
            }
            let combined = lib_dirs.join(":");
            #[cfg(target_os = "macos")]
            cmd.env("DYLD_LIBRARY_PATH", &combined);
            #[cfg(target_os = "linux")]
            cmd.env("LD_LIBRARY_PATH", &combined);
        }

        if let Some(ngl) = config.gpu_layers {
            cmd.arg("-ngl").arg(ngl.to_string());
        }
        if let Some(threads) = config.threads {
            cmd.arg("--threads").arg(threads.to_string());
        }
        if let Some(ref draft_path) = config.draft_model_path {
            cmd.arg("--draft").arg(draft_path);
        }

        // Enable flash attention if GPU
        if self.system.has_gpu {
            cmd.arg("--flash-attn").arg("auto");
        }

        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        cmd.kill_on_drop(true);

        info!(model = model_name, port, context_length = config.context_length, "starting llama-server");

        let child = cmd.spawn().map_err(|e| format!("Failed to start llama-server: {}", e))?;
        let pid = child.id();

        let managed = ManagedProcess {
            config,
            process: Some(child),
            state: ProcessState::Starting,
            pid,
            last_used: std::time::Instant::now(),
        };

        self.processes
            .write()
            .await
            .insert(model_name.to_string(), managed);

        // Promote the process to Ready/Failed asynchronously so the UI can
        // show "Starting" immediately instead of blocking on startup.
        let health_url = format!("http://127.0.0.1:{}/health", port);
        let processes = Arc::clone(&self.processes);
        let model_name = model_name.to_string();
        tokio::spawn(async move {
            match wait_for_health(&health_url, 120).await {
                Ok(()) => {
                    if let Some(proc) = processes.write().await.get_mut(&model_name) {
                        proc.state = ProcessState::Ready;
                    }
                    info!(model = model_name.as_str(), port, "llama-server ready");
                }
                Err(error) => {
                    let mut processes = processes.write().await;
                    if let Some(proc) = processes.get_mut(&model_name) {
                        proc.state = ProcessState::Failed;
                        proc.pid = None;
                        if let Some(child) = proc.process.as_mut() {
                            let _ = child.kill().await;
                        }
                        proc.process = None;
                    }
                    warn!(model = model_name.as_str(), port, error = %error, "llama-server failed to become ready");
                }
            }
        });

        Ok(port)
    }

    /// Stop a running model server.
    pub async fn stop_model(&self, model_name: &str) -> Result<(), String> {
        let mut processes = self.processes.write().await;
        if let Some(mut proc) = processes.remove(model_name) {
            proc.state = ProcessState::Stopping;
            if let Some(ref mut child) = proc.process {
                info!(model = model_name, pid = ?proc.pid, "stopping llama-server");
                child.kill().await.map_err(|e| e.to_string())?;
            }
            Ok(())
        } else {
            Err(format!("Model '{}' is not running", model_name))
        }
    }

    /// Stop all running model servers.
    pub async fn stop_all(&self) {
        let mut processes = self.processes.write().await;
        for (name, proc) in processes.iter_mut() {
            if let Some(ref mut child) = proc.process {
                info!(model = name, "stopping llama-server");
                let _ = child.kill().await;
            }
        }
        processes.clear();
    }

    /// Get the status of all managed processes.
    pub async fn status(&self) -> Vec<RunningModel> {
        let mut processes = self.processes.write().await;

        for (name, proc) in processes.iter_mut() {
            if let Some(child) = proc.process.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        if proc.state != ProcessState::Stopping {
                            proc.state = ProcessState::Failed;
                            proc.pid = None;
                            proc.process = None;
                            warn!(model = name.as_str(), exit_status = %status, "llama-server exited unexpectedly");
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        proc.state = ProcessState::Failed;
                        proc.pid = None;
                        proc.process = None;
                        warn!(model = name.as_str(), error = %error, "failed to inspect llama-server process state");
                    }
                }
            }
        }

        processes
            .iter()
            .map(|(name, proc)| RunningModel {
                name: name.clone(),
                state: proc.state,
                port: proc.config.port,
                model_path: proc.config.model_path.display().to_string(),
                pid: proc.pid,
            })
            .collect()
    }

    /// Get the port for a running model (for proxy requests).
    pub async fn get_model_port(&self, model_name: &str) -> Option<u16> {
        let processes = self.processes.read().await;
        processes.get(model_name).and_then(|p| {
            if p.state == ProcessState::Ready {
                Some(p.config.port)
            } else {
                None
            }
        })
    }

    /// Check if a specific model is running and ready.
    pub async fn is_model_running(&self, model_name: &str) -> bool {
        self.get_model_port(model_name).await.is_some()
    }

    /// Mark a model as recently used (resets TTL countdown).
    pub async fn touch_model(&self, model_name: &str) {
        if let Some(proc) = self.processes.write().await.get_mut(model_name) {
            proc.last_used = std::time::Instant::now();
        }
    }

    /// Set the default TTL (seconds) for auto-unloading idle models. 0 = never.
    pub async fn set_default_ttl(&self, secs: u64) {
        *self.default_ttl_secs.write().await = secs;
    }

    /// Get the current default TTL.
    pub async fn get_default_ttl(&self) -> u64 {
        *self.default_ttl_secs.read().await
    }

    /// Run the TTL reaper once — stops models that have been idle too long.
    /// Called periodically from a background task.
    pub async fn reap_idle_models(&self) {
        let now = std::time::Instant::now();
        let mut to_stop = Vec::new();

        {
            let processes = self.processes.read().await;
            for (name, proc) in processes.iter() {
                if proc.state != ProcessState::Ready {
                    continue;
                }
                let ttl = proc.config.ttl_secs;
                if ttl == 0 {
                    continue; // no auto-unload
                }
                if now.duration_since(proc.last_used).as_secs() >= ttl {
                    to_stop.push(name.clone());
                }
            }
        }

        for name in to_stop {
            info!(model = name.as_str(), "TTL expired, auto-unloading model");
            if let Err(e) = self.stop_model(&name).await {
                warn!(model = name.as_str(), error = %e, "failed to auto-unload model");
            }
        }
    }

    /// Scan a directory for GGUF model files.
    /// Returns a list of discovered files (does NOT add them to the models dir).
    pub fn scan_directory(&self, dir: &Path) -> Vec<ScannedModel> {
        let mut found = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("gguf") {
                    let name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let size_bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    found.push(ScannedModel {
                        name,
                        path: path.clone(),
                        size_gb: size_bytes as f64 / (1024.0 * 1024.0 * 1024.0),
                        already_imported: false,
                    });
                }
            }
        }
        // Mark ones already in our models dir
        let existing: std::collections::HashSet<String> = std::fs::read_dir(&self.models_dir)
            .ok()
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|e| e.file_name().into_string().ok())
                    .collect()
            })
            .unwrap_or_default();
        for model in &mut found {
            if let Some(fname) = model.path.file_name().and_then(|f| f.to_str()) {
                model.already_imported = existing.contains(fname);
            }
        }
        found
    }

    /// Import a GGUF model by creating a symlink from the models dir to the source.
    /// This avoids duplicating large files.
    pub async fn import_model(&self, source_path: &Path) -> Result<String, String> {
        if !source_path.exists() {
            return Err(format!("File not found: {}", source_path.display()));
        }
        if source_path.extension().and_then(|e| e.to_str()) != Some("gguf") {
            return Err("Only .gguf files can be imported".to_string());
        }

        let filename = source_path
            .file_name()
            .ok_or("Invalid filename")?
            .to_str()
            .ok_or("Non-UTF8 filename")?;

        let dest = self.models_dir.join(filename);
        if dest.exists() {
            return Err(format!("Model '{}' already exists", filename));
        }

        // Create models dir if needed
        tokio::fs::create_dir_all(&self.models_dir)
            .await
            .map_err(|e| format!("Failed to create models dir: {}", e))?;

        // Symlink on Unix, copy on Windows
        #[cfg(unix)]
        {
            tokio::fs::symlink(source_path, &dest)
                .await
                .map_err(|e| format!("Failed to symlink: {}", e))?;
        }
        #[cfg(windows)]
        {
            tokio::fs::copy(source_path, &dest)
                .await
                .map_err(|e| format!("Failed to copy: {}", e))?;
        }

        let name = dest
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        info!(name = name.as_str(), path = %dest.display(), "model imported");
        Ok(name)
    }
}

/// A downloaded GGUF model file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownloadedModel {
    pub name: String,
    pub path: PathBuf,
    pub size_gb: f64,
}

/// A model discovered by scanning a directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedModel {
    pub name: String,
    pub path: PathBuf,
    pub size_gb: f64,
    pub already_imported: bool,
}

/// Status of a running model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunningModel {
    pub name: String,
    pub state: ProcessState,
    pub port: u16,
    pub model_path: String,
    pub pid: Option<u32>,
}

/// Try to find llama-server binary on the system.
fn detect_llama_server() -> Option<PathBuf> {
    // Check common locations
    let candidates = [
        "llama-server",
        "/usr/local/bin/llama-server",
        "/opt/homebrew/bin/llama-server",
    ];

    for candidate in &candidates {
        if let Ok(output) = std::process::Command::new("which")
            .arg(candidate)
            .output()
        {
            if output.status.success() {
                let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                if !path.is_empty() {
                    return Some(PathBuf::from(path));
                }
            }
        }
    }

    // Check if llama-server exists in PATH (cross-platform)
    if let Ok(output) = std::process::Command::new("llama-server")
        .arg("--version")
        .output()
    {
        if output.status.success() {
            return Some(PathBuf::from("llama-server"));
        }
    }

    None
}

fn is_valid_llama_server_path(path: &Path) -> bool {
    ensure_llama_server_path(path).is_ok()
}

fn ensure_llama_server_path(path: &Path) -> Result<(), String> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    if file_name.contains("llama-cli") {
        return Err(
            "Configured binary points to llama-cli, but Local Models requires llama-server. Install llama.cpp with llama-server or set the path manually.".to_string(),
        );
    }

    if path.components().count() > 1 && !path.exists() {
        return Err(format!("llama-server not found at {}", path.display()));
    }

    Ok(())
}

/// Wait for llama-server health endpoint to respond.
async fn wait_for_health(url: &str, timeout_secs: u64) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .map_err(|e| e.to_string())?;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout_secs);

    loop {
        if tokio::time::Instant::now() > deadline {
            return Err("health check timeout".to_string());
        }

        match client.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                // llama-server returns {"status":"ok"} when ready
                if let Ok(body) = resp.text().await {
                    if body.contains("ok") || body.contains("ready") || body.contains("no slot") {
                        return Ok(());
                    }
                    // loading state - keep waiting
                    debug!(status = body.as_str(), "llama-server loading...");
                }
            }
            Ok(resp) => {
                debug!(status = %resp.status(), "waiting for llama-server...");
            }
            Err(_) => {
                debug!("llama-server not yet reachable...");
            }
        }

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}
