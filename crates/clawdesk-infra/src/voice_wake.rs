//! Voice wake detection, PTT hotkey, and audio stream integration.
//!
//! Listens for voice wake events from the OS audio subsystem (macOS dictation,
//! Android speech recognition, etc.) and forwards transcribed text to an agent.
//!
//! ## Audio Integration
//! - `AudioStreamListener` trait — abstract audio capture (cpal, CoreAudio, etc.)
//! - `PttMonitor` — push-to-talk global hotkey monitoring
//! - `VoiceWakeRuntime` — combines VoiceWakeManager + audio listener + PTT

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info};

/// Voice wake configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceWakeConfig {
    /// Whether voice wake is enabled.
    pub enabled: bool,
    /// Wake phrase(s) that trigger listening.
    pub wake_phrases: Vec<String>,
    /// Agent to forward transcribed text to.
    pub target_agent: String,
    /// Command template (use `${text}` for transcribed text).
    pub command_template: String,
    /// Whether to play a confirmation sound.
    pub play_confirmation: bool,
    /// Silence timeout in seconds before finalizing input.
    pub silence_timeout_secs: u32,
}

impl Default for VoiceWakeConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            wake_phrases: vec!["hey llama".to_string()],
            target_agent: "default".to_string(),
            command_template: r#"openclaw agent --message "${text}" --thinking low"#.to_string(),
            play_confirmation: true,
            silence_timeout_secs: 3,
        }
    }
}

/// Voice wake event.
#[derive(Debug, Clone)]
pub enum WakeEvent {
    /// Wake phrase detected, start listening.
    WakeDetected { phrase: String },
    /// Transcription in progress.
    Transcribing { partial: String },
    /// Final transcription ready.
    Transcribed { text: String },
    /// Error during recognition.
    Error { message: String },
}

/// Manages voice wake detection and forwarding.
pub struct VoiceWakeManager {
    config: VoiceWakeConfig,
}

impl VoiceWakeManager {
    pub fn new(config: VoiceWakeConfig) -> Self {
        Self { config }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Check if text matches a wake phrase.
    pub fn is_wake_phrase(&self, text: &str) -> bool {
        let lower = text.to_lowercase();
        self.config
            .wake_phrases
            .iter()
            .any(|p| lower.contains(&p.to_lowercase()))
    }

    /// Build the command to execute for a transcribed message.
    pub fn build_command(&self, text: &str) -> String {
        self.config.command_template.replace("${text}", text)
    }

    /// Process a wake event.
    pub fn handle_event(&self, event: WakeEvent) -> Option<String> {
        match event {
            WakeEvent::WakeDetected { phrase } => {
                info!(phrase = %phrase, "voice wake detected");
                None
            }
            WakeEvent::Transcribing { partial } => {
                debug!(partial = %partial, "transcription in progress");
                None
            }
            WakeEvent::Transcribed { text } => {
                if text.trim().is_empty() {
                    debug!("empty transcription, ignoring");
                    return None;
                }
                let cmd = self.build_command(&text);
                info!(text = %text, cmd = %cmd, "forwarding voice command");
                Some(cmd)
            }
            WakeEvent::Error { message } => {
                tracing::warn!(error = %message, "voice wake error");
                None
            }
        }
    }
}

// ═══════════════════════════════════════════════════════════════
// Audio stream listener (integration point for cpal / CoreAudio)
// ═══════════════════════════════════════════════════════════════

/// Raw audio frame from the microphone.
#[derive(Debug, Clone)]
pub struct AudioFrame {
    /// PCM data (16-bit signed LE mono).
    pub data: Vec<u8>,
    /// Sample rate.
    pub sample_rate: u32,
    /// Duration in milliseconds.
    pub duration_ms: u32,
}

/// Trait for platform-specific audio capture.
#[async_trait]
pub trait AudioStreamListener: Send + Sync {
    /// Start capturing audio from the default input device.
    async fn start(&self) -> Result<(), String>;
    /// Stop capturing.
    async fn stop(&self) -> Result<(), String>;
    /// Whether the stream is active.
    fn is_active(&self) -> bool;
    /// Get the sample rate.
    fn sample_rate(&self) -> u32;
}

/// Stub audio listener for testing and platforms without native audio.
pub struct StubAudioListener {
    active: Arc<RwLock<bool>>,
}

impl StubAudioListener {
    pub fn new() -> Self {
        Self {
            active: Arc::new(RwLock::new(false)),
        }
    }
}

impl Default for StubAudioListener {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AudioStreamListener for StubAudioListener {
    async fn start(&self) -> Result<(), String> {
        *self.active.write().await = true;
        debug!("stub audio listener started");
        Ok(())
    }

