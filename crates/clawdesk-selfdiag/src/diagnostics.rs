//! Self-diagnostics — monitors all components and suggests degradation actions.

use crate::component::{Component, ComponentHealth, HealthObservation, HealthStatus};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Configuration for self-diagnostics.
#[derive(Debug, Clone)]
pub struct DiagConfig {
    /// How long a component can be unhealthy before we alert.
    pub alert_after: Duration,
    /// Whether to auto-apply degradation actions.
    pub auto_degrade: bool,
}

impl Default for DiagConfig {
    fn default() -> Self {
        Self {
            alert_after: Duration::from_secs(60),
            auto_degrade: true,
        }
    }
}

/// What to do when a component is degraded or critical.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DegradationAction {
    /// Switch to a fallback provider.
    SwitchProvider { from: String, to: String },
    /// Reduce quality (e.g., skip embeddings, use BM25 only).
    ReduceQuality { setting: String, reason: String },
    /// Notify the user about the issue.
    NotifyUser { message: String },
    /// Attempt self-healing (restart, reconnect).
    SelfHeal { action: String },
    /// No action needed.
    None,
}

/// Result of diagnosing a single component.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosisResult {
    /// Which component was diagnosed.
    pub component: Component,
    /// Health status.
    pub status: HealthStatus,
    /// Error rate (0.0–1.0).
    pub error_rate: f64,
    /// Latency anomaly ratio (1.0 = normal).
    pub latency_ratio: f64,
    /// How long since the component was healthy.
    pub unhealthy_duration: Duration,
    /// Most common recent error.
    pub dominant_error: Option<String>,
    /// Recommended action.
    pub recommended_action: DegradationAction,
    /// Human-readable summary.
    pub summary: String,
}

/// Self-diagnostics engine — tracks all component health and recommends actions.
pub struct SelfDiagnostics {
    components: HashMap<Component, ComponentHealth>,
    config: DiagConfig,
    /// Fallback mapping: component → preferred fallback.
    fallbacks: HashMap<Component, DegradationAction>,
}

impl SelfDiagnostics {
    pub fn new(config: DiagConfig) -> Self {
        Self {
            components: HashMap::new(),
            config,
            fallbacks: Self::default_fallbacks(),
        }
    }

    /// Record a health observation for a component.
    pub fn record(&mut self, component: Component, observation: HealthObservation) {
        let health = self.components.entry(component.clone()).or_default();
        health.record(&observation);

        if !observation.success {
            debug!(
                component = ?component,
                error = ?observation.error_kind,
                ewma_error_rate = health.error_rate_ewma,
                "selfdiag: recorded error"
            );
        }
    }

    /// Diagnose all components and return results for unhealthy ones.
    pub fn diagnose(&self) -> Vec<DiagnosisResult> {
        self.components.iter()
            .filter_map(|(component, health)| {
                let status = health.status();
                if status == HealthStatus::Healthy || status == HealthStatus::Unknown {
                    return None;
                }

                let action = self.recommend_action(component, health, status);
                let summary = self.build_summary(component, health, status);

                if status == HealthStatus::Critical {
                    warn!(
                        component = ?component,
                        error_rate = health.error_rate_ewma,
                        latency_ratio = health.latency_anomaly_ratio(),
                        "selfdiag: component CRITICAL"
                    );
                }

                Some(DiagnosisResult {
                    component: component.clone(),
                    status,
                    error_rate: health.error_rate_ewma,
                    latency_ratio: health.latency_anomaly_ratio(),
                    unhealthy_duration: health.time_since_healthy(),
                    dominant_error: health.dominant_error(),
                    recommended_action: action,
                    summary,
                })
            })
            .collect()
    }

    /// Run a full health check and return a summary for prompt injection.
    pub fn health_summary(&self) -> Option<String> {
        let diagnoses = self.diagnose();
        if diagnoses.is_empty() {
            return None; // all healthy, no need to inject
        }

        let mut lines = vec!["<self_diagnosis>".to_string()];
        for d in &diagnoses {
            lines.push(format!("  {}", d.summary));
        }
        lines.push("</self_diagnosis>".to_string());
        Some(lines.join("\n"))
    }

    /// Get the health status of a specific component.
    pub fn component_status(&self, component: &Component) -> HealthStatus {
        self.components.get(component)
            .map(|h| h.status())
            .unwrap_or(HealthStatus::Unknown)
    }

    /// How many components are being tracked.
    pub fn tracked_count(&self) -> usize {
        self.components.len()
    }

    /// How many components are unhealthy (Degraded or Critical).
    pub fn unhealthy_count(&self) -> usize {
        self.components.values()
            .filter(|h| {
                let s = h.status();
                s == HealthStatus::Degraded || s == HealthStatus::Critical
            })
            .count()
    }

