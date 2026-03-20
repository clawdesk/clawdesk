//! Wizard flow — DAG-based adaptive onboarding with 3 visible + background steps.
//!
//! ## Design
//!
//! The wizard presents 3 user-facing steps backed by a conditional DAG.
//! Technical setup runs as background tasks with safe defaults:
//!
//! - Sandbox: ON (per capability algebra)
//! - Model: local Ollama (per installer bootstrap)
//! - Gateway: localhost
//! - Credentials: auto-generated internal
//!
//! Visible: Personalization → Connection → Confirmation
//! Background: SecuritySetup, ConfigSetup, GatewaySetup, ModelSetup, SystemCheck
//!
//! Onboarding completion follows exponential decay: `P(complete) = P₀ × rⁿ`
//! At 3 visible steps with r=0.92: P ≈ 0.74 (74%) vs. 3.2% at 8 steps.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Categorization of wizard steps into visible (user-facing) and background.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepVisibility {
    /// Shown to the user with UI
    Visible,
    /// Runs in background with safe defaults, no user interaction
    Background,
}

/// Onboarding wizard steps — 3 visible + 5 background.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WizardStep {
    // === Visible steps (user-facing) ===
    /// Step 1: Name, avatar, use-case selection via checkboxes
    Personalization,
    /// Step 2: Optional channel pairing with visual QR/OAuth flows
    Connection,
    /// Step 3: "Your assistant is ready, say hi."
    Confirmation,

    // === Background steps (safe defaults, no user interaction) ===
    /// Enable sandbox-by-default (CapabilitySet::EMPTY)
    SecuritySetup,
    /// Config validation with sensible defaults
    ConfigSetup,
    /// Localhost gateway with auto-generated internal credentials
    GatewaySetup,
    /// Local model detection and download
    ModelSetup,
    /// System health verification
    SystemCheck,

    /// Terminal state
    Complete,
}

impl WizardStep {
    /// Whether this step is visible to the user.
    pub fn visibility(self) -> StepVisibility {
        match self {
            Self::Personalization | Self::Connection | Self::Confirmation => StepVisibility::Visible,
            Self::SecuritySetup | Self::ConfigSetup | Self::GatewaySetup
            | Self::ModelSetup | Self::SystemCheck => StepVisibility::Background,
            Self::Complete => StepVisibility::Background,
        }
    }

    /// Next visible step (skips background steps in the user flow).
    pub fn next_visible(self) -> Option<Self> {
        match self {
            Self::Personalization => Some(Self::Connection),
            Self::Connection => Some(Self::Confirmation),
            Self::Confirmation => Some(Self::Complete),
            _ => None,
        }
    }

    /// Background steps that can run during transition FROM this visible step.
    /// Uses conditional DAG edges — steps can execute concurrently.
    pub fn background_tasks(self) -> &'static [WizardStep] {
        match self {
            // After Personalization: start security + config + model setup
            Self::Personalization => &[
                WizardStep::SecuritySetup,
                WizardStep::ConfigSetup,
                WizardStep::ModelSetup,
            ],
            // After Connection: gateway setup + system check
            Self::Connection => &[
                WizardStep::GatewaySetup,
                WizardStep::SystemCheck,
            ],
            _ => &[],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Personalization => "Personalization",
            Self::Connection => "Connect",
            Self::Confirmation => "Ready!",
            Self::SecuritySetup => "Security Setup",
            Self::ConfigSetup => "Config Setup",
            Self::GatewaySetup => "Gateway Setup",
            Self::ModelSetup => "AI Model Setup",
            Self::SystemCheck => "System Check",
            Self::Complete => "Complete",
        }
    }

    /// User-facing description for visual wizard.
    pub fn description(self) -> &'static str {
        match self {
            Self::Personalization => "Tell us about yourself and how you'll use your assistant",
            Self::Connection => "Connect your favorite apps and channels (optional)",
            Self::Confirmation => "Your assistant is ready — say hi!",
            Self::SecuritySetup => "Enabling secure sandbox environment",
            Self::ConfigSetup => "Setting up configuration",
            Self::GatewaySetup => "Configuring local gateway",
            Self::ModelSetup => "Preparing AI model",
            Self::SystemCheck => "Verifying system health",
            Self::Complete => "Setup complete",
        }
    }

    /// All visible steps in order.
    pub fn visible_steps() -> &'static [WizardStep] {
        &[
            WizardStep::Personalization,
            WizardStep::Connection,
            WizardStep::Confirmation,
        ]
    }

    /// All background steps.
    pub fn background_steps() -> &'static [WizardStep] {
        &[
            WizardStep::SecuritySetup,
            WizardStep::ConfigSetup,
            WizardStep::GatewaySetup,
            WizardStep::ModelSetup,
            WizardStep::SystemCheck,
        ]
    }
}