    async fn stop(&self) -> Result<(), String> {
        *self.active.write().await = false;
        debug!("stub audio listener stopped");
        Ok(())
    }

    fn is_active(&self) -> bool {
        self.active.try_read().map(|g| *g).unwrap_or(false)
    }

    fn sample_rate(&self) -> u32 {
        16000
    }
}

// ═══════════════════════════════════════════════════════════════
// Push-to-Talk hotkey monitor
// ═══════════════════════════════════════════════════════════════

/// PTT hotkey configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PttConfig {
    /// Whether PTT is enabled.
    pub enabled: bool,
    /// Hotkey string (e.g. "CmdOrCtrl+Shift+Space", "F13", "RightAlt").
    pub hotkey: String,
    /// Whether holding the key keeps the mic active (true)
    /// or toggling it starts/stops (false).
    pub hold_to_talk: bool,
}

impl Default for PttConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            hotkey: "CmdOrCtrl+Shift+Space".into(),
            hold_to_talk: true,
        }
    }
}

/// PTT hotkey event.
#[derive(Debug, Clone)]
pub enum PttEvent {
    /// Key pressed (start talking).
    Pressed,
    /// Key released (stop talking, hold_to_talk mode).
    Released,
    /// Toggle: mic activated.
    ToggleOn,
    /// Toggle: mic deactivated.
    ToggleOff,
}

/// Trait for platform-specific global hotkey monitoring.
#[async_trait]
pub trait PttMonitor: Send + Sync {
    /// Register the hotkey and start monitoring.
    async fn start(&self, config: &PttConfig) -> Result<(), String>;
    /// Unregister the hotkey and stop monitoring.
    async fn stop(&self) -> Result<(), String>;
    /// Whether the monitor is active.
    fn is_active(&self) -> bool;
}

/// Stub PTT monitor for testing.
pub struct StubPttMonitor {
    active: Arc<RwLock<bool>>,
}

impl StubPttMonitor {
    pub fn new() -> Self {
        Self {
            active: Arc::new(RwLock::new(false)),
        }
    }
}

impl Default for StubPttMonitor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl PttMonitor for StubPttMonitor {
    async fn start(&self, config: &PttConfig) -> Result<(), String> {
        *self.active.write().await = true;
        debug!(hotkey = %config.hotkey, "stub PTT monitor started");
        Ok(())
    }

    async fn stop(&self) -> Result<(), String> {
        *self.active.write().await = false;
        debug!("stub PTT monitor stopped");
        Ok(())
    }

    fn is_active(&self) -> bool {
        self.active.try_read().map(|g| *g).unwrap_or(false)
    }
}

// ═══════════════════════════════════════════════════════════════
// Voice Wake Runtime
// ═══════════════════════════════════════════════════════════════

/// State of the wake runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WakeRuntimeState {
    /// Not started.
    Stopped,
    /// Listening for wake phrase (passive).
    Passive,
    /// Wake detected, actively capturing speech.
    Active,
    /// PTT key held, capturing speech.
    PttActive,
    /// Error state.
    Error,
}

/// Runtime event emitted by VoiceWakeRuntime.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WakeRuntimeEvent {
    /// Runtime state changed.
    StateChanged { state: WakeRuntimeState },
    /// Wake phrase detected.
    WakeDetected { phrase: String },
    /// PTT activated.
    PttActivated,
    /// PTT deactivated.
    PttDeactivated,
    /// Transcription result.
    Transcription { text: String, is_final: bool },
    /// Error.
    Error { message: String },
}

