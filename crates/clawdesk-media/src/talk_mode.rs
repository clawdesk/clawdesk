//! Talk Mode — full-duplex conversational overlay with phase management.
//!
//! Talk Mode provides a floating overlay UI for voice conversation with agents.
//! It manages phases: Idle → Listening → Thinking → Speaking → Idle.
//!
//! ## Phases
//! - **Idle**: Waiting for activation (wake phrase or PTT).
//! - **Listening**: Capturing user speech via microphone (VAD-controlled).
//! - **Thinking**: Agent processing the transcription.
//! - **Speaking**: TTS playing the agent response.
//!
//! ## Overlay
//! A floating circular overlay shows waveform / pulse animation and status.
//! The overlay is always-on-top, draggable, and semi-transparent.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

// ═══════════════════════════════════════════════════════════════
// Phase state machine
// ═══════════════════════════════════════════════════════════════

/// State of the Talk Mode conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TalkPhase {
    /// Overlay hidden or showing idle state.
    Idle,
    /// Microphone active, capturing user speech.
    Listening,
    /// Agent is processing the user's request.
    Thinking,
    /// TTS is playing the agent response.
    Speaking,
    /// Paused by user.
    Paused,
    /// Error state (mic failure, API timeout, etc.).
    Error,
}

/// How Talk Mode was activated.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationSource {
    /// Activated by voice wake phrase detection.
    WakePhrase,
    /// Activated by push-to-talk hotkey.
    PushToTalk,
    /// Activated by UI button click.
    UiButton,
    /// Activated programmatically by agent.
    Programmatic,
}

/// Talk Mode event for UI updates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TalkEvent {
    /// Phase changed.
    PhaseChanged {
        phase: TalkPhase,
        source: Option<ActivationSource>,
    },
    /// User speech transcription (partial).
    TranscriptPartial { text: String },
    /// User speech transcription (final).
    TranscriptFinal { text: String },
    /// Agent response text (for subtitle display).
    ResponseText { text: String },
    /// Audio level for waveform visualization.
    AudioLevel { level: f32 },
    /// Error occurred.
    Error { message: String },
    /// Talk session started.
    SessionStarted { session_id: String },
    /// Talk session ended.
    SessionEnded {
        session_id: String,
        duration_secs: f64,
        turns: u32,
    },
}

// ═══════════════════════════════════════════════════════════════
// Overlay configuration
// ═══════════════════════════════════════════════════════════════

/// Position of the floating overlay.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OverlayPosition {
    /// X position (0.0 = left, 1.0 = right).
    pub x: f64,
    /// Y position (0.0 = top, 1.0 = bottom).
    pub y: f64,
}

impl Default for OverlayPosition {
    fn default() -> Self {
        Self { x: 0.5, y: 0.85 }
    }
}

/// Visual style for the overlay.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverlayStyle {
    /// Small circle with pulse animation.
    Circle,
    /// Pill-shaped bar with waveform.
    Pill,
    /// Full bottom bar with subtitles.
    Bar,
    /// Minimal dot indicator.
    Minimal,
}

impl Default for OverlayStyle {
    fn default() -> Self {
        Self::Circle
    }
}

/// Configuration for Talk Mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TalkModeConfig {
    /// Whether Talk Mode is enabled.
    pub enabled: bool,
    /// Target agent to converse with.
    pub target_agent: Option<String>,
    /// Always-on-top window.
    pub always_on_top: bool,
    /// Overlay style.
    pub style: OverlayStyle,
    /// Initial overlay position.
    pub position: OverlayPosition,
    /// Overlay opacity (0.0–1.0).
    pub opacity: f64,
    /// Overlay diameter in pixels (for circle style).
    pub diameter: u32,
    /// Auto-dismiss after silence (seconds). 0 = never.
    pub auto_dismiss_secs: u32,
    /// Show subtitles for agent response.
    pub show_subtitles: bool,
    /// Max subtitle length before scrolling.
    pub max_subtitle_chars: usize,
    /// Continuous conversation mode (auto-re-listen after speaking).
    pub continuous: bool,
    /// Beep on phase transitions.
    pub transition_sounds: bool,
    /// Push-to-talk hotkey (e.g. "CmdOrCtrl+Shift+Space").
    pub ptt_hotkey: Option<String>,
}

impl Default for TalkModeConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            target_agent: None,
            always_on_top: true,
            style: OverlayStyle::default(),
            position: OverlayPosition::default(),
            opacity: 0.9,
            diameter: 80,
            auto_dismiss_secs: 30,
            show_subtitles: true,
            max_subtitle_chars: 200,
            continuous: true,
            transition_sounds: true,
            ptt_hotkey: Some("CmdOrCtrl+Shift+Space".into()),
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Session tracking
// ═══════════════════════════════════════════════════════════════

