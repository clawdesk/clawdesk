//! C1: System Tray with Gateway Health Indicator.
//!
//! Provides a persistent system tray icon that shows gateway health status
//! using color-coded indicators:
//!
//! - **Green** (●): All LLM providers connected, gateway healthy
//! - **Yellow** (●): Degraded — some providers unreachable or slow
//! - **Red** (●): Offline — no providers connected or gateway error
//! - **Grey** (●): Unknown — health check not yet completed
//!
//! ## Menu items
//!
//! - Show/Hide window toggle
//! - Provider status submenu (per-provider health)
//! - Quick actions: New chat, Settings
//! - Quit
//!
//! ## Architecture
//!
//! The tray runs a background health poll loop (30s interval).
//! State transitions emit `tray-health-changed` events to the frontend.
//! Uses Tauri 2.0's built-in `tauri::tray` API (requires `tray-icon` feature).

use tauri::{
    image::Image,
    menu::{CheckMenuItemBuilder, MenuBuilder, MenuItemBuilder},
    tray::{TrayIconBuilder, TrayIconEvent, MouseButton, MouseButtonState},
    Emitter, Manager,
};
use tauri_plugin_autostart::ManagerExt;
use tracing::{info, warn};
use std::sync::{Arc, RwLock};

// ═══════════════════════════════════════════════════════════════════════════
// Health status model
// ═══════════════════════════════════════════════════════════════════════════

/// Gateway health status levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum HealthStatus {
    /// All providers connected.
    Healthy,
    /// Some providers degraded.
    Degraded,
    /// Gateway offline or unreachable.
    Offline,
    /// Not yet determined.
    Unknown,
}

impl HealthStatus {
    /// Color indicator for the tray tooltip.
    pub fn indicator(&self) -> &'static str {
        match self {
            Self::Healthy => "● Healthy",
            Self::Degraded => "● Degraded",
            Self::Offline => "● Offline",
            Self::Unknown => "● Checking...",
        }
    }

    /// Tooltip text for the system tray.
    pub fn tooltip(&self) -> &'static str {
        match self {
            Self::Healthy => "ClawDesk — All systems operational",
            Self::Degraded => "ClawDesk — Some providers degraded",
            Self::Offline => "ClawDesk — Gateway offline",
            Self::Unknown => "ClawDesk — Checking status...",
        }
    }
}

impl std::fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.indicator())
    }
}

/// Per-provider health status.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProviderHealth {
    pub name: String,
    pub status: HealthStatus,
    pub latency_ms: Option<u64>,
    pub last_check: Option<chrono::DateTime<chrono::Utc>>,
    pub error: Option<String>,
}

/// Aggregate gateway health.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GatewayHealth {
    pub overall: HealthStatus,
    pub providers: Vec<ProviderHealth>,
    pub checked_at: chrono::DateTime<chrono::Utc>,
}

