//! Capability-aware provider negotiation and model routing.
//!
//! The negotiator selects the best provider for a request based on:
//! 1. Capability intersection (O(1) bitwise AND)
//! 2. Model trie lookup (O(k) where k = namespace depth)
//! 3. Cost × latency scoring (greedy sort)
//!
//! ## Model namespace convention
//!
//! Models use hierarchical namespacing: `provider/model` or `provider/family/model`.
//! Examples: `anthropic/claude-sonnet-4-20250514`, `bedrock/meta/llama-3.1-70b`.
//!
//! Bare model names (e.g., `gpt-4o`) are resolved by scanning all providers.

use crate::capability::{ProviderCaps, ProviderWeight};
use crate::Provider;
use rustc_hash::FxHashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// A registered provider with its capabilities and model weights.
struct RegisteredProvider {
    provider: Arc<dyn Provider>,
    caps: ProviderCaps,
    weights: Vec<ProviderWeight>,
}

/// Capability-aware provider negotiator.
///
/// Selects the optimal provider for a given request based on capability
/// requirements, model name, and cost/latency preferences.
pub struct ProviderNegotiator {
    /// Provider name → registered provider.
    providers: FxHashMap<String, RegisteredProvider>,
    /// Model name → (provider_name, model_name) for fast lookup.
    model_index: FxHashMap<String, Vec<(String, String)>>,
}

impl ProviderNegotiator {
    pub fn new() -> Self {
        Self {
            providers: FxHashMap::default(),
            model_index: FxHashMap::default(),
        }
    }

    /// Register a provider with its capabilities and cost/latency weights.
    pub fn register(
        &mut self,
        provider: Arc<dyn Provider>,
        caps: ProviderCaps,
        weights: Vec<ProviderWeight>,
    ) {
        let name = provider.name().to_string();
        info!(%name, %caps, models = ?provider.models(), "negotiator: registering provider");

        // Index all models: both namespaced and bare names.
        for model in provider.models() {
            // Bare name index: "gpt-4o" → ("openai", "gpt-4o")
            self.model_index
                .entry(model.clone())
                .or_default()
                .push((name.clone(), model.clone()));

            // Namespaced index: "openai/gpt-4o" → ("openai", "gpt-4o")
            let namespaced = format!("{}/{}", name, model);
            self.model_index
                .entry(namespaced)
                .or_default()
                .push((name.clone(), model.clone()));
        }

        // Also index from weights (which may include meta-provider sub-models).
        for w in &weights {
            let key = format!("{}/{}", w.provider, w.model);
            self.model_index
                .entry(key)
                .or_default()
                .push((name.clone(), w.model.clone()));
        }

        self.providers.insert(name, RegisteredProvider {
            provider,
            caps,
            weights,
        });
    }

    /// Resolve a model name to a provider. Returns (provider, resolved_model_name).
    ///
    /// Resolution order:
    /// 1. Exact namespaced match (`provider/model`)
    /// 2. Bare model name across all providers
    /// 3. First provider that lists the model in `models()`
    pub fn resolve_model(
        &self,
        model: &str,
        required_caps: ProviderCaps,
    ) -> Option<(&Arc<dyn Provider>, String)> {
        // 1. Check model index.
        if let Some(candidates) = self.model_index.get(model) {
            for (provider_name, resolved_model) in candidates {
                if let Some(reg) = self.providers.get(provider_name) {
                    if reg.caps.satisfies(required_caps) {
                        debug!(%model, provider = %provider_name, "resolved via model index");
                        return Some((&reg.provider, resolved_model.clone()));
                    }
                }
            }
        }

        // 2. Scan all providers for the bare model name.
        for (name, reg) in &self.providers {
            if reg.caps.satisfies(required_caps) && reg.provider.models().iter().any(|m| m == model) {
                debug!(%model, provider = %name, "resolved via provider scan");
                return Some((&reg.provider, model.to_string()));
            }
        }

        warn!(%model, ?required_caps, "no provider satisfies model + capability requirements");
        None
    }

