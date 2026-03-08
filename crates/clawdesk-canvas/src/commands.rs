//! Canvas commands — lifecycle management + canvas operations.
//!
//! Manages a single agent-controlled canvas surface with these operations:
//! - `present` — Show canvas WebView with optional placement
//! - `hide` — Hide the canvas WebView
//! - `navigate` — Navigate to URL in the canvas
//! - `eval` — Execute JavaScript in the canvas WebView
//! - `snapshot` — Capture screenshot of the canvas
//! - `a2ui_push` — Push JSONL to the A2UI rendering surface
//! - `a2ui_reset` — Clear the A2UI surface

use crate::a2ui::{A2UIMessage, ComponentTree};
use crate::capability::CapabilityStore;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, warn};

// ═══════════════════════════════════════════════════════════════
// Canvas state
// ═══════════════════════════════════════════════════════════════

/// Per-agent canvas state.
#[derive(Debug)]
pub struct CanvasState {
    /// Whether the canvas WebView is currently visible.
    pub visible: bool,
    /// Current URL loaded in the canvas.
    pub current_url: Option<String>,
    /// Placement (x, y, width, height).
    pub placement: CanvasPlacement,
    /// A2UI component trees per surface.
    pub surfaces: DashMap<String, ComponentTree>,
    /// Pending eval results (indexed by request id).
    pub eval_results: DashMap<String, String>,
    /// Last snapshot (base64 PNG).
    pub last_snapshot: RwLock<Option<CanvasSnapshot>>,
}

impl CanvasState {
    pub fn new() -> Self {
        Self {
            visible: false,
            current_url: None,
            placement: CanvasPlacement::default(),
            surfaces: DashMap::new(),
            eval_results: DashMap::new(),
            last_snapshot: RwLock::new(None),
        }
    }
}

impl Default for CanvasState {
    fn default() -> Self {
        Self::new()
    }
}

/// Canvas placement on screen.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CanvasPlacement {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Default for CanvasPlacement {
    fn default() -> Self {
        Self {
            x: 100.0,
            y: 100.0,
            width: 800.0,
            height: 600.0,
        }
    }
}

/// Canvas snapshot result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CanvasSnapshot {
    pub format: String,
    pub base64: String,
    pub width: u32,
    pub height: u32,
    pub timestamp: String,
}

// ═══════════════════════════════════════════════════════════════
// Canvas command types
// ═══════════════════════════════════════════════════════════════

/// Canvas commands that agents can dispatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub enum CanvasCommand {
    /// Show the canvas WebView.
    Present {
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        x: Option<f64>,
        #[serde(default)]
        y: Option<f64>,
        #[serde(default)]
        width: Option<f64>,
        #[serde(default)]
        height: Option<f64>,
    },
    /// Hide the canvas WebView.
    Hide,
    /// Navigate to a URL in the canvas.
    Navigate { url: String },
    /// Execute JavaScript in the canvas WebView.
    Eval {
        javascript: String,
        #[serde(default = "default_timeout_ms")]
        timeout_ms: u64,
    },
    /// Capture a screenshot of the canvas.
    Snapshot {
        #[serde(default = "default_format")]
        format: String,
        #[serde(default)]
        max_width: Option<u32>,
        #[serde(default)]
        quality: Option<f64>,
    },
    /// Push JSONL to the A2UI surface.
    A2uiPush { jsonl: String },
    /// Reset (clear) the A2UI surface.
    A2uiReset {
        #[serde(default)]
        surface_id: Option<String>,
    },
}

fn default_timeout_ms() -> u64 {
    5000
}

fn default_format() -> String {
    "png".into()
}

