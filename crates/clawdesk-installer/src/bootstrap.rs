//! Ollama bootstrap — detect, download, and launch Ollama runtime.

use crate::{InstallConfig, InstallProgress, InstallerError};
use std::path::PathBuf;
use tokio::process::Command;
use tracing::{debug, info, warn};

/// Ollama installation state.
#[derive(Debug, Clone)]
pub enum OllamaState {
    /// Ollama is installed and running
    Running { version: String, path: PathBuf },
    /// Ollama is installed but not running
    Installed { path: PathBuf },
    /// Ollama is not installed
    NotInstalled,
}

/// Check if Ollama is available on the system.
pub async fn detect_ollama() -> OllamaState {
    // Check if 'ollama' binary exists
    let path = match which::which("ollama") {
        Ok(p) => p,
        Err(_) => {
            // Check common install locations
            let common_paths = if cfg!(target_os = "macos") {
                vec!["/usr/local/bin/ollama", "/opt/homebrew/bin/ollama"]
            } else if cfg!(target_os = "linux") {
                vec!["/usr/local/bin/ollama", "/usr/bin/ollama"]
            } else {
                vec![]
            };

            let found = common_paths.into_iter()
                .map(PathBuf::from)
                .find(|p| p.exists());

            match found {
                Some(p) => p,
                None => return OllamaState::NotInstalled,
            }
        }
    };

    // Check if Ollama is running
    match Command::new(&path)
        .args(["--version"])
        .output()
        .await
    {
        Ok(output) if output.status.success() => {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            // Try to reach the API to see if it's running
            match reqwest::Client::new()
                .get("http://127.0.0.1:11434/api/version")
                .timeout(std::time::Duration::from_secs(2))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    OllamaState::Running { version, path }
                }
                _ => OllamaState::Installed { path },
            }
        }
        _ => OllamaState::Installed { path },
    }
}

/// Start the Ollama server (background process).
pub async fn start_ollama(ollama_path: &std::path::Path) -> Result<(), InstallerError> {
    info!("starting Ollama server");

    let mut cmd = Command::new(ollama_path);
    cmd.arg("serve");

    // Detach from parent process
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    cmd.spawn()
        .map_err(|e| InstallerError::OllamaBootstrapFailed(
            format!("failed to start Ollama: {}", e)
        ))?;

    // Wait for Ollama to be ready
    let client = reqwest::Client::new();
    for i in 0..30 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if let Ok(resp) = client
            .get("http://127.0.0.1:11434/api/version")
            .timeout(std::time::Duration::from_secs(2))
            .send()
            .await
        {
            if resp.status().is_success() {
                info!(attempts = i + 1, "Ollama server ready");
                return Ok(());
            }
        }
    }

    Err(InstallerError::OllamaBootstrapFailed(
        "Ollama server did not become ready within 15 seconds".into()
    ))
}

/// Install Ollama on the system (platform-specific).
pub async fn install_ollama<F>(progress: F) -> Result<PathBuf, InstallerError>
where
    F: Fn(InstallProgress) + Send + 'static,
{
    progress(InstallProgress::DownloadingOllama { progress_pct: 0.0 });

    #[cfg(target_os = "macos")]
    {
        install_ollama_macos(&progress).await
    }

    #[cfg(target_os = "linux")]
    {
        install_ollama_linux(&progress).await
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Err(InstallerError::UnsupportedPlatform(
            std::env::consts::OS.to_string()
        ))
    }
}

#[cfg(target_os = "macos")]
async fn install_ollama_macos<F>(progress: &F) -> Result<PathBuf, InstallerError>
where
    F: Fn(InstallProgress),
{
    // Use the official install script
    let output = Command::new("curl")
        .args(["-fsSL", "https://ollama.ai/install.sh"])
        .output()
        .await
        .map_err(|e| InstallerError::OllamaBootstrapFailed(format!("curl failed: {}", e)))?;

    if !output.status.success() {
        return Err(InstallerError::OllamaBootstrapFailed(
            "failed to download Ollama install script".into()
        ));
    }

    let script = String::from_utf8_lossy(&output.stdout);
    let install = Command::new("sh")
        .arg("-c")
        .arg(script.as_ref())
        .output()
        .await
        .map_err(|e| InstallerError::OllamaBootstrapFailed(format!("install failed: {}", e)))?;

    progress(InstallProgress::DownloadingOllama { progress_pct: 100.0 });

    if !install.status.success() {
        let stderr = String::from_utf8_lossy(&install.stderr);
        return Err(InstallerError::OllamaBootstrapFailed(
            format!("Ollama install failed: {}", stderr)
        ));
    }

    which::which("ollama")
        .map_err(|_| InstallerError::OllamaBootstrapFailed(
            "ollama not found after installation".into()
        ))
}

