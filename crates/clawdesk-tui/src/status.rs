//! Status bar — provider health, active model, usage stats.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Provider health status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HealthStatus {
    Healthy,
    Degraded,
    Down,
    Unknown,
}

impl HealthStatus {
    pub fn symbol(&self) -> &'static str {
        match self {
            Self::Healthy => "●",
            Self::Degraded => "◐",
            Self::Down => "○",
            Self::Unknown => "?",
        }
    }

    pub fn color_index(&self) -> u8 {
        match self {
            Self::Healthy => 2,  // green
            Self::Degraded => 3, // yellow
            Self::Down => 1,     // red
            Self::Unknown => 7,  // white
        }
    }
}

/// Session usage statistics.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageStats {
    pub total_tokens: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub request_count: u32,
    pub error_count: u32,
    pub avg_latency_ms: u64,
}

/// Status bar state.
pub struct StatusBar {
    pub active_model: String,
    pub active_provider: String,
    pub provider_health: HashMap<String, HealthStatus>,
    pub usage: UsageStats,
    pub mode_label: String,
}

impl StatusBar {
    pub fn new() -> Self {
        Self {
            active_model: "none".to_string(),
            active_provider: "none".to_string(),
            provider_health: HashMap::new(),
            usage: UsageStats::default(),
            mode_label: "CHAT".to_string(),
        }
    }

    /// Set active model.
    pub fn set_model(&mut self, provider: &str, model: &str) {
        self.active_provider = provider.to_string();
        self.active_model = model.to_string();
    }

    /// Update provider health.
    pub fn set_health(&mut self, provider: &str, status: HealthStatus) {
        self.provider_health.insert(provider.to_string(), status);
    }

    /// Record a completed request.
    pub fn record_request(&mut self, prompt_tokens: u32, completion_tokens: u32, latency_ms: u64) {
        self.usage.request_count += 1;
        self.usage.prompt_tokens += prompt_tokens as u64;
        self.usage.completion_tokens += completion_tokens as u64;
        self.usage.total_tokens += (prompt_tokens + completion_tokens) as u64;

        // Running average
        let n = self.usage.request_count as u64;
        self.usage.avg_latency_ms =
            (self.usage.avg_latency_ms * (n - 1) + latency_ms) / n;
    }

    /// Record an error.
    pub fn record_error(&mut self) {
        self.usage.error_count += 1;
    }

    /// Format status line for display.
    pub fn format_line(&self, width: usize) -> String {
        let model_info = format!("{}:{}", self.active_provider, self.active_model);

        let health_str: String = self
            .provider_health
            .iter()
            .map(|(name, status)| format!("{}{}", status.symbol(), name))
            .collect::<Vec<_>>()
            .join(" ");

        let usage_str = format!(
            "{}tok {}req {}ms",
            format_compact(self.usage.total_tokens),
            self.usage.request_count,
            self.usage.avg_latency_ms
        );

        let left = format!(" {} │ {}", self.mode_label, model_info);
        let right = format!("{} │ {} ", health_str, usage_str);

        let gap = width.saturating_sub(left.len() + right.len());
        format!("{}{:gap$}{}", left, "", right, gap = gap)
    }

    /// Get overall health (worst across providers).
    pub fn overall_health(&self) -> HealthStatus {
        let mut worst = HealthStatus::Unknown;
        for status in self.provider_health.values() {
            match status {
                HealthStatus::Down => return HealthStatus::Down,
                HealthStatus::Degraded => worst = HealthStatus::Degraded,
                HealthStatus::Healthy if worst == HealthStatus::Unknown => {
                    worst = HealthStatus::Healthy;
                }
                _ => {}
            }
        }
        worst
    }
}

/// Compact number formatting (1.2k, 3.4M, etc.)
fn format_compact(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_bar_model_setting() {
        let mut bar = StatusBar::new();
        bar.set_model("anthropic", "claude-sonnet-4-20250514");
        assert_eq!(bar.active_provider, "anthropic");
        assert_eq!(bar.active_model, "claude-sonnet-4-20250514");
    }

    #[test]
    fn request_recording() {
        let mut bar = StatusBar::new();
        bar.record_request(100, 50, 200);
        bar.record_request(200, 100, 400);
        assert_eq!(bar.usage.request_count, 2);
        assert_eq!(bar.usage.total_tokens, 450);
        assert_eq!(bar.usage.avg_latency_ms, 300);
    }

    #[test]
    fn format_compact_numbers() {
        assert_eq!(format_compact(500), "500");
        assert_eq!(format_compact(1500), "1.5k");
        assert_eq!(format_compact(2_500_000), "2.5M");
    }

    #[test]
    fn health_status_symbols() {
        assert_eq!(HealthStatus::Healthy.symbol(), "●");
        assert_eq!(HealthStatus::Down.symbol(), "○");
    }

    #[test]
    fn overall_health_worst_case() {
        let mut bar = StatusBar::new();
        bar.set_health("a", HealthStatus::Healthy);
        bar.set_health("b", HealthStatus::Degraded);
        assert_eq!(bar.overall_health(), HealthStatus::Degraded);
    }
}