/// A single conversation turn in Talk Mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TalkTurn {
    /// Turn index (0-based).
    pub index: u32,
    /// User's spoken text (transcription).
    pub user_text: String,
    /// Agent's response text.
    pub agent_text: String,
    /// How long the user spoke (ms).
    pub user_speech_ms: u64,
    /// How long the agent response TTS played (ms).
    pub agent_speech_ms: u64,
    /// Time from end-of-user-speech to start-of-agent-speech (ms).
    pub latency_ms: u64,
}

/// Talk Mode session statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TalkSessionStats {
    pub session_id: String,
    pub started_at: String,
    pub duration_secs: f64,
    pub turns: Vec<TalkTurn>,
    pub total_user_speech_secs: f64,
    pub total_agent_speech_secs: f64,
    pub avg_latency_ms: f64,
    pub activation_source: ActivationSource,
}

// ═══════════════════════════════════════════════════════════════
// Controller
// ═══════════════════════════════════════════════════════════════

/// Talk Mode controller — manages the conversation state machine.
pub struct TalkModeController {
    config: Arc<RwLock<TalkModeConfig>>,
    phase: Arc<RwLock<TalkPhase>>,
    session_id: Arc<RwLock<Option<String>>>,
    session_start: Arc<RwLock<Option<Instant>>>,
    turn_count: Arc<RwLock<u32>>,
    turns: Arc<RwLock<Vec<TalkTurn>>>,
    event_tx: Option<mpsc::Sender<TalkEvent>>,
    current_transcript: Arc<RwLock<String>>,
    current_response: Arc<RwLock<String>>,
}