    /// Select the cheapest provider that satisfies the capability requirements.
    /// Returns providers sorted by routing_score (ascending = cheapest first).
    pub fn select_by_cost(
        &self,
        required_caps: ProviderCaps,
        cost_weight: f64,
        latency_weight: f64,
    ) -> Vec<(&Arc<dyn Provider>, &ProviderWeight)> {
        let mut candidates: Vec<_> = self
            .providers
            .values()
            .filter(|reg| reg.caps.satisfies(required_caps))
            .flat_map(|reg| {
                reg.weights.iter().map(move |w| (&reg.provider, w))
            })
            .collect();

        candidates.sort_by(|a, b| {
            let sa = a.1.routing_score(cost_weight, latency_weight);
            let sb = b.1.routing_score(cost_weight, latency_weight);
            sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
        });

        candidates
    }

    /// Get a provider by name.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Provider>> {
        self.providers.get(name).map(|r| &r.provider)
    }

    /// List all registered provider names.
    pub fn list_providers(&self) -> Vec<&str> {
        self.providers.keys().map(|s| s.as_str()).collect()
    }

    /// List all known models (bare + namespaced).
    pub fn list_models(&self) -> Vec<String> {
        self.model_index.keys().cloned().collect()
    }

    /// Get capabilities for a provider.
    pub fn capabilities(&self, provider: &str) -> Option<ProviderCaps> {
        self.providers.get(provider).map(|r| r.caps)
    }

    /// Number of registered providers.
    pub fn provider_count(&self) -> usize {
        self.providers.len()
    }
}

impl Default for ProviderNegotiator {
    fn default() -> Self {
        Self::new()
    }
}

// ── Fallback FSM ─────────────────────────────────────────────────────────

/// Provider fallback state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FallbackState {
    /// Using the primary (preferred) provider.
    Primary,
    /// Primary failed, attempting fallback providers in order.
    Falling { attempt: usize },
    /// All providers exhausted.
    Exhausted,
    /// Recovered back to primary after cool-down.
    Recovered,
}

/// Fallback finite state machine for provider resilience.
///
/// When the primary provider fails (rate limit, 5xx, timeout), the FSM
/// transitions through fallback providers. After a cool-down period,
/// it attempts recovery back to the primary.
///
/// ```text
/// Primary ──(fail)──→ Falling(0) ──(fail)──→ Falling(1) ──…──→ Exhausted
///    ↑                    │                      │
///    └────(recover)───────┴──────(recover)────────┘
/// ```
/// FSM state is guarded by a `std::sync::Mutex` so concurrent
/// requests cannot race on `record_success()` / `record_failure()`.
/// The Mutex protects the inner mutable fields; the chain (immutable after
/// construction) is stored outside.
pub struct FallbackFsm {
    inner: std::sync::Mutex<FallbackFsmInner>,
    /// Ordered list of fallback provider names (primary first).
    chain: Vec<String>,
}

/// Interior mutable state for the FSM, protected by Mutex.
struct FallbackFsmInner {
    state: FallbackState,
    /// Number of consecutive failures for current provider.
    consecutive_failures: usize,
    /// Maximum failures before switching to next fallback.
    max_failures: usize,
    /// When the last failure occurred (for cool-down).
    last_failure: Option<std::time::Instant>,
    /// Cool-down duration before attempting recovery.
    cooldown: std::time::Duration,
}

impl FallbackFsm {
    /// Create a new FSM with a primary provider and fallbacks.
    pub fn new(primary: &str, fallbacks: &[&str]) -> Self {
        let mut chain = vec![primary.to_string()];
        chain.extend(fallbacks.iter().map(|s| s.to_string()));
        Self {
            inner: std::sync::Mutex::new(FallbackFsmInner {
                state: FallbackState::Primary,
                consecutive_failures: 0,
                max_failures: 3,
                last_failure: None,
                cooldown: std::time::Duration::from_secs(60),
            }),
            chain,
        }
    }

    /// Set the maximum consecutive failures before fallback.
    pub fn with_max_failures(self, n: usize) -> Self {
        self.inner.lock().unwrap().max_failures = n;
        self
    }

    /// Set the recovery cool-down period.
    pub fn with_cooldown(self, d: std::time::Duration) -> Self {
        self.inner.lock().unwrap().cooldown = d;
        self
    }