impl GatewayHealth {
    /// Compute overall status from provider statuses.
    pub fn compute_overall(providers: &[ProviderHealth]) -> HealthStatus {
        if providers.is_empty() {
            return HealthStatus::Unknown;
        }

        let healthy = providers
            .iter()
            .filter(|p| p.status == HealthStatus::Healthy)
            .count();
        let total = providers.len();

        if healthy == total {
            HealthStatus::Healthy
        } else if healthy > 0 {
            HealthStatus::Degraded
        } else {
            HealthStatus::Offline
        }
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tray shared state
// ═══════════════════════════════════════════════════════════════════════════

/// Shared tray state accessible from both the health poll loop and event handlers.
pub struct TrayState {
    health: RwLock<GatewayHealth>,
}

impl TrayState {
    pub fn new() -> Self {
        Self {
            health: RwLock::new(GatewayHealth {
                overall: HealthStatus::Unknown,
                providers: vec![],
                checked_at: chrono::Utc::now(),
            }),
        }
    }

    /// Get the current health status.
    pub fn health(&self) -> GatewayHealth {
        self.health
            .read()
            .map(|h| h.clone())
            .unwrap_or(GatewayHealth {
                overall: HealthStatus::Unknown,
                providers: vec![],
                checked_at: chrono::Utc::now(),
            })
    }

    /// Update the health status (called by the poll loop).
    pub fn update_health(&self, health: GatewayHealth) {
        if let Ok(mut h) = self.health.write() {
            *h = health;
        }
    }
}

impl Default for TrayState {
    fn default() -> Self {
        Self::new()
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// Tray setup
// ═══════════════════════════════════════════════════════════════════════════

/// Initialize the system tray with health indicator.
///
/// Call this from `tauri::Builder::setup()`.
///
/// # Arguments
/// * `app` - The Tauri application handle.
///
/// # Panics
/// Panics if the tray icon cannot be created (missing icon file).
pub fn setup_tray(app: &tauri::App) -> Result<(), Box<dyn std::error::Error>> {
    let tray_state = Arc::new(TrayState::new());
    app.manage(tray_state.clone());

    // Build the tray context menu
    let show_hide = MenuItemBuilder::new("Show/Hide Window")
        .id("show_hide")
        .build(app)?;

    let new_chat = MenuItemBuilder::new("New Chat")
        .id("new_chat")
        .build(app)?;

    let settings = MenuItemBuilder::new("Settings...")
        .id("settings")
        .build(app)?;

    let status_item = MenuItemBuilder::new("Status: Checking...")
        .id("status")
        .enabled(false)
        .build(app)?;

    // Auto-launch toggle — check current state from plugin
    let autostart_enabled = app
        .autolaunch()
        .is_enabled()
        .unwrap_or(false);

    let start_at_login = CheckMenuItemBuilder::new("Start at Login")
        .id("start_at_login")
        .checked(autostart_enabled)
        .build(app)?;

    let quit = MenuItemBuilder::new("Quit ClawDesk")
        .id("quit")
        .build(app)?;

    let menu = MenuBuilder::new(app)
        .item(&status_item)
        .separator()
        .item(&show_hide)
        .item(&new_chat)
        .item(&settings)
        .separator()
        .item(&start_at_login)
        .separator()
        .item(&quit)
        .build()?;

    // Load the tray icon image (44×44 template, black on transparent)
    let icon_bytes = include_bytes!("../icons/tray-icon.png");
    let icon = Image::from_bytes(icon_bytes)
        .expect("Failed to load tray icon");

    // Build the tray icon
    let _tray = TrayIconBuilder::new()
        .icon(icon)
        .icon_as_template(true)
        .menu(&menu)
        .tooltip("ClawDesk — Checking status...")
        .on_menu_event(move |app, event| {
            match event.id().as_ref() {
                "show_hide" => {
                    if let Some(window) = app.get_webview_window("main") {
                        if window.is_visible().unwrap_or(false) {
                            let _ = window.hide();
                        } else {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                }
                "new_chat" => {
                    // Emit event to frontend to create a new chat
                    let _ = app.emit("tray-new-chat", ());
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
                "settings" => {
                    let _ = app.emit("tray-open-settings", ());
                    if let Some(window) = app.get_webview_window("main") {
                        let _ = window.show();
                        let _ = window.set_focus();
                    }
                }
                "start_at_login" => {
                    let autolaunch = app.autolaunch();
                    let currently = autolaunch.is_enabled().unwrap_or(false);
                    if currently {
                        if let Err(e) = autolaunch.disable() {
                            warn!("Failed to disable autostart: {e}");
                        } else {
                            info!("Auto-launch disabled");
                        }
                    } else {
                        if let Err(e) = autolaunch.enable() {
                            warn!("Failed to enable autostart: {e}");
                        } else {
                            info!("Auto-launch enabled");
                        }
                    }
                }
                "quit" => {
                    info!("Quit requested from system tray");
                    app.exit(0);
                }
                _ => {}
            }
        })
        .on_tray_icon_event(|tray, event| {
            // Double-click (or single-click on macOS) shows window
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.set_focus();
                }
            }
        })
        .build(app)?;

    info!("System tray initialized");

    // Start the health poll loop
    start_health_poll(app.handle().clone(), tray_state);

    Ok(())
}

// ═══════════════════════════════════════════════════════════════════════════
// Health polling
// ═══════════════════════════════════════════════════════════════════════════

/// Start a background thread that polls provider health every 30 seconds.
fn start_health_poll(
    app_handle: tauri::AppHandle,
    tray_state: Arc<TrayState>,
) {
    std::thread::spawn(move || {
        // Initial delay — let the app finish startup
        std::thread::sleep(std::time::Duration::from_secs(3));

        loop {
            let health = check_provider_health();
            let previous = tray_state.health().overall;

            tray_state.update_health(health.clone());

            // Emit event to frontend if status changed
            if health.overall != previous {
                info!(
                    old = ?previous,
                    new = ?health.overall,
                    "gateway health status changed"
                );
                let _ = app_handle.emit("tray-health-changed", &health);
            }

            // Update tray tooltip
            if let Some(tray) = app_handle.tray_by_id("main") {
                let _ = tray.set_tooltip(Some(health.overall.tooltip()));
            }

            std::thread::sleep(std::time::Duration::from_secs(30));
        }
    });
}

/// Check health of all configured LLM providers.
///
/// This is a synchronous function called from the poll thread.
/// It checks each provider by verifying that the API key is set
/// (via env var or vault reference).
fn check_provider_health() -> GatewayHealth {
    let providers = vec![
        ("Anthropic", "ANTHROPIC_API_KEY"),
        ("OpenAI", "OPENAI_API_KEY"),
        ("Google AI", "GOOGLE_API_KEY"),
        ("Azure OpenAI", "AZURE_OPENAI_API_KEY"),
        ("Cohere", "COHERE_API_KEY"),
    ];

    let mut health_list = Vec::new();

    for (name, env_var) in &providers {
        let has_key = std::env::var(env_var).map(|v| !v.is_empty()).unwrap_or(false);
        let status = if has_key {
            HealthStatus::Healthy
        } else {
            HealthStatus::Offline
        };

        health_list.push(ProviderHealth {
            name: name.to_string(),
            status,
            latency_ms: None,
            last_check: Some(chrono::Utc::now()),
            error: if has_key {
                None
            } else {
                Some(format!("{} not set", env_var))
            },
        });
    }

    let overall = GatewayHealth::compute_overall(&health_list);

    GatewayHealth {
        overall,
        providers: health_list,
        checked_at: chrono::Utc::now(),
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// IPC commands for tray status
// ═══════════════════════════════════════════════════════════════════════════

/// Get current gateway health (callable from frontend).
#[tauri::command]
pub fn get_gateway_health(
    state: tauri::State<'_, Arc<TrayState>>,
) -> Result<GatewayHealth, String> {
    Ok(state.health())
}

/// Force a health check refresh.
#[tauri::command]
pub fn refresh_gateway_health(
    state: tauri::State<'_, Arc<TrayState>>,
) -> Result<GatewayHealth, String> {
    let health = check_provider_health();
    state.update_health(health.clone());
    Ok(health)
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_status_display() {
        assert_eq!(HealthStatus::Healthy.indicator(), "● Healthy");
        assert_eq!(HealthStatus::Degraded.indicator(), "● Degraded");
        assert_eq!(HealthStatus::Offline.indicator(), "● Offline");
    }

    #[test]
    fn compute_overall_all_healthy() {
        let providers = vec![
            ProviderHealth {
                name: "openai".into(),
                status: HealthStatus::Healthy,
                latency_ms: Some(50),
                last_check: None,
                error: None,
            },
            ProviderHealth {
                name: "anthropic".into(),
                status: HealthStatus::Healthy,
                latency_ms: Some(30),
                last_check: None,
                error: None,
            },
        ];
        assert_eq!(
            GatewayHealth::compute_overall(&providers),
            HealthStatus::Healthy
        );
    }

    #[test]
    fn compute_overall_some_degraded() {
        let providers = vec![
            ProviderHealth {
                name: "openai".into(),
                status: HealthStatus::Healthy,
                latency_ms: None,
                last_check: None,
                error: None,
            },
            ProviderHealth {
                name: "anthropic".into(),
                status: HealthStatus::Offline,
                latency_ms: None,
                last_check: None,
                error: Some("timeout".into()),
            },
        ];
        assert_eq!(
            GatewayHealth::compute_overall(&providers),
            HealthStatus::Degraded
        );
    }

    #[test]
    fn compute_overall_all_offline() {
        let providers = vec![ProviderHealth {
            name: "openai".into(),
            status: HealthStatus::Offline,
            latency_ms: None,
            last_check: None,
            error: Some("no key".into()),
        }];
        assert_eq!(
            GatewayHealth::compute_overall(&providers),
            HealthStatus::Offline
        );
    }

    #[test]
    fn compute_overall_empty() {
        assert_eq!(
            GatewayHealth::compute_overall(&[]),
            HealthStatus::Unknown
        );
    }

    #[test]
    fn tray_state_default() {
        let state = TrayState::new();
        assert_eq!(state.health().overall, HealthStatus::Unknown);
    }

    #[test]
    fn tray_state_update() {
        let state = TrayState::new();
        state.update_health(GatewayHealth {
            overall: HealthStatus::Healthy,
            providers: vec![],
            checked_at: chrono::Utc::now(),
        });
        assert_eq!(state.health().overall, HealthStatus::Healthy);
    }
}
