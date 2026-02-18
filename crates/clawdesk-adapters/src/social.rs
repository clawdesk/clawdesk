//! Social platform snapshot collector with EWMA trend detection (R11).
//!
//! ## Trend Detection
//!
//! Exponentially Weighted Moving Average with Bollinger-style anomaly bands:
//!
//! ```text
//! EWMA_t = α · x_t + (1 - α) · EWMA_{t-1}
//! EWMV_t = α · (x_t - EWMA_t)² + (1 - α) · EWMV_{t-1}
//! σ_t = √EWMV_t
//! anomaly if |x_t - EWMA_t| > k · σ_t  (k=2 for 95%)
//! ```
//!
//! O(1) update per data point — just two floats per metric.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// EWMA tracker for a single metric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EwmaTracker {
    /// Metric name (e.g., "youtube.views", "instagram.followers")
    pub name: String,
    /// Smoothing factor α = 2/(span+1)
    pub alpha: f64,
    /// Current EWMA value
    pub ewma: f64,
    /// Current exponential variance
    pub ewmv: f64,
    /// Anomaly detection multiplier (default: 2.0 for 95% band)
    pub anomaly_k: f64,
    /// Whether the tracker has been initialized
    pub initialized: bool,
    /// Last observed value
    pub last_value: f64,
    /// Last observation timestamp
    pub last_updated: Option<DateTime<Utc>>,
    /// Observation count
    pub count: u64,
}

impl EwmaTracker {
    /// Create a tracker with a given span (number of periods).
    /// α = 2/(span+1), so span=7 gives weekly trends.
    pub fn new(name: impl Into<String>, span: usize) -> Self {
        let alpha = 2.0 / (span as f64 + 1.0);
        Self {
            name: name.into(),
            alpha,
            ewma: 0.0,
            ewmv: 0.0,
            anomaly_k: 2.0,
            initialized: false,
            last_value: 0.0,
            last_updated: None,
            count: 0,
        }
    }

    /// Update with a new observation. O(1).
    pub fn update(&mut self, value: f64, timestamp: DateTime<Utc>) -> EwmaUpdate {
        self.count += 1;
        self.last_value = value;
        self.last_updated = Some(timestamp);

        if !self.initialized {
            self.ewma = value;
            self.ewmv = 0.0;
            self.initialized = true;
            return EwmaUpdate {
                value,
                ewma: self.ewma,
                sigma: 0.0,
                z_score: 0.0,
                is_anomaly: false,
            };
        }

        let prev_ewma = self.ewma;
        self.ewma = self.alpha * value + (1.0 - self.alpha) * self.ewma;
        let diff = value - self.ewma;
        self.ewmv = self.alpha * diff * diff + (1.0 - self.alpha) * self.ewmv;

        let sigma = self.ewmv.sqrt();
        let z_score = if sigma > 0.0 {
            (value - prev_ewma).abs() / sigma
        } else {
            0.0
        };
        let is_anomaly = z_score > self.anomaly_k;

        EwmaUpdate {
            value,
            ewma: self.ewma,
            sigma,
            z_score,
            is_anomaly,
        }
    }

    /// Current trend direction: positive, negative, or flat.
    pub fn trend(&self) -> Trend {
        if !self.initialized || self.count < 2 {
            return Trend::Flat;
        }
        let diff = self.last_value - self.ewma;
        let sigma = self.ewmv.sqrt();
        if sigma <= 0.0 {
            return Trend::Flat;
        }
        if diff > 0.5 * sigma {
            Trend::Up
        } else if diff < -0.5 * sigma {
            Trend::Down
        } else {
            Trend::Flat
        }
    }
}

/// Result of an EWMA update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EwmaUpdate {
    pub value: f64,
    pub ewma: f64,
    pub sigma: f64,
    pub z_score: f64,
    pub is_anomaly: bool,
}

/// Trend direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Trend {
    Up,
    Down,
    Flat,
}

/// Social platform metric snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocialSnapshot {
    /// Platform name (youtube, instagram, x, tiktok)
    pub platform: String,
    /// Metrics: name → value
    pub metrics: HashMap<String, f64>,
    /// When this snapshot was taken
    pub timestamp: DateTime<Utc>,
}

/// Manages EWMA trackers for all metrics across all platforms.
pub struct SocialMetricsStore {
    /// Key: "platform.metric_name" → tracker
    pub trackers: HashMap<String, EwmaTracker>,
    /// EWMA span in periods (default: 7 for weekly)
    pub span: usize,
}

impl SocialMetricsStore {
    pub fn new(span: usize) -> Self {
        Self {
            trackers: HashMap::new(),
            span,
        }
    }

    /// Ingest a snapshot, updating all trackers. Returns anomalies.
    pub fn ingest(&mut self, snapshot: &SocialSnapshot) -> Vec<AnomalyAlert> {
        let mut alerts = Vec::new();
        for (metric_name, &value) in &snapshot.metrics {
            let key = format!("{}.{}", snapshot.platform, metric_name);
            let tracker = self
                .trackers
                .entry(key.clone())
                .or_insert_with(|| EwmaTracker::new(&key, self.span));

            let update = tracker.update(value, snapshot.timestamp);
            if update.is_anomaly {
                alerts.push(AnomalyAlert {
                    platform: snapshot.platform.clone(),
                    metric: metric_name.clone(),
                    value,
                    ewma: update.ewma,
                    z_score: update.z_score,
                    trend: tracker.trend(),
                    timestamp: snapshot.timestamp,
                });
            }
        }
        alerts
    }

    /// Generate a summary of all tracked metrics.
    pub fn summary(&self) -> Vec<MetricTrend> {
        self.trackers
            .values()
            .map(|t| MetricTrend {
                name: t.name.clone(),
                current: t.last_value,
                ewma: t.ewma,
                sigma: t.ewmv.sqrt(),
                trend: t.trend(),
                count: t.count,
            })
            .collect()
    }
}

/// Alert for an anomalous metric observation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnomalyAlert {
    pub platform: String,
    pub metric: String,
    pub value: f64,
    pub ewma: f64,
    pub z_score: f64,
    pub trend: Trend,
    pub timestamp: DateTime<Utc>,
}

/// Summary of a tracked metric's trend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricTrend {
    pub name: String,
    pub current: f64,
    pub ewma: f64,
    pub sigma: f64,
    pub trend: Trend,
    pub count: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ewma_detects_anomaly() {
        let mut tracker = EwmaTracker::new("test", 7);
        let now = Utc::now();

        // Establish baseline with 20 normal observations
        for i in 0..20 {
            let ts = now + chrono::Duration::hours(i);
            tracker.update(100.0 + (i as f64 % 3.0), ts);
        }

        // Big spike should be anomaly
        let spike = now + chrono::Duration::hours(21);
        let result = tracker.update(500.0, spike);
        assert!(result.is_anomaly, "500 should be anomalous vs ~100 baseline");
    }

    #[test]
    fn social_store_ingestion() {
        let mut store = SocialMetricsStore::new(7);
        let snapshot = SocialSnapshot {
            platform: "youtube".into(),
            metrics: [("views".into(), 1000.0), ("subscribers".into(), 500.0)]
                .into_iter()
                .collect(),
            timestamp: Utc::now(),
        };
        let alerts = store.ingest(&snapshot);
        assert!(alerts.is_empty()); // first observation, no anomaly
        assert_eq!(store.trackers.len(), 2);
    }
}
