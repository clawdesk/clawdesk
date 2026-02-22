//! Auth profile store — per-profile cooldown with weighted round-robin.
//!
//! Manages multiple API key profiles for a single provider, enabling:
//! - **Key rotation** — distribute requests across profiles to avoid per-key rate limits.
//! - **Cooldown tracking** — mark profiles as rate-limited with server-directed `Retry-After`.
//! - **Failure penalty** — reduce selection weight on repeated failures.
//! - **Expiry management** — BTreeMap for efficient expiry tracking.
//!
//! ## Algorithm
//!
//! Selection uses weighted round-robin with failure penalty:
//! ```text
//! effective_weight(p) = base_weight(p) × (1 - penalty_factor)^consecutive_failures(p)
//! ```
//!
//! Profiles on cooldown are excluded from selection until their cooldown
//! expires. The `BTreeMap<Instant, ProfileId>` allows O(log n) expiry checks.
//!
//! ## Complexity
//! - Select: O(K) where K = number of profiles
//! - Record failure: O(log K) for BTreeMap insertion
//! - Check cooldowns: O(expired) amortised

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

/// A unique identifier for an auth profile.
pub type ProfileId = String;

/// An individual auth profile with key material and state.
#[derive(Debug, Clone)]
pub struct AuthProfile {
    /// Unique profile identifier.
    pub id: ProfileId,
    /// The API key or token.
    pub api_key: String,
    /// Optional organization ID (e.g., for OpenAI).
    pub org_id: Option<String>,
    /// Base selection weight (higher = preferred).
    pub base_weight: f64,
    /// Number of consecutive failures.
    pub consecutive_failures: u32,
    /// Total lifetime requests.
    pub total_requests: u64,
    /// Total lifetime failures.
    pub total_failures: u64,
    /// Whether this profile is currently on cooldown.
    pub on_cooldown: bool,
    /// When the cooldown expires (if on cooldown).
    pub cooldown_until: Option<Instant>,
    /// When this profile was last used.
    pub last_used: Option<Instant>,
}

impl AuthProfile {
    pub fn new(id: impl Into<String>, api_key: impl Into<String>, base_weight: f64) -> Self {
        Self {
            id: id.into(),
            api_key: api_key.into(),
            org_id: None,
            base_weight,
            consecutive_failures: 0,
            total_requests: 0,
            total_failures: 0,
            on_cooldown: false,
            cooldown_until: None,
            last_used: None,
        }
    }

    /// Effective weight after failure penalty.
    ///
    /// `effective_weight = base_weight × (1 - penalty_factor)^consecutive_failures`
    pub fn effective_weight(&self, penalty_factor: f64) -> f64 {
        let penalty = (1.0 - penalty_factor).powi(self.consecutive_failures as i32);
        self.base_weight * penalty
    }

    /// Whether this profile is available for selection (not on cooldown).
    pub fn is_available(&self) -> bool {
        if !self.on_cooldown {
            return true;
        }
        // Check if cooldown has expired
        match self.cooldown_until {
            Some(until) => Instant::now() >= until,
            None => true,
        }
    }

    /// Record a successful request.
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.total_requests += 1;
        self.on_cooldown = false;
        self.cooldown_until = None;
        self.last_used = Some(Instant::now());
    }

    /// Record a failed request with optional cooldown.
    pub fn record_failure(&mut self, cooldown: Option<Duration>) {
        self.consecutive_failures += 1;
        self.total_requests += 1;
        self.total_failures += 1;
        self.last_used = Some(Instant::now());

        if let Some(duration) = cooldown {
            self.on_cooldown = true;
            self.cooldown_until = Some(Instant::now() + duration);
        }
    }
}

/// Store managing multiple auth profiles for a provider.
pub struct AuthProfileStore {
    /// Profiles indexed by ID.
    profiles: HashMap<ProfileId, AuthProfile>,
    /// Expiry index: cooldown_until → profile_id.
    expiry_index: BTreeMap<Instant, ProfileId>,
    /// Failure penalty factor (0.0–1.0). Default 0.3.
    pub penalty_factor: f64,
    /// Default cooldown duration when server doesn't specify Retry-After.
    pub default_cooldown: Duration,
}

impl AuthProfileStore {
    pub fn new() -> Self {
        Self {
            profiles: HashMap::new(),
            expiry_index: BTreeMap::new(),
            penalty_factor: 0.3,
            default_cooldown: Duration::from_secs(60),
        }
    }

    /// Add or update a profile.
    pub fn upsert(&mut self, profile: AuthProfile) {
        self.profiles.insert(profile.id.clone(), profile);
    }

    /// Remove a profile by ID.
    pub fn remove(&mut self, id: &str) -> Option<AuthProfile> {
        let profile = self.profiles.remove(id)?;
        // Clean up expiry index
        self.expiry_index.retain(|_, pid| pid != id);
        Some(profile)
    }

    /// Sweep expired cooldowns. Call periodically or before selection.
    pub fn sweep_expired(&mut self) {
        let now = Instant::now();
        let expired: Vec<Instant> = self
            .expiry_index
            .range(..=now)
            .map(|(k, _)| *k)
            .collect();

        for instant in expired {
            if let Some(pid) = self.expiry_index.remove(&instant) {
                if let Some(profile) = self.profiles.get_mut(&pid) {
                    profile.on_cooldown = false;
                    profile.cooldown_until = None;
                }
            }
        }
    }

