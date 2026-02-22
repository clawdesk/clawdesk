//! Media processing commands — audio, image, document, link preview, TTS.

use crate::state::AppState;
use serde::Serialize;
use tauri::State;

#[derive(Debug, Serialize)]
pub struct LinkPreviewResult {
    pub url: String,
    pub title: Option<String>,
    pub description: Option<String>,
    pub image: Option<String>,
    pub site_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MediaPipelineStatus {
    pub processor_count: usize,
    pub processors: Vec<String>,
}

/// Get the status of the media processing pipeline.
#[tauri::command]
pub async fn get_media_pipeline_status(
    state: State<'_, AppState>,
) -> Result<MediaPipelineStatus, String> {
    let pipeline = state.media_pipeline.read().await;
    let processors: Vec<String> = pipeline.processors().iter()
        .map(|p| p.name().to_string())
        .collect();
    Ok(MediaPipelineStatus {
        processor_count: processors.len(),
        processors,
    })
}

/// Get a rich preview for a URL.
///
/// Note: Requires an HTTP fetcher to be configured. Currently returns
/// extracted URL metadata placeholder until a production fetcher is wired.
#[tauri::command]
pub async fn get_link_preview(url: String) -> Result<LinkPreviewResult, String> {
    // LinkUnderstanding requires an Arc<dyn HttpFetcher> which needs a production
    // HTTP client. For now, return the URL as-is with empty metadata.
    // In production, wire a reqwest-based HttpFetcher implementation.
    Ok(LinkPreviewResult {
        url,
        title: None,
        description: None,
        image: None,
        site_name: None,
    })
}