impl TalkModeController {
    pub fn new(config: TalkModeConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            phase: Arc::new(RwLock::new(TalkPhase::Idle)),
            session_id: Arc::new(RwLock::new(None)),
            session_start: Arc::new(RwLock::new(None)),
            turn_count: Arc::new(RwLock::new(0)),
            turns: Arc::new(RwLock::new(Vec::new())),
            event_tx: None,
            current_transcript: Arc::new(RwLock::new(String::new())),
            current_response: Arc::new(RwLock::new(String::new())),
        }
    }

    /// Set the event channel for UI updates.
    pub fn with_event_channel(mut self, tx: mpsc::Sender<TalkEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// Get current phase.
    pub async fn phase(&self) -> TalkPhase {
        *self.phase.read().await
    }

    /// Emit a TalkEvent.
    async fn emit(&self, event: TalkEvent) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(event).await;
        }
    }

    /// Transition to a new phase.
    async fn transition(&self, new_phase: TalkPhase, source: Option<ActivationSource>) {
        let old = *self.phase.read().await;
        if old == new_phase {
            return;
        }
        *self.phase.write().await = new_phase;
        debug!(from = ?old, to = ?new_phase, "talk mode phase transition");
        self.emit(TalkEvent::PhaseChanged {
            phase: new_phase,
            source,
        })
        .await;
    }

    /// Activate Talk Mode — start a session and begin listening.
    pub async fn activate(&self, source: ActivationSource) -> Result<(), String> {
        let current = self.phase().await;
        if current != TalkPhase::Idle && current != TalkPhase::Paused {
            return Err(format!("cannot activate from {:?}", current));
        }

        let sid = format!("talk-{}", uuid::Uuid::new_v4().as_simple());
        *self.session_id.write().await = Some(sid.clone());
        *self.session_start.write().await = Some(Instant::now());
        *self.turn_count.write().await = 0;
        self.turns.write().await.clear();

        info!(session_id = %sid, source = ?source, "talk mode activated");
        self.emit(TalkEvent::SessionStarted {
            session_id: sid,
        })
        .await;

        self.transition(TalkPhase::Listening, Some(source)).await;
        Ok(())
    }

    /// Deactivate Talk Mode — end the session.
    pub async fn deactivate(&self) {
        let sid = self.session_id.read().await.clone();
        let duration = self
            .session_start
            .read()
            .await
            .map(|s| s.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        let turns = *self.turn_count.read().await;

        self.transition(TalkPhase::Idle, None).await;
        *self.session_id.write().await = None;
        *self.session_start.write().await = None;

        if let Some(sid) = sid {
            info!(session_id = %sid, duration_secs = duration, turns, "talk mode deactivated");
            self.emit(TalkEvent::SessionEnded {
                session_id: sid,
                duration_secs: duration,
                turns,
            })
            .await;
        }
    }

    /// Pause Talk Mode (keep session open, stop listening).
    pub async fn pause(&self) -> Result<(), String> {
        let current = self.phase().await;
        if current == TalkPhase::Idle {
            return Err("not active".into());
        }
        self.transition(TalkPhase::Paused, None).await;
        Ok(())
    }

    /// Resume Talk Mode from paused state.
    pub async fn resume(&self) -> Result<(), String> {
        let current = self.phase().await;
        if current != TalkPhase::Paused {
            return Err(format!("not paused, currently {:?}", current));
        }
        self.transition(TalkPhase::Listening, None).await;
        Ok(())
    }

    /// Signal that user speech has finished, with transcript.
    /// Transitions Listening → Thinking.
    pub async fn on_user_speech_end(&self, transcript: String) -> Result<(), String> {
        let current = self.phase().await;
        if current != TalkPhase::Listening {
            return Err(format!("expected Listening, got {:?}", current));
        }

        *self.current_transcript.write().await = transcript.clone();
        self.emit(TalkEvent::TranscriptFinal {
            text: transcript,
        })
        .await;
        self.transition(TalkPhase::Thinking, None).await;
        Ok(())
    }

    /// Signal that agent response is ready to speak.
    /// Transitions Thinking → Speaking.
    pub async fn on_agent_response(&self, response_text: String) -> Result<(), String> {
        let current = self.phase().await;
        if current != TalkPhase::Thinking {
            return Err(format!("expected Thinking, got {:?}", current));
        }

        *self.current_response.write().await = response_text.clone();
        self.emit(TalkEvent::ResponseText {
            text: response_text,
        })
        .await;
        self.transition(TalkPhase::Speaking, None).await;
        Ok(())
    }

    /// Signal that TTS playback has finished.
    /// Transitions Speaking → Listening (continuous) or Idle.
    pub async fn on_speaking_finished(&self) -> Result<(), String> {
        let current = self.phase().await;
        if current != TalkPhase::Speaking {
            return Err(format!("expected Speaking, got {:?}", current));
        }

        // Record the turn — scope the write guards so they drop before deactivate()
        {
            let user_text = self.current_transcript.read().await.clone();
            let agent_text = self.current_response.read().await.clone();
            let mut count = self.turn_count.write().await;
            let turn = TalkTurn {
                index: *count,
                user_text,
                agent_text,
                user_speech_ms: 0, // Populated by caller
                agent_speech_ms: 0,
                latency_ms: 0,
            };
            self.turns.write().await.push(turn);
            *count += 1;
        }

        // Clear current state
        self.current_transcript.write().await.clear();
        self.current_response.write().await.clear();

        // Check if continuous mode — all locks dropped before potential deactivate()
        let continuous = self.config.read().await.continuous;
        if continuous {
            self.transition(TalkPhase::Listening, None).await;
        } else {
            self.deactivate().await;
        }
        Ok(())
    }

    /// Report an error, transitions to Error state.
    pub async fn on_error(&self, message: String) {
        warn!(error = %message, "talk mode error");
        self.emit(TalkEvent::Error {
            message: message.clone(),
        })
        .await;
        self.transition(TalkPhase::Error, None).await;
    }

    /// Feed audio level for overlay visualization.
    pub async fn feed_audio_level(&self, level: f32) {
        self.emit(TalkEvent::AudioLevel { level }).await;
    }

    /// Get session stats.
    pub async fn session_stats(&self) -> Option<TalkSessionStats> {
        let sid = self.session_id.read().await.clone()?;
        let duration = self
            .session_start
            .read()
            .await
            .map(|s| s.elapsed().as_secs_f64())
            .unwrap_or(0.0);
        let turns = self.turns.read().await.clone();
        let avg_latency = if turns.is_empty() {
            0.0
        } else {
            turns.iter().map(|t| t.latency_ms as f64).sum::<f64>() / turns.len() as f64
        };

        Some(TalkSessionStats {
            session_id: sid,
            started_at: chrono::Utc::now().to_rfc3339(),
            duration_secs: duration,
            total_user_speech_secs: turns.iter().map(|t| t.user_speech_ms as f64 / 1000.0).sum(),
            total_agent_speech_secs: turns
                .iter()
                .map(|t| t.agent_speech_ms as f64 / 1000.0)
                .sum(),
            avg_latency_ms: avg_latency,
            activation_source: ActivationSource::UiButton,
            turns,
        })
    }

    /// Get config.
    pub async fn config(&self) -> TalkModeConfig {
        self.config.read().await.clone()
    }

    /// Update config at runtime.
    pub async fn update_config(&self, new_config: TalkModeConfig) {
        *self.config.write().await = new_config;
    }
}