/// Combined voice wake + audio + PTT runtime.
pub struct VoiceWakeRuntime {
    manager: VoiceWakeManager,
    ptt_config: PttConfig,
    state: Arc<RwLock<WakeRuntimeState>>,
    event_tx: Option<mpsc::Sender<WakeRuntimeEvent>>,
}

impl VoiceWakeRuntime {
    pub fn new(wake_config: VoiceWakeConfig, ptt_config: PttConfig) -> Self {
        Self {
            manager: VoiceWakeManager::new(wake_config),
            ptt_config,
            state: Arc::new(RwLock::new(WakeRuntimeState::Stopped)),
            event_tx: None,
        }
    }

    /// Set event channel for UI updates.
    pub fn with_event_channel(mut self, tx: mpsc::Sender<WakeRuntimeEvent>) -> Self {
        self.event_tx = Some(tx);
        self
    }

    /// Get current state.
    pub async fn state(&self) -> WakeRuntimeState {
        *self.state.read().await
    }

    async fn set_state(&self, new_state: WakeRuntimeState) {
        *self.state.write().await = new_state;
        self.emit(WakeRuntimeEvent::StateChanged { state: new_state })
            .await;
    }

    async fn emit(&self, event: WakeRuntimeEvent) {
        if let Some(tx) = &self.event_tx {
            let _ = tx.send(event).await;
        }
    }

    /// Start the runtime (begin passive listening for wake phrase).
    pub async fn start(
        &self,
        audio: &dyn AudioStreamListener,
        ptt: &dyn PttMonitor,
    ) -> Result<(), String> {
        if !self.manager.is_enabled() && !self.ptt_config.enabled {
            return Err("neither wake nor PTT is enabled".into());
        }

        // Start audio capture if wake is enabled
        if self.manager.is_enabled() {
            audio.start().await?;
        }

        // Start PTT monitor if enabled
        if self.ptt_config.enabled {
            ptt.start(&self.ptt_config).await?;
        }

        self.set_state(WakeRuntimeState::Passive).await;
        info!("voice wake runtime started");
        Ok(())
    }

    /// Stop the runtime.
    pub async fn stop(
        &self,
        audio: &dyn AudioStreamListener,
        ptt: &dyn PttMonitor,
    ) -> Result<(), String> {
        audio.stop().await.ok();
        ptt.stop().await.ok();
        self.set_state(WakeRuntimeState::Stopped).await;
        info!("voice wake runtime stopped");
        Ok(())
    }

    /// Handle an incoming audio frame (from audio listener callback).
    pub async fn on_audio_frame(&self, _frame: &AudioFrame) -> Option<WakeRuntimeEvent> {
        let state = self.state().await;
        match state {
            WakeRuntimeState::Passive => {
                // In passive mode, we'd run wake-word detection on the audio.
                // This is where a keyword-spotting model (e.g. Porcupine, Whisper VAD)
                // would analyze the frame. For now, return None (stub).
                None
            }
            WakeRuntimeState::Active | WakeRuntimeState::PttActive => {
                // In active mode, audio is being buffered for STT.
                // The actual STT integration happens in the voice pipeline.
                None
            }
            _ => None,
        }
    }

    /// Handle a PTT hotkey event.
    pub async fn on_ptt_event(&self, event: PttEvent) {
        match event {
            PttEvent::Pressed | PttEvent::ToggleOn => {
                self.set_state(WakeRuntimeState::PttActive).await;
                self.emit(WakeRuntimeEvent::PttActivated).await;
                info!("PTT activated");
            }
            PttEvent::Released | PttEvent::ToggleOff => {
                self.set_state(WakeRuntimeState::Passive).await;
                self.emit(WakeRuntimeEvent::PttDeactivated).await;
                info!("PTT deactivated");
            }
        }
    }

