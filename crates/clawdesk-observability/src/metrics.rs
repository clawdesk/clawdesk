//! Metrics aggregation for GenAI traces.
//!
//! Pre-aggregated metrics bucketed by hour for dashboards.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Metrics aggregator for GenAI operations.
pub struct MetricsAggregator {
    data: Arc<RwLock<HashMap<MetricKey, MetricValue>>>,
}

/// Key for metrics aggregation.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct MetricKey {
    pub tenant_id: u64,
    pub model: String,
    pub metric_name: String,
    pub bucket_time: u64,
}

/// Aggregated metric value.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MetricValue {
    pub count: u64,
    pub sum: f64,
    pub min: f64,
    pub max: f64,
}

impl MetricValue {
    pub fn avg(&self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.sum / self.count as f64
        }
    }
}

impl MetricsAggregator {
    pub fn new() -> Self {
        Self {
            data: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub fn record_latency(&self, tenant_id: u64, model: &str, latency_us: u64) {
        self.record(tenant_id, model, "latency", latency_us as f64);
    }

    pub fn record_tokens(&self, tenant_id: u64, model: &str, tokens: u32) {
        self.record(tenant_id, model, "tokens", tokens as f64);
    }

    pub fn record_cost(&self, tenant_id: u64, model: &str, cost: f64) {
        self.record(tenant_id, model, "cost", cost);
    }

    fn record(&self, tenant_id: u64, model: &str, metric_name: &str, value: f64) {
        let bucket_time = self.get_bucket_time();
        let key = MetricKey {
            tenant_id,
            model: model.to_string(),
            metric_name: metric_name.to_string(),
            bucket_time,
        };

        let mut data = self.data.write().unwrap();
        let entry = data.entry(key).or_default();
        entry.count += 1;
        entry.sum += value;
        if entry.min == 0.0 || value < entry.min {
            entry.min = value;
        }
        if value > entry.max {
            entry.max = value;
        }
    }

    fn get_bucket_time(&self) -> u64 {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        (now / 3600) * 3600
    }
}

impl Default for MetricsAggregator {
    fn default() -> Self {
        Self::new()
    }
}