    /// Get the current state.
    pub fn state(&self) -> FallbackState {
        self.inner.lock().unwrap().state
    }

    /// Get the name of the currently selected provider.
    pub fn current_provider(&self) -> Option<&str> {
        let inner = self.inner.lock().unwrap();
        match inner.state {
            FallbackState::Primary | FallbackState::Recovered => {
                self.chain.first().map(|s| s.as_str())
            }
            FallbackState::Falling { attempt } => {
                self.chain.get(attempt + 1).map(|s| s.as_str())
            }
            FallbackState::Exhausted => None,
        }
    }

    /// Record a successful request — resets failure count.
    /// Atomic via Mutex — concurrent calls cannot interleave.
    pub fn record_success(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.consecutive_failures = 0;
        if inner.state != FallbackState::Primary {
            if let Some(last) = inner.last_failure {
                if last.elapsed() >= inner.cooldown {
                    inner.state = FallbackState::Recovered;
                    inner.last_failure = None;
                }
            }
        }
    }

    /// Record a request failure — may trigger state transition.
    /// Atomic via Mutex — concurrent calls see consistent state.
    pub fn record_failure(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.consecutive_failures += 1;
        inner.last_failure = Some(std::time::Instant::now());

        if inner.consecutive_failures >= inner.max_failures {
            inner.consecutive_failures = 0;
            Self::advance_fallback_inner(&mut inner, &self.chain);
        }
    }

    /// Advance to the next fallback provider (inner helper).
    fn advance_fallback_inner(inner: &mut FallbackFsmInner, chain: &[String]) {
        match inner.state {
            FallbackState::Primary => {
                if chain.len() > 1 {
                    inner.state = FallbackState::Falling { attempt: 0 };
                    info!(
                        next = %chain.get(1).unwrap_or(&String::new()),
                        "fallback: switching from primary"
                    );
                } else {
                    inner.state = FallbackState::Exhausted;
                    warn!("fallback: no fallback providers configured");
                }
            }
            FallbackState::Falling { attempt } => {
                let next = attempt + 1;
                if next + 1 < chain.len() {
                    inner.state = FallbackState::Falling { attempt: next };
                    info!(
                        next = %chain.get(next + 1).unwrap_or(&String::new()),
                        "fallback: advancing to next"
                    );
                } else {
                    inner.state = FallbackState::Exhausted;
                    warn!("fallback: all providers exhausted");
                }
            }
            FallbackState::Recovered => {
                if chain.len() > 1 {
                    inner.state = FallbackState::Falling { attempt: 0 };
                } else {
                    inner.state = FallbackState::Exhausted;
                }
            }
            FallbackState::Exhausted => {}
        }
    }

