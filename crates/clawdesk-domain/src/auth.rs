//! Auth profile rotation with cooldown ring buffer.
//!
//! Circular buffer FSM with lazy cooldown expiry.
//! Round-robin fairness guaranteed by cursor increment.
//! Exponential backoff with cap prevents thundering-herd.

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// An authentication profile for an LLM provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthProfile {
    pub id: String,
    pub provider: String,
    /// Reference to keychain entry (preferred)
    pub api_key_ref: Option<String>,
    /// Direct API key (dev/testing only)
    pub api_key: Option<String>,
    pub enabled: bool,
}

/// Cooldown state for a profile.
#[derive(Debug, Clone)]
pub enum CooldownState {
    Available,
    CoolingDown {
        until: Instant,
        reason: CooldownReason,
    },
    Disabled {
        reason: String,
    },
}

/// Why a profile is cooling down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CooldownReason {
    RateLimit,
    AuthFailure,
    ServerError,
    Timeout,
}

/// Circular buffer for auth profile rotation.
///
/// Selection is O(n) worst case (full ring scan), O(1) expected
/// with low failure rate. Cooldown expiry is lazy (checked at selection time).
pub struct AuthRing {
    profiles: Vec<AuthProfile>,
    cursor: usize,
    cooldowns: Vec<CooldownState>,
}

impl AuthRing {
    pub fn new(profiles: Vec<AuthProfile>) -> Self {
        let n = profiles.len();
        Self {
            profiles,
            cursor: 0,
            cooldowns: vec![CooldownState::Available; n],
        }
    }

    /// Number of profiles in the ring.
    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }

    /// Select the next available profile, skipping cooled-down ones.
    /// Returns None only if ALL profiles are cooling down or disabled.
    ///
    /// Hoists `Instant::now()` outside the scan loop to avoid N syscalls
    /// (clock_gettime) when scanning N profiles.
    pub fn next_available(&mut self) -> Option<(usize, &AuthProfile)> {
        let n = self.profiles.len();
        if n == 0 {
            return None;
        }

        let now = Instant::now();
        for _ in 0..n {
            self.cursor = (self.cursor + 1) % n;
            match &self.cooldowns[self.cursor] {
                CooldownState::Available => {
                    if self.profiles[self.cursor].enabled {
                        return Some((self.cursor, &self.profiles[self.cursor]));
                    }
                }
                CooldownState::CoolingDown { until, .. } => {
                    if now >= *until {
                        // Cooldown expired — mark available
                        self.cooldowns[self.cursor] = CooldownState::Available;
                        if self.profiles[self.cursor].enabled {
                            return Some((self.cursor, &self.profiles[self.cursor]));
                        }
                    }
                }
                CooldownState::Disabled { .. } => continue,
            }
        }

        None // All exhausted
    }

    /// Mark a profile as rate-limited with exponential backoff cooldown.
    ///
    /// Backoff sequence: T_k = base × 2^k, capped at base × 32.
    /// Total wait for k retries: O(T_max) since k is capped at 5.
    pub fn mark_rate_limited(&mut self, idx: usize, attempt: u32) {
        if idx < self.cooldowns.len() {
            let base = Duration::from_secs(30);
            let backoff = base * 2u32.pow(attempt.min(5));
            self.cooldowns[idx] = CooldownState::CoolingDown {
                until: Instant::now() + backoff,
                reason: CooldownReason::RateLimit,
            };
        }
    }

    /// Mark a profile as having an auth failure.
    pub fn mark_auth_failure(&mut self, idx: usize) {
        if idx < self.cooldowns.len() {
            self.cooldowns[idx] = CooldownState::CoolingDown {
                until: Instant::now() + Duration::from_secs(300), // 5 min
                reason: CooldownReason::AuthFailure,
            };
        }
    }

    /// Mark a profile as having a server error with backoff.
    pub fn mark_server_error(&mut self, idx: usize, attempt: u32) {
        if idx < self.cooldowns.len() {
            let base = Duration::from_secs(10);
            let backoff = base * 2u32.pow(attempt.min(4));
            self.cooldowns[idx] = CooldownState::CoolingDown {
                until: Instant::now() + backoff,
                reason: CooldownReason::ServerError,
            };
        }
    }

    /// Permanently disable a profile.
    pub fn disable(&mut self, idx: usize, reason: &str) {
        if idx < self.cooldowns.len() {
            self.cooldowns[idx] = CooldownState::Disabled {
                reason: reason.to_string(),
            };
        }
    }

    /// Re-enable a disabled profile.
    pub fn enable(&mut self, idx: usize) {
        if idx < self.cooldowns.len() {
            self.cooldowns[idx] = CooldownState::Available;
        }
    }

    /// Get the number of currently available profiles.
    ///
    /// Single `Instant::now()` call — avoids N clock syscalls.
    pub fn available_count(&self) -> usize {
        let now = Instant::now();
        self.cooldowns
            .iter()
            .enumerate()
            .filter(|(i, c)| {
                self.profiles[*i].enabled
                    && match c {
                        CooldownState::Available => true,
                        CooldownState::CoolingDown { until, .. } => now >= *until,
                        CooldownState::Disabled { .. } => false,
                    }
            })
            .count()
    }
}

// Manual Clone because Instant doesn't impl Serialize
impl std::fmt::Debug for AuthRing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthRing")
            .field("profiles", &self.profiles.len())
            .field("cursor", &self.cursor)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_profile(id: &str, provider: &str) -> AuthProfile {
        AuthProfile {
            id: id.to_string(),
            provider: provider.to_string(),
            api_key_ref: None,
            api_key: Some(format!("sk-{}", id)),
            enabled: true,
        }
    }

    #[test]
    fn test_round_robin_selection() {
        let profiles = vec![
            make_profile("a", "anthropic"),
            make_profile("b", "anthropic"),
            make_profile("c", "openai"),
        ];
        let mut ring = AuthRing::new(profiles);

        let (i1, _) = ring.next_available().unwrap();
        let (i2, _) = ring.next_available().unwrap();
        let (i3, _) = ring.next_available().unwrap();

        // Round-robin: should cycle through all three
        assert_ne!(i1, i2);
        assert_ne!(i2, i3);
    }

    #[test]
    fn test_cooldown_skips_profile() {
        let profiles = vec![
            make_profile("a", "anthropic"),
            make_profile("b", "anthropic"),
        ];
        let mut ring = AuthRing::new(profiles);

        let (idx, _) = ring.next_available().unwrap();
        ring.mark_rate_limited(idx, 0);

        // Next selection should skip the cooled-down profile
        let (idx2, _) = ring.next_available().unwrap();
        assert_ne!(idx, idx2);
    }

    #[test]
    fn test_empty_ring_returns_none() {
        let mut ring = AuthRing::new(vec![]);
        assert!(ring.next_available().is_none());
    }

    #[test]
    fn test_all_disabled_returns_none() {
        let profiles = vec![make_profile("a", "anthropic")];
        let mut ring = AuthRing::new(profiles);
        ring.disable(0, "test");
        assert!(ring.next_available().is_none());
    }
}