// ═══════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn talk_mode_config_default() {
        let cfg = TalkModeConfig::default();
        assert!(cfg.enabled);
        assert!(cfg.continuous);
        assert!(cfg.show_subtitles);
        assert_eq!(cfg.diameter, 80);
    }

    #[tokio::test]
    async fn phase_transitions_happy_path() {
        let ctrl = TalkModeController::new(TalkModeConfig::default());
        assert_eq!(ctrl.phase().await, TalkPhase::Idle);

        ctrl.activate(ActivationSource::UiButton).await.unwrap();
        assert_eq!(ctrl.phase().await, TalkPhase::Listening);

        ctrl.on_user_speech_end("Hello".into()).await.unwrap();
        assert_eq!(ctrl.phase().await, TalkPhase::Thinking);

        ctrl.on_agent_response("Hi there!".into()).await.unwrap();
        assert_eq!(ctrl.phase().await, TalkPhase::Speaking);

        // Continuous mode → back to Listening
        ctrl.on_speaking_finished().await.unwrap();
        assert_eq!(ctrl.phase().await, TalkPhase::Listening);

        ctrl.deactivate().await;
        assert_eq!(ctrl.phase().await, TalkPhase::Idle);
    }

    #[tokio::test]
    async fn non_continuous_ends_after_speaking() {
        let mut cfg = TalkModeConfig::default();
        cfg.continuous = false;
        let ctrl = TalkModeController::new(cfg);

        ctrl.activate(ActivationSource::WakePhrase).await.unwrap();
        ctrl.on_user_speech_end("Test".into()).await.unwrap();
        ctrl.on_agent_response("Done".into()).await.unwrap();
        ctrl.on_speaking_finished().await.unwrap();
        assert_eq!(ctrl.phase().await, TalkPhase::Idle);
    }

    #[tokio::test]
    async fn pause_resume() {
        let ctrl = TalkModeController::new(TalkModeConfig::default());
        ctrl.activate(ActivationSource::PushToTalk).await.unwrap();

        ctrl.pause().await.unwrap();
        assert_eq!(ctrl.phase().await, TalkPhase::Paused);

        ctrl.resume().await.unwrap();
        assert_eq!(ctrl.phase().await, TalkPhase::Listening);
    }

    #[tokio::test]
    async fn activate_from_invalid_state() {
        let ctrl = TalkModeController::new(TalkModeConfig::default());
        ctrl.activate(ActivationSource::UiButton).await.unwrap();
        ctrl.on_user_speech_end("x".into()).await.unwrap();
        // In Thinking state — cannot activate
        let result = ctrl.activate(ActivationSource::UiButton).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn event_channel() {
        let (tx, mut rx) = mpsc::channel(32);
        let ctrl = TalkModeController::new(TalkModeConfig::default()).with_event_channel(tx);

        ctrl.activate(ActivationSource::UiButton).await.unwrap();

        // Should have received SessionStarted and PhaseChanged
        let e1 = rx.recv().await.unwrap();
        match e1 {
            TalkEvent::SessionStarted { .. } => {}
            other => panic!("expected SessionStarted, got {:?}", other),
        }
        let e2 = rx.recv().await.unwrap();
        match e2 {
            TalkEvent::PhaseChanged {
                phase: TalkPhase::Listening,
                ..
            } => {}
            other => panic!("expected PhaseChanged(Listening), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn turn_tracking() {
        let ctrl = TalkModeController::new(TalkModeConfig::default());
        ctrl.activate(ActivationSource::UiButton).await.unwrap();

        // Complete two turns
        for i in 0..2 {
            ctrl.on_user_speech_end(format!("Q{}", i)).await.unwrap();
            ctrl.on_agent_response(format!("A{}", i)).await.unwrap();
            ctrl.on_speaking_finished().await.unwrap();
        }

        let stats = ctrl.session_stats().await.unwrap();
        assert_eq!(stats.turns.len(), 2);
        assert_eq!(stats.turns[0].user_text, "Q0");
        assert_eq!(stats.turns[1].agent_text, "A1");
    }

    #[test]
    fn talk_phase_serialization() {
        assert_eq!(
            serde_json::to_string(&TalkPhase::Listening).unwrap(),
            "\"listening\""
        );
        assert_eq!(
            serde_json::to_string(&TalkPhase::Thinking).unwrap(),
            "\"thinking\""
        );
    }

    #[test]
    fn overlay_style_serialization() {
        assert_eq!(
            serde_json::to_string(&OverlayStyle::Pill).unwrap(),
            "\"pill\""
        );
    }

    #[tokio::test]
    async fn error_state() {
        let ctrl = TalkModeController::new(TalkModeConfig::default());
        ctrl.activate(ActivationSource::UiButton).await.unwrap();
        ctrl.on_error("mic failed".into()).await;
        assert_eq!(ctrl.phase().await, TalkPhase::Error);
    }
}
