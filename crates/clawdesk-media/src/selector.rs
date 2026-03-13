//! Adaptive provider selector using Thompson Sampling (Beta distribution).
//!
//! Tracks per-provider success/failure rates and latencies. Selects the
//! provider with the highest sampled probability of success.

use std::collections::HashMap;

/// Statistics about a provider's performance.
#[derive(Debug, Clone)]
pub struct ProviderStats {
    /// Successes + 1 (Beta prior).
    pub alpha: f64,
    /// Failures + 1 (Beta prior).
    pub beta: f64,
    /// EMA of latency in milliseconds.
    pub avg_latency_ms: f64,
    /// Total number of calls.
    pub total_calls: u64,
}

impl Default for ProviderStats {
    fn default() -> Self {
        Self {
            alpha: 1.0,
            beta: 1.0,
            avg_latency_ms: 0.0,
            total_calls: 0,
        }
    }
}

impl ProviderStats {
    /// Estimated success rate.
    pub fn success_rate(&self) -> f64 {
        self.alpha / (self.alpha + self.beta)
    }
}

/// Adaptive provider selection using Thompson Sampling.
pub struct AdaptiveSelector {
    stats: HashMap<String, ProviderStats>,
}

impl AdaptiveSelector {
    pub fn new() -> Self {
        Self {
            stats: HashMap::new(),
        }
    }

    /// Register a provider.
    pub fn register(&mut self, name: &str) {
        self.stats
            .entry(name.to_string())
            .or_insert_with(ProviderStats::default);
    }

    /// Select the best provider using Thompson Sampling.
    ///
    /// For each provider, samples from its Beta distribution and picks the
    /// one with the highest sampled value (balances exploration/exploitation).
    pub fn select(&self) -> Option<String> {
        if self.stats.is_empty() {
            return None;
        }

        let mut best_name = None;
        let mut best_sample = f64::NEG_INFINITY;

        for (name, stats) in &self.stats {
            let sample = Self::sample_beta(stats.alpha, stats.beta);
            if sample > best_sample {
                best_sample = sample;
                best_name = Some(name.clone());
            }
        }

        best_name
    }

    /// Sample from the Beta(alpha, beta) distribution.
    ///
    /// Uses the Gamma ratio method: Beta(a,b) = X/(X+Y) where X~Gamma(a),
    /// Y~Gamma(b). Gamma sampling uses Marsaglia & Tsang (2000) for shape≥1
    /// and Ahrens–Dieter (1974) for shape<1.
    fn sample_beta(alpha: f64, beta: f64) -> f64 {
        let x = Self::sample_gamma(alpha);
        let y = Self::sample_gamma(beta);
        if x + y == 0.0 {
            // Degenerate case — fall back to mean
            alpha / (alpha + beta)
        } else {
            x / (x + y)
        }
    }

    /// Sample from Gamma(shape, scale=1) using Marsaglia & Tsang's method.
    fn sample_gamma(shape: f64) -> f64 {
        assert!(shape > 0.0, "Gamma shape must be positive");

        if shape < 1.0 {
            // Ahrens–Dieter: Gamma(a) = Gamma(a+1) * U^(1/a)
            let g = Self::sample_gamma(shape + 1.0);
            let u: f64 = fastrand::f64().max(f64::MIN_POSITIVE);
            return g * u.powf(1.0 / shape);
        }

        // Marsaglia & Tsang (2000)
        let d = shape - 1.0 / 3.0;
        let c = 1.0 / (9.0 * d).sqrt();
        loop {
            let x = Self::standard_normal();
            let v = (1.0 + c * x).powi(3);
            if v <= 0.0 {
                continue;
            }
            let u: f64 = fastrand::f64().max(f64::MIN_POSITIVE);
            let x2 = x * x;
            if u < 1.0 - 0.0331 * x2 * x2 {
                return d * v;
            }
            if u.ln() < 0.5 * x2 + d * (1.0 - v + v.ln()) {
                return d * v;
            }
        }
    }

    /// Box-Muller transform for standard normal samples.
    fn standard_normal() -> f64 {
        let u1: f64 = fastrand::f64().max(f64::MIN_POSITIVE);
        let u2: f64 = fastrand::f64();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }

    /// Record a successful call.
    pub fn record_success(&mut self, name: &str, latency_ms: u64) {
        let stats = self
            .stats
            .entry(name.to_string())
            .or_insert_with(ProviderStats::default);
        stats.alpha += 1.0;
        stats.total_calls += 1;
        // EMA with decay factor 0.1.
        let decay = 0.1;
        stats.avg_latency_ms =
            stats.avg_latency_ms * (1.0 - decay) + latency_ms as f64 * decay;
    }

    /// Record a failed call.
    pub fn record_failure(&mut self, name: &str, latency_ms: u64) {
        let stats = self
            .stats
            .entry(name.to_string())
            .or_insert_with(ProviderStats::default);
        stats.beta += 1.0;
        stats.total_calls += 1;
        let decay = 0.1;
        stats.avg_latency_ms =
            stats.avg_latency_ms * (1.0 - decay) + latency_ms as f64 * decay;
    }

    /// Get stats for a provider.
    pub fn get_stats(&self, name: &str) -> Option<&ProviderStats> {
        self.stats.get(name)
    }

    /// Get all provider stats.
    pub fn all_stats(&self) -> &HashMap<String, ProviderStats> {
        &self.stats
    }
}

impl Default for AdaptiveSelector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_and_select() {
        let mut sel = AdaptiveSelector::new();
        sel.register("provider-a");
        sel.register("provider-b");
        let choice = sel.select();
        assert!(choice.is_some());
    }

    #[test]
    fn test_success_improves_selection() {
        let mut sel = AdaptiveSelector::new();
        sel.register("good");
        sel.register("bad");
        for _ in 0..20 {
            sel.record_success("good", 50);
            sel.record_failure("bad", 200);
        }
        let stats_good = sel.get_stats("good").unwrap();
        let stats_bad = sel.get_stats("bad").unwrap();
        assert!(stats_good.success_rate() > stats_bad.success_rate());
    }

    #[test]
    fn test_ema_latency() {
        let mut sel = AdaptiveSelector::new();
        sel.register("p");
        sel.record_success("p", 100);
        sel.record_success("p", 100);
        sel.record_success("p", 100);
        let stats = sel.get_stats("p").unwrap();
        assert!(stats.avg_latency_ms > 0.0);
        assert!(stats.avg_latency_ms <= 100.0);
    }

    #[test]
    fn test_empty_selector() {
        let sel = AdaptiveSelector::new();
        assert!(sel.select().is_none());
    }
}