/// Use-case categories for personalization step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UseCase {
    Coding,
    Writing,
    Research,
    DataAnalysis,
    Automation,
    Communication,
    Creative,
    Education,
    Business,
    Personal,
}

impl UseCase {
    pub fn label(self) -> &'static str {
        match self {
            Self::Coding => "Coding & Development",
            Self::Writing => "Writing & Content",
            Self::Research => "Research & Analysis",
            Self::DataAnalysis => "Data & Analytics",
            Self::Automation => "Automation & Workflows",
            Self::Communication => "Communication & Email",
            Self::Creative => "Creative & Design",
            Self::Education => "Learning & Tutoring",
            Self::Business => "Business & Planning",
            Self::Personal => "Personal Assistant",
        }
    }

    pub fn all() -> &'static [UseCase] {
        &[
            Self::Coding, Self::Writing, Self::Research, Self::DataAnalysis,
            Self::Automation, Self::Communication, Self::Creative,
            Self::Education, Self::Business, Self::Personal,
        ]
    }
}

/// Status of a background task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundTaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Skipped,
}

/// Wizard session state — serializable for resumability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WizardState {
    pub current_step: WizardStep,
    pub completed_steps: Vec<WizardStep>,
    pub accumulated_config: HashMap<String, serde_json::Value>,
    /// Background task status tracking
    pub background_status: HashMap<String, BackgroundTaskStatus>,
    /// User personalization data
    pub user_name: Option<String>,
    pub user_avatar: Option<String>,
    pub selected_use_cases: Vec<UseCase>,
    /// Connection data
    pub channels_paired: Vec<String>,
}

impl Default for WizardState {
    fn default() -> Self {
        Self {
            current_step: WizardStep::Personalization,
            completed_steps: Vec::new(),
            accumulated_config: HashMap::new(),
            background_status: HashMap::new(),
            user_name: None,
            user_avatar: None,
            selected_use_cases: Vec::new(),
            channels_paired: Vec::new(),
        }
    }
}

impl WizardState {
    /// Advance to the next visible step, launching background tasks.
    pub fn advance(&mut self) -> Result<Vec<WizardStep>, String> {
        let next = self.current_step.next_visible()
            .ok_or("wizard is already complete")?;

        // Collect background tasks to launch during this transition
        let bg_tasks: Vec<WizardStep> = self.current_step.background_tasks().to_vec();

        // Mark background tasks as pending
        for task in &bg_tasks {
            self.background_status.insert(
                format!("{:?}", task),
                BackgroundTaskStatus::Pending,
            );
        }

        self.completed_steps.push(self.current_step);
        self.current_step = next;

        Ok(bg_tasks)
    }

    pub fn is_complete(&self) -> bool {
        self.current_step == WizardStep::Complete
    }

    /// Update status of a background task.
    pub fn update_background_task(&mut self, step: WizardStep, status: BackgroundTaskStatus) {
        self.background_status.insert(format!("{:?}", step), status);
    }

    /// Check if all background tasks are done (completed or skipped).
    pub fn all_background_done(&self) -> bool {
        self.background_status.values().all(|s| {
            matches!(s, BackgroundTaskStatus::Completed | BackgroundTaskStatus::Skipped)
        })
    }

    /// Store a config value.
    pub fn set_config(&mut self, key: &str, value: serde_json::Value) {
        self.accumulated_config.insert(key.to_string(), value);
    }

    /// Set personalization data.
    pub fn set_personalization(&mut self, name: Option<String>, avatar: Option<String>, use_cases: Vec<UseCase>) {
        self.user_name = name;
        self.user_avatar = avatar;
        self.selected_use_cases = use_cases;
    }

    /// Number of visible steps completed.
    pub fn visible_steps_completed(&self) -> usize {
        self.completed_steps.iter()
            .filter(|s| s.visibility() == StepVisibility::Visible)
            .count()
    }
}

/// Result of executing a wizard step.
#[derive(Debug, Clone)]
pub enum StepResult {
    /// Step completed, advance to next.
    Continue,
    /// Step requires user input (display prompt).
    NeedInput { prompt: String },
    /// Step failed with an error.
    Error { message: String },
    /// User requested to go back.
    Back,
    /// User requested to abort.
    Abort,
    /// Background tasks launched (returned after advance).
    BackgroundLaunched { tasks: Vec<WizardStep> },
}

/// The wizard flow controller.
pub struct WizardFlow {
    pub state: WizardState,
}

impl WizardFlow {
    pub fn new() -> Self {
        Self { state: WizardState::default() }
    }

    /// Resume from a saved state.
    pub fn resume(state: WizardState) -> Self {
        Self { state }
    }

    /// Serialize state for persistence.
    pub fn save(&self) -> String {
        serde_json::to_string_pretty(&self.state)
            .unwrap_or_default()
    }