    /// Select the best available profile using weighted round-robin.
    ///
    /// Returns `None` if all profiles are on cooldown.
    pub fn select(&mut self) -> Option<&AuthProfile> {
        self.sweep_expired();

        let penalty = self.penalty_factor;
        let best = self
            .profiles
            .values()
            .filter(|p| p.is_available())
            .max_by(|a, b| {
                let wa = a.effective_weight(penalty);
                let wb = b.effective_weight(penalty);
                wa.partial_cmp(&wb).unwrap_or(std::cmp::Ordering::Equal)
            });

        best
    }

    /// Select and return a mutable reference (for recording success/failure).
    pub fn select_mut(&mut self) -> Option<&mut AuthProfile> {
        self.sweep_expired();

        let penalty = self.penalty_factor;

        // Find the best profile ID first
        let best_id = self
            .profiles
            .values()
            .filter(|p| p.is_available())
            .max_by(|a, b| {
                let wa = a.effective_weight(penalty);
                let wb = b.effective_weight(penalty);
                wa.partial_cmp(&wb).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|p| p.id.clone());

        best_id.and_then(move |id| self.profiles.get_mut(&id))
    }

    /// Record a failure for a specific profile, with optional server-directed cooldown.
    pub fn record_failure(&mut self, profile_id: &str, cooldown: Option<Duration>) {
        let cd = cooldown.unwrap_or(self.default_cooldown);

        if let Some(profile) = self.profiles.get_mut(profile_id) {
            profile.record_failure(Some(cd));

            if let Some(until) = profile.cooldown_until {
                self.expiry_index
                    .insert(until, profile_id.to_string());
            }
        }
    }

    /// Record a success for a specific profile.
    pub fn record_success(&mut self, profile_id: &str) {
        if let Some(profile) = self.profiles.get_mut(profile_id) {
            profile.record_success();
        }
    }

    /// Get a profile by ID.
    pub fn get(&self, id: &str) -> Option<&AuthProfile> {
        self.profiles.get(id)
    }

    /// Number of profiles.
    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }

    /// Number of profiles currently available (not on cooldown).
    pub fn available_count(&self) -> usize {
        self.profiles.values().filter(|p| p.is_available()).count()
    }

    /// All profile IDs.
    pub fn profile_ids(&self) -> Vec<&str> {
        self.profiles.keys().map(|k| k.as_str()).collect()
    }
}

impl Default for AuthProfileStore {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_effective_weight_degrades_on_failure() {
        let mut p = AuthProfile::new("p1", "key-1", 1.0);
        assert!((p.effective_weight(0.3) - 1.0).abs() < f64::EPSILON);

        p.consecutive_failures = 1;
        let w1 = p.effective_weight(0.3);
        assert!((w1 - 0.7).abs() < f64::EPSILON);

        p.consecutive_failures = 3;
        let w3 = p.effective_weight(0.3);
        assert!(w3 < 0.5); // 0.7^3 = 0.343
    }

    #[test]
    fn test_select_highest_weight() {
        let mut store = AuthProfileStore::new();
        store.upsert(AuthProfile::new("low", "key-1", 0.5));
        store.upsert(AuthProfile::new("high", "key-2", 1.0));

        let selected = store.select().unwrap();
        assert_eq!(selected.id, "high");
    }

    #[test]
    fn test_cooldown_excludes_profile() {
        let mut store = AuthProfileStore::new();
        let mut p1 = AuthProfile::new("p1", "key-1", 1.0);
        p1.on_cooldown = true;
        p1.cooldown_until = Some(Instant::now() + Duration::from_secs(60));
        store.upsert(p1);
        store.upsert(AuthProfile::new("p2", "key-2", 0.5));

        let selected = store.select().unwrap();
        assert_eq!(selected.id, "p2"); // p1 is on cooldown
    }

    #[test]
    fn test_record_failure_adds_cooldown() {
        let mut store = AuthProfileStore::new();
        store.upsert(AuthProfile::new("p1", "key-1", 1.0));

        store.record_failure("p1", Some(Duration::from_secs(30)));

        let p = store.get("p1").unwrap();
        assert!(p.on_cooldown);
        assert_eq!(p.consecutive_failures, 1);
    }

    #[test]
    fn test_record_success_clears_failures() {
        let mut store = AuthProfileStore::new();
        store.upsert(AuthProfile::new("p1", "key-1", 1.0));

        store.record_failure("p1", Some(Duration::from_secs(30)));
        store.record_success("p1");

        let p = store.get("p1").unwrap();
        assert!(!p.on_cooldown);
        assert_eq!(p.consecutive_failures, 0);
    }

    #[test]
    fn test_sweep_expired() {
        let mut store = AuthProfileStore::new();
        let mut p1 = AuthProfile::new("p1", "key-1", 1.0);
        p1.on_cooldown = true;
        p1.cooldown_until = Some(Instant::now() - Duration::from_secs(1)); // already expired
        store.upsert(p1.clone());
        store.expiry_index.insert(
            Instant::now() - Duration::from_secs(1),
            "p1".to_string(),
        );

        store.sweep_expired();

        let p = store.get("p1").unwrap();
        assert!(!p.on_cooldown);
    }

    #[test]
    fn test_all_on_cooldown_returns_none() {
        let mut store = AuthProfileStore::new();
        let mut p1 = AuthProfile::new("p1", "key-1", 1.0);
        p1.on_cooldown = true;
        p1.cooldown_until = Some(Instant::now() + Duration::from_secs(60));
        store.upsert(p1);

        assert!(store.select().is_none());
    }
}
