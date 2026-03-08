//! GGUF model download manager.
//!
//! Downloads GGUF model files from HuggingFace with progress tracking.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::sync::mpsc;
use tracing::info;

/// Progress event during model download.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DownloadEvent {
    Progress {
        model_name: String,
        downloaded_bytes: u64,
        total_bytes: u64,
        percent: f64,
    },
    Done {
        model_name: String,
        path: String,
        size_gb: f64,
    },
    Error {
        model_name: String,
        message: String,
    },
}

/// Download a GGUF model file from HuggingFace.
///
/// The URL should be a direct link like:
/// `https://huggingface.co/{repo}/resolve/main/{filename}`
pub async fn download_model(
    url: &str,
    dest_dir: &Path,
    model_name: &str,
    event_tx: mpsc::Sender<DownloadEvent>,
) -> Result<PathBuf, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(7200)) // 2 hours for large models
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;

    info!(model = model_name, url, "starting model download");

    let response = client
        .get(url)
        .header("User-Agent", "clawdesk/0.1")
        .send()
        .await
        .map_err(|e| format!("Download failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        return Err(format!("Download failed with HTTP {}", status));
    }

    let total_bytes = response.content_length().unwrap_or(0);

    // Extract filename from URL
    let filename = url
        .rsplit('/')
        .next()
        .unwrap_or("model.gguf")
        .to_string();

    let dest_path = dest_dir.join(&filename);

    // Create dest dir if needed
    tokio::fs::create_dir_all(dest_dir)
        .await
        .map_err(|e| format!("Failed to create directory: {}", e))?;

    let mut file = tokio::fs::File::create(&dest_path)
        .await
        .map_err(|e| format!("Failed to create file: {}", e))?;

    let mut downloaded: u64 = 0;
    let mut response = response;
    let mut last_report = std::time::Instant::now();

    use tokio::io::AsyncWriteExt;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("Download interrupted: {}", e))?
    {
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("Write error: {}", e))?;

        downloaded += chunk.len() as u64;

        // Report progress at most every 500ms
        if last_report.elapsed() >= std::time::Duration::from_millis(500) || downloaded >= total_bytes
        {
            let percent = if total_bytes > 0 {
                (downloaded as f64 / total_bytes as f64 * 100.0).min(100.0)
            } else {
                0.0
            };

            let _ = event_tx
                .send(DownloadEvent::Progress {
                    model_name: model_name.to_string(),
                    downloaded_bytes: downloaded,
                    total_bytes,
                    percent,
                })
                .await;

            last_report = std::time::Instant::now();
        }
    }

    file.flush().await.map_err(|e| format!("Flush error: {}", e))?;

    let size_gb = downloaded as f64 / (1024.0 * 1024.0 * 1024.0);
    info!(model = model_name, size_gb, "download complete");

    let _ = event_tx
        .send(DownloadEvent::Done {
            model_name: model_name.to_string(),
            path: dest_path.display().to_string(),
            size_gb,
        })
        .await;

    Ok(dest_path)
}

/// Delete a downloaded model file.
pub async fn delete_model(path: &Path) -> Result<(), String> {
    tokio::fs::remove_file(path)
        .await
        .map_err(|e| format!("Failed to delete model: {}", e))
}