    /// Handle wake phrase detection (from audio processing callback).
    pub async fn on_wake_detected(&self, phrase: &str) {
        if self.manager.is_wake_phrase(phrase) {
            self.set_state(WakeRuntimeState::Active).await;
            self.emit(WakeRuntimeEvent::WakeDetected {
                phrase: phrase.into(),
            })
            .await;
            info!(phrase, "wake phrase detected, entering active mode");
        }
    }

    /// Handle transcription result.
    pub async fn on_transcription(&self, text: &str, is_final: bool) -> Option<String> {
        self.emit(WakeRuntimeEvent::Transcription {
            text: text.into(),
            is_final,
        })
        .await;

        if is_final {
            let cmd = self.manager.handle_event(WakeEvent::Transcribed {
                text: text.into(),
            });
            // Return to passive listening
            self.set_state(WakeRuntimeState::Passive).await;
            cmd
        } else {
            self.manager.handle_event(WakeEvent::Transcribing {
                partial: text.into(),
            });
            None
        }
    }

    /// Get reference to the wake manager.
    pub fn manager(&self) -> &VoiceWakeManager {
        &self.manager
    }

    /// Get PTT config.
    pub fn ptt_config(&self) -> &PttConfig {
        &self.ptt_config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wake_phrase_detection() {
        let mgr = VoiceWakeManager::new(VoiceWakeConfig::default());
        assert!(mgr.is_wake_phrase("Hey Llama, what's the weather?"));
        assert!(mgr.is_wake_phrase("HEY LLAMA"));
        assert!(!mgr.is_wake_phrase("hello world"));
    }

    #[test]
    fn test_command_building() {
        let mgr = VoiceWakeManager::new(VoiceWakeConfig::default());
        let cmd = mgr.build_command("what's the weather");
        assert!(cmd.contains("what's the weather"));
        assert!(cmd.contains("openclaw agent --message"));
    }

    #[test]
    fn test_transcription_event() {
        let mgr = VoiceWakeManager::new(VoiceWakeConfig {
            enabled: true,
            ..Default::default()
        });
        let result = mgr.handle_event(WakeEvent::Transcribed {
            text: "turn off the lights".to_string(),
        });
        assert!(result.is_some());
        assert!(result.unwrap().contains("turn off the lights"));
    }

    #[test]
    fn test_empty_transcription_ignored() {
        let mgr = VoiceWakeManager::new(VoiceWakeConfig::default());
        let result = mgr.handle_event(WakeEvent::Transcribed {
            text: "  ".to_string(),
        });
        assert!(result.is_none());
    }

    #[test]
    fn ptt_config_default() {
        let cfg = PttConfig::default();
        assert!(!cfg.enabled);
        assert!(cfg.hold_to_talk);
        assert_eq!(cfg.hotkey, "CmdOrCtrl+Shift+Space");
    }

    #[tokio::test]
    async fn stub_audio_listener_lifecycle() {
        let listener = StubAudioListener::new();
        assert!(!listener.is_active());

        listener.start().await.unwrap();
        assert!(listener.is_active());

        listener.stop().await.unwrap();
        assert!(!listener.is_active());
    }

    #[tokio::test]
    async fn stub_ptt_monitor_lifecycle() {
        let monitor = StubPttMonitor::new();
        assert!(!monitor.is_active());

        monitor.start(&PttConfig::default()).await.unwrap();
        assert!(monitor.is_active());

        monitor.stop().await.unwrap();
        assert!(!monitor.is_active());
    }

    #[tokio::test]
    async fn runtime_start_with_wake_enabled() {
        let wake = VoiceWakeConfig {
            enabled: true,
            ..Default::default()
        };
        let ptt = PttConfig::default();
        let runtime = VoiceWakeRuntime::new(wake, ptt);

        let listener = StubAudioListener::new();
        let monitor = StubPttMonitor::new();

        runtime.start(&listener, &monitor).await.unwrap();
        assert_eq!(runtime.state().await, WakeRuntimeState::Passive);
        assert!(listener.is_active());
    }

    #[tokio::test]
    async fn runtime_ptt_event() {
        let wake = VoiceWakeConfig {
            enabled: true,
            ..Default::default()
        };
        let ptt = PttConfig {
            enabled: true,
            ..Default::default()
        };
        let runtime = VoiceWakeRuntime::new(wake, ptt);

        let listener = StubAudioListener::new();
        let monitor = StubPttMonitor::new();
        runtime.start(&listener, &monitor).await.unwrap();

        runtime.on_ptt_event(PttEvent::Pressed).await;
        assert_eq!(runtime.state().await, WakeRuntimeState::PttActive);

        runtime.on_ptt_event(PttEvent::Released).await;
        assert_eq!(runtime.state().await, WakeRuntimeState::Passive);
    }

    #[tokio::test]
    async fn runtime_wake_detection() {
        let wake = VoiceWakeConfig {
            enabled: true,
            ..Default::default()
        };
        let ptt = PttConfig::default();
        let runtime = VoiceWakeRuntime::new(wake, ptt);

        let listener = StubAudioListener::new();
        let monitor = StubPttMonitor::new();
        runtime.start(&listener, &monitor).await.unwrap();

        runtime.on_wake_detected("hey llama").await;
        assert_eq!(runtime.state().await, WakeRuntimeState::Active);
    }

    #[tokio::test]
    async fn runtime_transcription_returns_command() {
        let wake = VoiceWakeConfig {
            enabled: true,
            ..Default::default()
        };
        let ptt = PttConfig::default();
        let runtime = VoiceWakeRuntime::new(wake, ptt);

        let listener = StubAudioListener::new();
        let monitor = StubPttMonitor::new();
        runtime.start(&listener, &monitor).await.unwrap();

        // Partial transcription returns None
        let result = runtime.on_transcription("what's the", false).await;
        assert!(result.is_none());

        // Final transcription returns command
        let result = runtime
            .on_transcription("what's the weather", true)
            .await;
        assert!(result.is_some());
        assert!(result.unwrap().contains("what's the weather"));
        // Should be back to passive
        assert_eq!(runtime.state().await, WakeRuntimeState::Passive);
    }

    #[tokio::test]
    async fn runtime_event_channel() {
        let (tx, mut rx) = mpsc::channel(32);
        let wake = VoiceWakeConfig {
            enabled: true,
            ..Default::default()
        };
        let ptt = PttConfig {
            enabled: true,
            ..Default::default()
        };
        let runtime = VoiceWakeRuntime::new(wake, ptt).with_event_channel(tx);

        let listener = StubAudioListener::new();
        let monitor = StubPttMonitor::new();
        runtime.start(&listener, &monitor).await.unwrap();

        // Should have emitted StateChanged(Passive)
        let evt = rx.recv().await.unwrap();
        match evt {
            WakeRuntimeEvent::StateChanged {
                state: WakeRuntimeState::Passive,
            } => {}
            other => panic!("expected StateChanged(Passive), got {:?}", other),
        }

        runtime.on_ptt_event(PttEvent::Pressed).await;
        // Should get StateChanged(PttActive) and PttActivated
        let _state_evt = rx.recv().await.unwrap();
        let ptt_evt = rx.recv().await.unwrap();
        match ptt_evt {
            WakeRuntimeEvent::PttActivated => {}
            other => panic!("expected PttActivated, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn runtime_neither_enabled_fails() {
        let runtime = VoiceWakeRuntime::new(
            VoiceWakeConfig::default(), // enabled: false
            PttConfig::default(),       // enabled: false
        );
        let listener = StubAudioListener::new();
        let monitor = StubPttMonitor::new();
        let result = runtime.start(&listener, &monitor).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn runtime_stop() {
        let wake = VoiceWakeConfig {
            enabled: true,
            ..Default::default()
        };
        let runtime = VoiceWakeRuntime::new(wake, PttConfig::default());
        let listener = StubAudioListener::new();
        let monitor = StubPttMonitor::new();

        runtime.start(&listener, &monitor).await.unwrap();
        assert_eq!(runtime.state().await, WakeRuntimeState::Passive);

        runtime.stop(&listener, &monitor).await.unwrap();
        assert_eq!(runtime.state().await, WakeRuntimeState::Stopped);
    }
}