    /// Force recovery to primary (e.g., after a config change).
    pub fn reset(&self) {
        let mut inner = self.inner.lock().unwrap();
        inner.state = FallbackState::Primary;
        inner.consecutive_failures = 0;
        inner.last_failure = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::*;
    use crate::{ProviderError, ProviderRequest, ProviderResponse, StreamChunk};
    use async_trait::async_trait;

    struct MockProvider {
        name: &'static str,
        models: Vec<String>,
    }

    #[async_trait]
    impl Provider for MockProvider {
        fn name(&self) -> &str {
            self.name
        }
        fn models(&self) -> Vec<String> {
            self.models.clone()
        }
        async fn complete(&self, _req: &ProviderRequest) -> Result<ProviderResponse, ProviderError> {
            unimplemented!()
        }
        async fn health_check(&self) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    #[test]
    fn resolve_bare_model() {
        let mut neg = ProviderNegotiator::new();
        neg.register(
            Arc::new(MockProvider {
                name: "openai",
                models: vec!["gpt-4o".into()],
            }),
            OPENAI_CAPS,
            vec![],
        );

        let (provider, model) = neg.resolve_model("gpt-4o", ProviderCaps::NONE).unwrap();
        assert_eq!(provider.name(), "openai");
        assert_eq!(model, "gpt-4o");
    }

    #[test]
    fn resolve_namespaced_model() {
        let mut neg = ProviderNegotiator::new();
        neg.register(
            Arc::new(MockProvider {
                name: "anthropic",
                models: vec!["claude-sonnet-4-20250514".into()],
            }),
            ANTHROPIC_CAPS,
            vec![],
        );

        let (provider, _) = neg.resolve_model("anthropic/claude-sonnet-4-20250514", ProviderCaps::NONE).unwrap();
        assert_eq!(provider.name(), "anthropic");
    }

    #[test]
    fn resolve_filters_by_capability() {
        let mut neg = ProviderNegotiator::new();
        neg.register(
            Arc::new(MockProvider {
                name: "ollama",
                models: vec!["llama3".into()],
            }),
            OLLAMA_CAPS,
            vec![],
        );

        // Ollama doesn't support tool use.
        let result = neg.resolve_model("llama3", ProviderCaps::TOOL_USE);
        assert!(result.is_none());
    }

    #[test]
    fn select_by_cost_orders_correctly() {
        let mut neg = ProviderNegotiator::new();
        neg.register(
            Arc::new(MockProvider {
                name: "ollama",
                models: vec!["llama3".into()],
            }),
            OLLAMA_CAPS,
            vec![ProviderWeight {
                provider: "ollama".into(),
                model: "llama3".into(),
                cost_per_m_input: 0.0,
                cost_per_m_output: 0.0,
                latency_p50_ms: 50,
                caps: OLLAMA_CAPS,
                quality_tier: 2,
            }],
        );
        neg.register(
            Arc::new(MockProvider {
                name: "openai",
                models: vec!["gpt-4o".into()],
            }),
            OPENAI_CAPS,
            vec![ProviderWeight {
                provider: "openai".into(),
                model: "gpt-4o".into(),
                cost_per_m_input: 5.0,
                cost_per_m_output: 15.0,
                latency_p50_ms: 200,
                caps: OPENAI_CAPS,
                quality_tier: 5,
            }],
        );

        let result = neg.select_by_cost(ProviderCaps::TEXT_COMPLETION, 1.0, 0.0);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0.name(), "ollama"); // Cheapest first.
    }

    // ── Fallback FSM tests ────────────────────────────────

    #[test]
    fn fallback_starts_as_primary() {
        let fsm = FallbackFsm::new("anthropic", &["openai", "ollama"]);
        assert_eq!(fsm.state(), FallbackState::Primary);
        assert_eq!(fsm.current_provider(), Some("anthropic"));
    }

    #[test]
    fn fallback_transitions_after_failures() {
        let mut fsm = FallbackFsm::new("anthropic", &["openai", "ollama"])
            .with_max_failures(2);

        fsm.record_failure();
        assert_eq!(fsm.state(), FallbackState::Primary); // 1 failure, need 2

        fsm.record_failure();
        assert_eq!(fsm.state(), FallbackState::Falling { attempt: 0 });
        assert_eq!(fsm.current_provider(), Some("openai"));
    }

    #[test]
    fn fallback_exhausts_all_providers() {
        let mut fsm = FallbackFsm::new("a", &["b"])
            .with_max_failures(1);

        fsm.record_failure(); // a → b
        assert_eq!(fsm.current_provider(), Some("b"));

        fsm.record_failure(); // b → exhausted
        assert_eq!(fsm.state(), FallbackState::Exhausted);
        assert_eq!(fsm.current_provider(), None);
    }

    #[test]
    fn fallback_success_resets_counter() {
        let mut fsm = FallbackFsm::new("a", &["b"])
            .with_max_failures(3);

        fsm.record_failure();
        fsm.record_failure();
        fsm.record_success(); // resets counter
        fsm.record_failure(); // only 1 failure now
        assert_eq!(fsm.state(), FallbackState::Primary);
    }

    #[test]
    fn fallback_reset_returns_to_primary() {
        let mut fsm = FallbackFsm::new("a", &["b"])
            .with_max_failures(1);

        fsm.record_failure();
        assert_eq!(fsm.state(), FallbackState::Falling { attempt: 0 });

        fsm.reset();
        assert_eq!(fsm.state(), FallbackState::Primary);
        assert_eq!(fsm.current_provider(), Some("a"));
    }
}
