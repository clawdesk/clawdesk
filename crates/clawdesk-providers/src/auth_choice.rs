//! Provider auth choice framework — declarative multi-method auth.
//!
//! ## Problem
//!
//! Auth resolution was hardcoded per provider, creating O(N) match arms
//! that each duplicate the pattern: check vault → check env → prompt user.
//! Adding a new provider required touching the central auth dispatcher.
//!
//! ## Design
//!
//! Auth choice selection is a decision tree over a finite lattice:
//!   `L = Method × Provider × Credential`
//!
//! The lattice ordering is by user friction:
//!   `None ≤ ApiKey ≤ Token ≤ OAuth ≤ DeviceCode ≤ Custom`
//!
//! The selector picks the infimum (least friction) method that has valid
//! credentials, implementing a meet operation on the lattice.
//!
//! ## Credential Resolution Order
//!
//! 1. Credential vault (encrypted, user-stored)
//! 2. Environment variables
//! 3. Config file (plain text, legacy)
//! 4. Missing → prompt user via wizard
//!
//! This ordering is a priority queue — first valid source wins.

use serde::{Deserialize, Serialize};

/// Resolved credential — the output of auth resolution.
#[derive(Debug, Clone)]
pub struct ResolvedCredential {
    /// The auth method that produced this credential.
    pub method: AuthMethod,
    /// The credential value (API key, token, etc.).
    /// SECURITY: This is sensitive — never log or serialize to disk without encryption.
    pub value: String,
    /// Source that provided the credential.
    pub source: CredentialSource,
    /// Optional supplementary data (e.g., base URL for Azure).
    pub metadata: std::collections::HashMap<String, String>,
}

/// Where a credential was resolved from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    /// From the encrypted credential vault.
    Vault,
    /// From an environment variable.
    Environment,
    /// From the config file (legacy, less secure).
    Config,
    /// User provided interactively during setup.
    UserInput,
    /// OAuth flow completed.
    OAuthFlow,
    /// Device code flow completed.
    DeviceCodeFlow,
}

/// Auth method — re-exported locally for use in resolution.
pub use crate::plugin_provider::AuthMethod;

/// Auth choice preference — user override for auth method selection.
///
/// Users can express a preferred auth method per provider, overriding
/// the default friction ordering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthPreference {
    /// Provider ID.
    pub provider_id: String,
    /// Preferred auth choice ID (from the manifest).
    pub preferred_choice_id: String,
}

/// Auth resolution context — inputs for credential resolution.
pub struct AuthResolutionContext {
    /// Environment variables (pre-scanned).
    pub env_vars: std::collections::HashMap<String, String>,
    /// User preferences for auth methods.
    pub preferences: Vec<AuthPreference>,
    /// Vault lookup function: `(provider_id, credential_name) → Optional<value>`.
    pub vault_lookup: Option<Box<dyn Fn(&str, &str) -> Option<String> + Send + Sync>>,
}

impl AuthResolutionContext {
    /// Try to resolve a credential for a provider, checking sources in priority order.
    ///
    /// ## Resolution algorithm
    ///
    /// For each auth choice on the provider (sorted by preference):
    /// 1. Check vault for `(provider_id, choice_id)`
    /// 2. Check env var specified in the auth choice
    /// 3. Skip to next choice
    ///
    /// Returns the first successful resolution, or `None` if all fail.
    pub fn resolve(
        &self,
        provider_id: &str,
        auth_choices: &[crate::plugin_provider::ProviderAuthChoice],
    ) -> Option<ResolvedCredential> {
        // Sort choices: user preference first, then by friction ordering
        let mut sorted_choices: Vec<_> = auth_choices.iter().collect();
        sorted_choices.sort_by(|a, b| {
            let a_pref = self.is_preferred(provider_id, &a.choice_id);
            let b_pref = self.is_preferred(provider_id, &b.choice_id);
            match (a_pref, b_pref) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => friction_ord(&a.method).cmp(&friction_ord(&b.method)),
            }
        });

        for choice in sorted_choices {
            // 1. Check vault
            if let Some(ref lookup) = self.vault_lookup {
                if let Some(value) = lookup(provider_id, &choice.choice_id) {
                    return Some(ResolvedCredential {
                        method: choice.method,
                        value,
                        source: CredentialSource::Vault,
                        metadata: std::collections::HashMap::new(),
                    });
                }
            }

            // 2. Check env var
            if let Some(ref env_name) = choice.env_var {
                if let Some(value) = self.env_vars.get(env_name.as_str()) {
                    return Some(ResolvedCredential {
                        method: choice.method,
                        value: value.clone(),
                        source: CredentialSource::Environment,
                        metadata: std::collections::HashMap::new(),
                    });
                }
            }
        }