#[cfg(target_os = "linux")]
async fn install_ollama_linux<F>(progress: &F) -> Result<PathBuf, InstallerError>
where
    F: Fn(InstallProgress),
{
    let output = Command::new("curl")
        .args(["-fsSL", "https://ollama.ai/install.sh"])
        .output()
        .await
        .map_err(|e| InstallerError::OllamaBootstrapFailed(format!("curl failed: {}", e)))?;

    if !output.status.success() {
        return Err(InstallerError::OllamaBootstrapFailed(
            "failed to download Ollama install script".into()
        ));
    }

    let script = String::from_utf8_lossy(&output.stdout);
    let install = Command::new("sh")
        .arg("-c")
        .arg(script.as_ref())
        .output()
        .await
        .map_err(|e| InstallerError::OllamaBootstrapFailed(format!("install failed: {}", e)))?;

    progress(InstallProgress::DownloadingOllama { progress_pct: 100.0 });

    if !install.status.success() {
        let stderr = String::from_utf8_lossy(&install.stderr);
        return Err(InstallerError::OllamaBootstrapFailed(
            format!("Ollama install failed: {}", stderr)
        ));
    }

    which::which("ollama")
        .map_err(|_| InstallerError::OllamaBootstrapFailed(
            "ollama not found after installation".into()
        ))
}

/// Pull a model via Ollama API.
pub async fn pull_model<F>(model_name: &str, progress: F) -> Result<(), InstallerError>
where
    F: Fn(InstallProgress) + Send + 'static,
{
    info!(model = model_name, "pulling model via Ollama");
    progress(InstallProgress::DownloadingModel {
        model_name: model_name.to_string(),
        size_mb: 0,
        progress_pct: 0.0,
    });

    let client = reqwest::Client::new();
    let resp = client
        .post("http://127.0.0.1:11434/api/pull")
        .json(&serde_json::json!({ "name": model_name, "stream": false }))
        .timeout(std::time::Duration::from_secs(3600)) // 1 hour max
        .send()
        .await
        .map_err(|e| InstallerError::ModelDownloadFailed(format!("pull request failed: {}", e)))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(InstallerError::ModelDownloadFailed(
            format!("Ollama pull failed ({}): {}", status, body)
        ));
    }

    progress(InstallProgress::DownloadingModel {
        model_name: model_name.to_string(),
        size_mb: 0,
        progress_pct: 100.0,
    });

    info!(model = model_name, "model pulled successfully");
    Ok(())
}

/// Run a test inference to verify the model works.
pub async fn test_inference(model_name: &str) -> Result<String, InstallerError> {
    debug!(model = model_name, "testing inference");

    let client = reqwest::Client::new();
    let resp = client
        .post("http://127.0.0.1:11434/api/generate")
        .json(&serde_json::json!({
            "model": model_name,
            "prompt": "Say hello in one sentence.",
            "stream": false,
        }))
        .timeout(std::time::Duration::from_secs(120))
        .send()
        .await
        .map_err(|e| InstallerError::ModelDownloadFailed(
            format!("inference test failed: {}", e)
        ))?;

    if !resp.status().is_success() {
        return Err(InstallerError::ModelDownloadFailed(
            "inference test returned non-200".into()
        ));
    }

    let body: serde_json::Value = resp.json().await
        .map_err(|e| InstallerError::ModelDownloadFailed(
            format!("failed to parse inference response: {}", e)
        ))?;

    let response = body["response"].as_str().unwrap_or("").to_string();
    info!(model = model_name, response_len = response.len(), "inference test passed");

    Ok(response)
}