/// Result of a canvas command execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CanvasCommandResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl CanvasCommandResult {
    pub fn success(result: Option<serde_json::Value>) -> Self {
        Self {
            ok: true,
            result,
            error: None,
        }
    }

    pub fn error(msg: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(msg.into()),
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Canvas manager
// ═══════════════════════════════════════════════════════════════

/// Callback trait for the Tauri layer to implement canvas operations.
///
/// The canvas manager calls these to actually show/hide/eval/screenshot
/// the WebView. The Tauri layer implements this trait.
#[async_trait::async_trait]
pub trait CanvasBackend: Send + Sync {
    /// Show the canvas WebView at the given position.
    async fn present(&self, url: &str, placement: CanvasPlacement) -> Result<(), String>;
    /// Hide the canvas WebView.
    async fn hide(&self) -> Result<(), String>;
    /// Navigate to a URL.
    async fn navigate(&self, url: &str) -> Result<(), String>;
    /// Execute JavaScript and return the result.
    async fn eval_js(&self, js: &str, timeout_ms: u64) -> Result<String, String>;
    /// Capture a screenshot.
    async fn snapshot(
        &self,
        format: &str,
        max_width: Option<u32>,
        quality: Option<f64>,
    ) -> Result<CanvasSnapshot, String>;
    /// Send A2UI state to the WebView for rendering.
    async fn push_a2ui_state(&self, state_json: &str) -> Result<(), String>;
}

/// Canvas manager — orchestrates canvas commands.
pub struct CanvasManager {
    /// Per-agent canvas state.
    states: DashMap<String, Arc<CanvasState>>,
    /// Capability token store.
    capabilities: CapabilityStore,
    /// Backend (Tauri WebView operations).
    backend: Option<Arc<dyn CanvasBackend>>,
    /// Canvas host URL.
    host_url: String,
}

impl CanvasManager {
    /// Create a new canvas manager.
    pub fn new(host_url: String) -> Self {
        let capabilities = CapabilityStore::new(host_url.clone());
        Self {
            states: DashMap::new(),
            capabilities,
            backend: None,
            host_url,
        }
    }

    /// Set the backend for WebView operations.
    pub fn set_backend(&mut self, backend: Arc<dyn CanvasBackend>) {
        self.backend = Some(backend);
    }

    /// Get or create state for an agent.
    pub fn get_state(&self, agent_id: &str) -> Arc<CanvasState> {
        self.states
            .entry(agent_id.to_owned())
            .or_insert_with(|| Arc::new(CanvasState::new()))
            .clone()
    }

    /// Get the capability store.
    pub fn capabilities(&self) -> &CapabilityStore {
        &self.capabilities
    }

    /// Execute a canvas command for an agent.
    pub async fn execute(
        &self,
        agent_id: &str,
        command: CanvasCommand,
    ) -> CanvasCommandResult {
        let state = self.get_state(agent_id);
        let backend = match &self.backend {
            Some(b) => b.clone(),
            None => return CanvasCommandResult::error("canvas backend not configured"),
        };

        match command {
            CanvasCommand::Present {
                url,
                x,
                y,
                width,
                height,
            } => {
                let mut placement = state.placement;
                if let Some(x) = x {
                    placement.x = x;
                }
                if let Some(y) = y {
                    placement.y = y;
                }
                if let Some(w) = width {
                    placement.width = w;
                }
                if let Some(h) = height {
                    placement.height = h;
                }

                let url = url.unwrap_or_else(|| {
                    format!("{}/__clawdesk__/a2ui/", self.host_url)
                });

                match backend.present(&url, placement).await {
                    Ok(()) => {
                        debug!(agent_id, url, "canvas presented");
                        CanvasCommandResult::success(Some(serde_json::json!({
                            "visible": true,
                            "url": url,
                        })))
                    }
                    Err(e) => CanvasCommandResult::error(e),
                }
            }

            CanvasCommand::Hide => match backend.hide().await {
                Ok(()) => {
                    debug!(agent_id, "canvas hidden");
                    CanvasCommandResult::success(None)
                }
                Err(e) => CanvasCommandResult::error(e),
            },

            CanvasCommand::Navigate { url } => match backend.navigate(&url).await {
                Ok(()) => {
                    debug!(agent_id, url, "canvas navigated");
                    CanvasCommandResult::success(Some(serde_json::json!({
                        "url": url,
                    })))
                }
                Err(e) => CanvasCommandResult::error(e),
            },

            CanvasCommand::Eval {
                javascript,
                timeout_ms,
            } => match backend.eval_js(&javascript, timeout_ms).await {
                Ok(result) => CanvasCommandResult::success(Some(serde_json::json!({
                    "result": result,
                }))),
                Err(e) => CanvasCommandResult::error(e),
            },

            CanvasCommand::Snapshot {
                format,
                max_width,
                quality,
            } => match backend.snapshot(&format, max_width, quality).await {
                Ok(snap) => {
                    let result = serde_json::json!({
                        "format": snap.format,
                        "base64": snap.base64,
                        "width": snap.width,
                        "height": snap.height,
                    });
                    // Store last snapshot
                    let mut last = state.last_snapshot.write().await;
                    *last = Some(snap);
                    CanvasCommandResult::success(Some(result))
                }
                Err(e) => CanvasCommandResult::error(e),
            },

            CanvasCommand::A2uiPush { jsonl } => {
                match A2UIMessage::parse_jsonl(&jsonl) {
                    Ok(messages) => {
                        let mut applied = 0;
                        for msg in &messages {
                            match msg {
                                A2UIMessage::CreateSurface(cs) => {
                                    if !state.surfaces.contains_key(&cs.surface_id) {
                                        state
                                            .surfaces
                                            .insert(cs.surface_id.clone(), ComponentTree::new());
                                    }
                                    applied += 1;
                                }
                                A2UIMessage::SurfaceUpdate(su) => {
                                    if let Some(mut tree) = state.surfaces.get_mut(&su.surface_id) {
                                        tree.apply_update(su);
                                        applied += 1;
                                    } else {
                                        warn!(
                                            surface_id = su.surface_id,
                                            "surface not found, creating implicitly"
                                        );
                                        let mut tree = ComponentTree::new();
                                        tree.apply_update(su);
                                        state.surfaces.insert(su.surface_id.clone(), tree);
                                        applied += 1;
                                    }
                                }
                                A2UIMessage::BeginRendering(br) => {
                                    if let Some(mut tree) = state.surfaces.get_mut(&br.surface_id) {
                                        tree.set_root(br.root.clone());
                                        applied += 1;
                                    }
                                }
                                A2UIMessage::DeleteSurface(ds) => {
                                    state.surfaces.remove(&ds.surface_id);
                                    applied += 1;
                                }
                                A2UIMessage::DataModelUpdate(dm) => {
                                    if let Some(mut tree) = state.surfaces.get_mut(&dm.surface_id) {
                                        tree.update_data(dm.data.clone());
                                        applied += 1;
                                    }
                                }
                            }
                        }

                        // Push merged state to renderer
                        let surfaces_json = self.build_surfaces_json(&state);
                        if let Err(e) = backend.push_a2ui_state(&surfaces_json).await {
                            warn!(error = %e, "failed to push A2UI state to renderer");
                        }

                        CanvasCommandResult::success(Some(serde_json::json!({
                            "messages_processed": messages.len(),
                            "components_applied": applied,
                        })))
                    }
                    Err(e) => CanvasCommandResult::error(format!("JSONL parse error: {e}")),
                }
            }

            CanvasCommand::A2uiReset { surface_id } => {
                if let Some(sid) = surface_id {
                    if let Some(mut tree) = state.surfaces.get_mut(&sid) {
                        tree.reset();
                    }
                } else {
                    state.surfaces.clear();
                }

                // Push empty state to renderer
                let surfaces_json = self.build_surfaces_json(&state);
                if let Err(e) = backend.push_a2ui_state(&surfaces_json).await {
                    warn!(error = %e, "failed to push A2UI reset to renderer");
                }

                CanvasCommandResult::success(None)
            }
        }
    }

    /// Build a JSON representation of all surfaces for the renderer.
    fn build_surfaces_json(&self, state: &CanvasState) -> String {
        let mut surfaces = serde_json::Map::new();
        for entry in state.surfaces.iter() {
            surfaces.insert(
                entry.key().clone(),
                entry.value().to_renderer_json(),
            );
        }
        serde_json::to_string(&serde_json::Value::Object(surfaces)).unwrap_or_default()
    }
}

// ═══════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_state_defaults() {
        let state = CanvasState::new();
        assert!(!state.visible);
        assert!(state.current_url.is_none());
        assert!(state.surfaces.is_empty());
    }

    #[test]
    fn canvas_command_result_success() {
        let r = CanvasCommandResult::success(Some(serde_json::json!({"ok": true})));
        assert!(r.ok);
        assert!(r.error.is_none());
    }

    #[test]
    fn canvas_command_result_error() {
        let r = CanvasCommandResult::error("test error");
        assert!(!r.ok);
        assert_eq!(r.error, Some("test error".into()));
    }

    #[test]
    fn canvas_manager_state_creation() {
        let mgr = CanvasManager::new("http://localhost:9000".into());
        let state = mgr.get_state("agent-1");
        assert!(!state.visible);

        // Same agent gets the same state
        let state2 = mgr.get_state("agent-1");
        assert!(Arc::ptr_eq(&state, &state2));
    }
}