        None
    }

    fn is_preferred(&self, provider_id: &str, choice_id: &str) -> bool {
        self.preferences.iter().any(|p| {
            p.provider_id == provider_id && p.preferred_choice_id == choice_id
        })
    }
}

/// Friction ordering for auth methods.
/// Lower = less user friction = preferred.
fn friction_ord(method: &AuthMethod) -> u8 {
    match method {
        AuthMethod::None => 0,
        AuthMethod::ApiKey => 1,
        AuthMethod::Token => 2,
        AuthMethod::OAuth => 3,
        AuthMethod::DeviceCode => 4,
        AuthMethod::Custom => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin_provider::ProviderAuthChoice;

    #[test]
    fn resolve_from_env() {
        let mut env = std::collections::HashMap::new();
        env.insert("OPENAI_API_KEY".into(), "sk-test-123".into());

        let ctx = AuthResolutionContext {
            env_vars: env,
            preferences: vec![],
            vault_lookup: None,
        };

        let choices = vec![ProviderAuthChoice {
            method: AuthMethod::ApiKey,
            choice_id: "openai-api-key".into(),
            label: "API Key".into(),
            hint: None,
            group_id: None,
            group_label: None,
            cli_flag: None,
            env_var: Some("OPENAI_API_KEY".into()),
        }];

        let resolved = ctx.resolve("openai", &choices);
        assert!(resolved.is_some());
        let cred = resolved.unwrap();
        assert_eq!(cred.source, CredentialSource::Environment);
        assert_eq!(cred.value, "sk-test-123");
    }

    #[test]
    fn vault_takes_priority_over_env() {
        let mut env = std::collections::HashMap::new();
        env.insert("OPENAI_API_KEY".into(), "sk-env".into());

        let ctx = AuthResolutionContext {
            env_vars: env,
            preferences: vec![],
            vault_lookup: Some(Box::new(|_provider, _choice| {
                Some("sk-vault-secret".into())
            })),
        };

        let choices = vec![ProviderAuthChoice {
            method: AuthMethod::ApiKey,
            choice_id: "openai-api-key".into(),
            label: "API Key".into(),
            hint: None,
            group_id: None,
            group_label: None,
            cli_flag: None,
            env_var: Some("OPENAI_API_KEY".into()),
        }];

        let resolved = ctx.resolve("openai", &choices).unwrap();
        assert_eq!(resolved.source, CredentialSource::Vault);
        assert_eq!(resolved.value, "sk-vault-secret");
    }

    #[test]
    fn preference_overrides_friction_order() {
        let env = std::collections::HashMap::new();

        let ctx = AuthResolutionContext {
            env_vars: env,
            preferences: vec![AuthPreference {
                provider_id: "test".into(),
                preferred_choice_id: "oauth".into(),
            }],
            vault_lookup: Some(Box::new(|_provider, choice| {
                // Both choices have vault entries
                Some(format!("cred-for-{choice}"))
            })),
        };

        let choices = vec![
            ProviderAuthChoice {
                method: AuthMethod::ApiKey,
                choice_id: "api-key".into(),
                label: "API Key".into(),
                hint: None, group_id: None, group_label: None,
                cli_flag: None, env_var: None,
            },
            ProviderAuthChoice {
                method: AuthMethod::OAuth,
                choice_id: "oauth".into(),
                label: "OAuth".into(),
                hint: None, group_id: None, group_label: None,
                cli_flag: None, env_var: None,
            },
        ];

        let resolved = ctx.resolve("test", &choices).unwrap();
        // OAuth should win because it's the preferred choice
        assert_eq!(resolved.value, "cred-for-oauth");
    }

    #[test]
    fn friction_ordering_is_monotonic() {
        let methods = [
            AuthMethod::None,
            AuthMethod::ApiKey,
            AuthMethod::Token,
            AuthMethod::OAuth,
            AuthMethod::DeviceCode,
            AuthMethod::Custom,
        ];
        for w in methods.windows(2) {
            assert!(
                friction_ord(&w[0]) <= friction_ord(&w[1]),
                "{:?} should have lower friction than {:?}",
                w[0], w[1]
            );
        }
    }
}