    /// Load state from persistence.
    pub fn load(json: &str) -> Result<Self, serde_json::Error> {
        let state: WizardState = serde_json::from_str(json)?;
        Ok(Self { state })
    }

    /// Progress percentage (based on visible steps only).
    pub fn progress(&self) -> f32 {
        let total = WizardStep::visible_steps().len() as f32;
        self.state.visible_steps_completed() as f32 / total * 100.0
    }

    /// Estimated completion probability using exponential decay model.
    ///
    /// `P(complete) = P₀ × rⁿ` where:
    /// - `P₀` = 0.95 (initial intent)
    /// - `r` = 0.92 (per-step retention for consumer apps)
    /// - `n` = remaining visible steps
    pub fn estimated_completion_probability(&self) -> f64 {
        let remaining = WizardStep::visible_steps().len() - self.state.visible_steps_completed();
        0.95 * 0.92_f64.powi(remaining as i32)
    }

    /// Default config values applied during background setup.
    pub fn default_config() -> HashMap<String, serde_json::Value> {
        let mut config = HashMap::new();
        config.insert("sandbox.enabled".into(), serde_json::json!(true));
        config.insert("sandbox.default_grant".into(), serde_json::json!("empty"));
        config.insert("gateway.host".into(), serde_json::json!("127.0.0.1"));
        config.insert("gateway.port".into(), serde_json::json!(18789));
        config.insert("model.source".into(), serde_json::json!("local_ollama"));
        config.insert("model.auto_download".into(), serde_json::json!(true));
        config.insert("security.auto_generated_credentials".into(), serde_json::json!(true));
        config
    }
}

impl Default for WizardFlow {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wizard_starts_at_personalization() {
        let flow = WizardFlow::new();
        assert_eq!(flow.state.current_step, WizardStep::Personalization);
    }

    #[test]
    fn wizard_has_3_visible_steps() {
        assert_eq!(WizardStep::visible_steps().len(), 3);
    }

    #[test]
    fn wizard_advances_through_visible_steps() {
        let mut flow = WizardFlow::new();
        assert_eq!(flow.state.current_step, WizardStep::Personalization);

        let bg = flow.state.advance().unwrap();
        assert!(!bg.is_empty()); // Background tasks launched
        assert_eq!(flow.state.current_step, WizardStep::Connection);

        let bg = flow.state.advance().unwrap();
        assert!(!bg.is_empty());
        assert_eq!(flow.state.current_step, WizardStep::Confirmation);

        let bg = flow.state.advance().unwrap();
        assert!(bg.is_empty()); // No background tasks for last step
        assert_eq!(flow.state.current_step, WizardStep::Complete);
        assert!(flow.state.is_complete());
    }

    #[test]
    fn wizard_serializable() {
        let mut flow = WizardFlow::new();
        flow.state.set_config("provider", serde_json::json!("anthropic"));
        flow.state.set_personalization(
            Some("Alice".into()),
            None,
            vec![UseCase::Coding, UseCase::Research],
        );
        let json = flow.save();
        let restored = WizardFlow::load(&json).unwrap();
        assert_eq!(restored.state.current_step, WizardStep::Personalization);
        assert_eq!(restored.state.user_name, Some("Alice".into()));
    }

    #[test]
    fn progress_percentage() {
        let mut flow = WizardFlow::new();
        assert_eq!(flow.progress(), 0.0);

        flow.state.advance().unwrap();
        // Personalization completed = 1/3
        let pct = flow.progress();
        assert!((pct - 33.333).abs() < 1.0);
    }

    #[test]
    fn background_tasks_per_visible_step() {
        // Personalization triggers 3 background tasks
        let bg = WizardStep::Personalization.background_tasks();
        assert_eq!(bg.len(), 3);
        assert!(bg.contains(&WizardStep::SecuritySetup));
        assert!(bg.contains(&WizardStep::ConfigSetup));
        assert!(bg.contains(&WizardStep::ModelSetup));

        // Connection triggers 2 background tasks
        let bg = WizardStep::Connection.background_tasks();
        assert_eq!(bg.len(), 2);
        assert!(bg.contains(&WizardStep::GatewaySetup));
        assert!(bg.contains(&WizardStep::SystemCheck));
    }

    #[test]
    fn estimated_completion_at_start() {
        let flow = WizardFlow::new();
        let prob = flow.estimated_completion_probability();
        // P = 0.95 × 0.92³ ≈ 0.74
        assert!(prob > 0.73 && prob < 0.75);
    }

    #[test]
    fn default_config_has_sandbox_enabled() {
        let config = WizardFlow::default_config();
        assert_eq!(config["sandbox.enabled"], serde_json::json!(true));
        assert_eq!(config["sandbox.default_grant"], serde_json::json!("empty"));
    }
}
