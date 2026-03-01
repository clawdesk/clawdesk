//! TUI Event system — multiplexed event channel.
//!
//! Combines terminal input events, backend bus events, and tick timer
//! into a single stream via `tokio::select!`.

use crossterm::event::{Event as CrosstermEvent, KeyEvent, MouseEvent};
use std::time::Duration;
use tokio::sync::mpsc;
use tracing::debug;

/// All possible events in the TUI event loop.
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// Terminal input event (key press, mouse, resize)
    Terminal(CrosstermEvent),
    /// Backend event from the event bus
    Backend(BackendEvent),
    /// Periodic tick for animations and polling
    Tick,
    /// Request to quit the application
    Quit,
}

/// Events from the backend (gateway, agents, channels, etc.)
#[derive(Debug, Clone)]
pub enum BackendEvent {
    /// LLM streaming token
    StreamToken {
        session_id: String,
        token: String,
    },
    /// Agent state change
    AgentStateChanged {
        agent_id: String,
        state: String,
    },
    /// Channel health update
    ChannelHealth {
        channel_id: String,
        healthy: bool,
    },
    /// Memory operation result
    MemoryUpdate {
        operation: String,
        count: usize,
    },
    /// Tool execution event
    ToolExecution {
        tool_name: String,
        status: String,
        duration_ms: u64,
    },
    /// Log entry
    LogEntry {
        level: String,
        target: String,
        message: String,
    },
    /// Generic notification
    Notification {
        title: String,
        body: String,
    },
}

/// Event handler — polls terminal events and dispatches to the event channel.
pub struct EventHandler {
    tx: mpsc::UnboundedSender<AppEvent>,
    rx: mpsc::UnboundedReceiver<AppEvent>,
    tick_rate: Duration,
}

impl EventHandler {
    pub fn new(tick_rate_hz: u32) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let tick_rate = Duration::from_millis(1000 / tick_rate_hz as u64);
        Self { tx, rx, tick_rate }
    }

    /// Get a sender for pushing backend events.
    pub fn sender(&self) -> mpsc::UnboundedSender<AppEvent> {
        self.tx.clone()
    }

    /// Start the event polling loop (run in background).
    pub fn start_polling(&self) -> tokio::task::JoinHandle<()> {
        let tx = self.tx.clone();
        let tick_rate = self.tick_rate;

        tokio::spawn(async move {
            let mut tick_interval = tokio::time::interval(tick_rate);
            tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                let event = tokio::select! {
                    _ = tick_interval.tick() => AppEvent::Tick,
                    event = Self::poll_terminal() => match event {
                        Some(e) => e,
                        None => continue,
                    },
                };

                if tx.send(event).is_err() {
                    break;
                }
            }
        })
    }

    /// Receive the next event.
    pub async fn next(&mut self) -> Option<AppEvent> {
        self.rx.recv().await
    }

    /// Non-blocking poll for terminal events.
    async fn poll_terminal() -> Option<AppEvent> {
        // Use crossterm's async event stream
        if crossterm::event::poll(Duration::from_millis(10)).ok()? {
            let event = crossterm::event::read().ok()?;
            Some(AppEvent::Terminal(event))
        } else {
            // Yield to other tasks
            tokio::time::sleep(Duration::from_millis(5)).await;
            None
        }
    }
}