    fn recommend_action(
        &self,
        component: &Component,
        health: &ComponentHealth,
        status: HealthStatus,
    ) -> DegradationAction {
        // Check if we have a configured fallback
        if let Some(fallback) = self.fallbacks.get(component) {
            if status == HealthStatus::Critical {
                return fallback.clone();
            }
        }

        match (component, status) {
            (Component::EmbeddingService, HealthStatus::Critical) => {
                DegradationAction::ReduceQuality {
                    setting: "use_bm25_only".into(),
                    reason: "Embedding service is down, falling back to keyword search".into(),
                }
            }
            (Component::Provider(name), HealthStatus::Critical) => {
                DegradationAction::NotifyUser {
                    message: format!(
                        "The {} provider is experiencing issues (error rate: {:.0}%). \
                         Responses may be slower or use a fallback model.",
                        name, health.error_rate_ewma * 100.0,
                    ),
                }
            }
            (Component::MemoryStore, HealthStatus::Critical) => {
                DegradationAction::ReduceQuality {
                    setting: "skip_memory_recall".into(),
                    reason: "Memory store is unavailable, proceeding without recall".into(),
                }
            }
            (Component::Network, HealthStatus::Critical) => {
                DegradationAction::NotifyUser {
                    message: "Network connectivity issues detected. Some tools may fail.".into(),
                }
            }
            (_, HealthStatus::Degraded) => {
                DegradationAction::None // degraded but recoverable
            }
            _ => DegradationAction::None,
        }
    }

    fn build_summary(
        &self,
        component: &Component,
        health: &ComponentHealth,
        status: HealthStatus,
    ) -> String {
        let error_info = health.dominant_error()
            .map(|e| format!(" (main error: {})", e))
            .unwrap_or_default();
        format!(
            "{:?} is {:?}: error rate {:.0}%, latency {:.0}x normal{}",
            component,
            status,
            health.error_rate_ewma * 100.0,
            health.latency_anomaly_ratio(),
            error_info,
        )
    }

    fn default_fallbacks() -> HashMap<Component, DegradationAction> {
        let mut m = HashMap::new();
        m.insert(
            Component::EmbeddingService,
            DegradationAction::ReduceQuality {
                setting: "use_bm25_only".into(),
                reason: "Embedding service unavailable".into(),
            },
        );
        m
    }
}

impl Default for SelfDiagnostics {
    fn default() -> Self {
        Self::new(DiagConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_obs(ms: u64) -> HealthObservation {
        HealthObservation {
            success: true,
            latency: Duration::from_millis(ms),
            error_kind: None,
            timestamp: chrono::Utc::now(),
        }
    }

    fn err_obs(kind: &str) -> HealthObservation {
        HealthObservation {
            success: false,
            latency: Duration::from_millis(5000),
            error_kind: Some(kind.into()),
            timestamp: chrono::Utc::now(),
        }
    }

    #[test]
    fn healthy_components_not_reported() {
        let mut diag = SelfDiagnostics::default();
        for _ in 0..10 {
            diag.record(Component::EmbeddingService, ok_obs(50));
        }
        assert!(diag.diagnose().is_empty());
        assert!(diag.health_summary().is_none());
    }

    #[test]
    fn critical_component_diagnosed() {
        let mut diag = SelfDiagnostics::default();
        for _ in 0..5 { diag.record(Component::EmbeddingService, ok_obs(50)); }
        for _ in 0..20 { diag.record(Component::EmbeddingService, err_obs("500")); }
        let results = diag.diagnose();
        assert!(!results.is_empty());
        assert_eq!(results[0].status, HealthStatus::Critical);
    }

    #[test]
    fn embedding_fallback_to_bm25() {
        let mut diag = SelfDiagnostics::default();
        for _ in 0..5 { diag.record(Component::EmbeddingService, ok_obs(50)); }
        for _ in 0..20 { diag.record(Component::EmbeddingService, err_obs("timeout")); }
        let results = diag.diagnose();
        assert!(matches!(results[0].recommended_action, DegradationAction::ReduceQuality { .. }));
    }

    #[test]
    fn health_summary_for_prompt() {
        let mut diag = SelfDiagnostics::default();
        for _ in 0..5 { diag.record(Component::Provider("openai".into()), ok_obs(200)); }
        for _ in 0..20 { diag.record(Component::Provider("openai".into()), err_obs("rate_limit")); }
        let summary = diag.health_summary();
        assert!(summary.is_some());
        assert!(summary.unwrap().contains("self_diagnosis"));
    }

    #[test]
    fn unhealthy_count() {
        let mut diag = SelfDiagnostics::default();
        for _ in 0..10 { diag.record(Component::MemoryStore, ok_obs(20)); }
        for _ in 0..5 { diag.record(Component::EmbeddingService, ok_obs(50)); }
        for _ in 0..20 { diag.record(Component::EmbeddingService, err_obs("500")); }
        assert_eq!(diag.unhealthy_count(), 1);
        assert_eq!(diag.tracked_count(), 2);
    }
}
