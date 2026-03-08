//! C3: Deep Link Handler for `clawdesk://` URI scheme.
//!
//! Handles incoming deep links to trigger specific actions within the app.
//!
//! ## Supported URIs
//!
//! ```text
//! clawdesk://chat/new                — Open a new chat
//! clawdesk://chat/{id}               — Open an existing chat
//! clawdesk://skill/install/{id}      — Install a skill from the store
//! clawdesk://provider/add/{provider} — Open provider configuration
//! clawdesk://settings                — Open settings
//! clawdesk://settings/{section}      — Open specific settings section
//! ```
//!
//! ## Security
//!
//! All deep link parameters are sanitized:
//! - IDs are validated against allowed character sets
//! - Path traversal is blocked
//! - Query length is bounded (max 2048 chars)

use serde::{Deserialize, Serialize};
use clawdesk_types::truncate_to_char_boundary;
use tracing::{debug, info, warn};

/// Maximum URI length to prevent abuse.
const MAX_URI_LENGTH: usize = 2048;

/// Characters allowed in an identifier segment.
fn is_valid_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.'
}

/// A parsed deep link action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum DeepLinkAction {
    /// Open a new chat.
    NewChat,
    /// Open an existing chat by ID.
    OpenChat { chat_id: String },
    /// Install a skill from the store.
    InstallSkill { skill_id: String },
    /// Open provider configuration.
    AddProvider { provider: String },
    /// Open settings (optionally to a specific section).
    OpenSettings { section: Option<String> },
    /// Unknown or malformed URI.
    Unknown { uri: String },
}

impl DeepLinkAction {
    /// Frontend event name for this action.
    pub fn event_name(&self) -> &'static str {
        match self {
            Self::NewChat => "deep-link-new-chat",
            Self::OpenChat { .. } => "deep-link-open-chat",
            Self::InstallSkill { .. } => "deep-link-install-skill",
            Self::AddProvider { .. } => "deep-link-add-provider",
            Self::OpenSettings { .. } => "deep-link-open-settings",
            Self::Unknown { .. } => "deep-link-unknown",
        }
    }
}

/// Parse and validate a `clawdesk://` deep link URI.
///
/// Returns a typed action if the URI is valid, or `Unknown` for malformed URIs.
pub fn parse_deep_link(uri: &str) -> DeepLinkAction {
    // Length check
    if uri.len() > MAX_URI_LENGTH {
        warn!(len = uri.len(), "Deep link URI too long, rejecting");
        return DeepLinkAction::Unknown {
            uri: {
                let end = truncate_to_char_boundary(uri, 64);
                uri[..end].to_string() + "..."
            },
        };
    }

    // Strip scheme
    let path = match uri.strip_prefix("clawdesk://") {
        Some(p) => p,
        None => {
            warn!(uri, "Not a clawdesk:// URI");
            return DeepLinkAction::Unknown {
                uri: uri.to_string(),
            };
        }
    };

    // Strip trailing slashes and query string for basic routing
    let path = path.split('?').next().unwrap_or(path);
    let path = path.trim_end_matches('/');

    // Split into segments
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    debug!(segments = ?segments, "Parsing deep link");

    match segments.as_slice() {
        // clawdesk://chat/new
        ["chat", "new"] => DeepLinkAction::NewChat,

        // clawdesk://chat/{id}
        ["chat", id] => {
            let id = *id;
            if id.chars().all(is_valid_id_char) && !id.is_empty() {
                DeepLinkAction::OpenChat {
                    chat_id: id.to_string(),
                }
            } else {
                warn!(id, "Invalid chat ID in deep link");
                DeepLinkAction::Unknown {
                    uri: uri.to_string(),
                }
            }
        }

        // clawdesk://skill/install/{id}
        ["skill", "install", id] => {
            let id = *id;
            if id.chars().all(|c| is_valid_id_char(c) || c == '/') && !id.is_empty() {
                DeepLinkAction::InstallSkill {
                    skill_id: id.to_string(),
                }
            } else {
                warn!(id, "Invalid skill ID in deep link");
                DeepLinkAction::Unknown {
                    uri: uri.to_string(),
                }
            }
        }

        // clawdesk://provider/add/{provider}
        ["provider", "add", provider] => {
            let provider = *provider;
            if provider.chars().all(is_valid_id_char) && !provider.is_empty() {
                DeepLinkAction::AddProvider {
                    provider: provider.to_string(),
                }
            } else {
                warn!(provider, "Invalid provider in deep link");
                DeepLinkAction::Unknown {
                    uri: uri.to_string(),
                }
            }
        }

        // clawdesk://settings
        ["settings"] => DeepLinkAction::OpenSettings { section: None },

        // clawdesk://settings/{section}
        ["settings", section] => {
            let section = *section;
            if section.chars().all(is_valid_id_char) {
                DeepLinkAction::OpenSettings {
                    section: Some(section.to_string()),
                }
            } else {
                warn!(section, "Invalid settings section in deep link");
                DeepLinkAction::Unknown {
                    uri: uri.to_string(),
                }
            }
        }

        _ => {
            info!(path, "Unknown deep link path");
            DeepLinkAction::Unknown {
                uri: uri.to_string(),
            }
        }
    }
}

/// Sanitize a deep link string, removing potentially dangerous characters.
pub fn sanitize_uri(uri: &str) -> String {
    uri.chars()
        .filter(|c| {
            c.is_ascii_alphanumeric()
                || *c == ':'
                || *c == '/'
                || *c == '-'
                || *c == '_'
                || *c == '.'
                || *c == '?'
                || *c == '='
                || *c == '&'
        })
        .take(MAX_URI_LENGTH)
        .collect()
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_new_chat() {
        assert_eq!(
            parse_deep_link("clawdesk://chat/new"),
            DeepLinkAction::NewChat
        );
    }

    #[test]
    fn parse_open_chat() {
        assert_eq!(
            parse_deep_link("clawdesk://chat/abc-123"),
            DeepLinkAction::OpenChat {
                chat_id: "abc-123".into()
            }
        );
    }

    #[test]
    fn parse_install_skill() {
        assert_eq!(
            parse_deep_link("clawdesk://skill/install/code-review"),
            DeepLinkAction::InstallSkill {
                skill_id: "code-review".into()
            }
        );
    }

    #[test]
    fn parse_add_provider() {
        assert_eq!(
            parse_deep_link("clawdesk://provider/add/anthropic"),
            DeepLinkAction::AddProvider {
                provider: "anthropic".into()
            }
        );
    }

    #[test]
    fn parse_settings() {
        assert_eq!(
            parse_deep_link("clawdesk://settings"),
            DeepLinkAction::OpenSettings { section: None }
        );
    }

    #[test]
    fn parse_settings_section() {
        assert_eq!(
            parse_deep_link("clawdesk://settings/providers"),
            DeepLinkAction::OpenSettings {
                section: Some("providers".into())
            }
        );
    }

    #[test]
    fn parse_trailing_slash() {
        assert_eq!(
            parse_deep_link("clawdesk://chat/new/"),
            DeepLinkAction::NewChat
        );
    }

    #[test]
    fn parse_unknown_path() {
        let result = parse_deep_link("clawdesk://foo/bar");
        assert!(matches!(result, DeepLinkAction::Unknown { .. }));
    }

    #[test]
    fn parse_wrong_scheme() {
        let result = parse_deep_link("https://example.com");
        assert!(matches!(result, DeepLinkAction::Unknown { .. }));
    }

    #[test]
    fn parse_too_long() {
        let long_uri = format!("clawdesk://chat/{}", "a".repeat(3000));
        let result = parse_deep_link(&long_uri);
        assert!(matches!(result, DeepLinkAction::Unknown { .. }));
    }

    #[test]
    fn sanitize_removes_dangerous_chars() {
        let dirty = "clawdesk://chat/<script>alert(1)</script>";
        let clean = sanitize_uri(dirty);
        assert!(!clean.contains('<'));
        assert!(!clean.contains('>'));
        assert!(!clean.contains('('));
    }

    #[test]
    fn event_names() {
        assert_eq!(DeepLinkAction::NewChat.event_name(), "deep-link-new-chat");
        assert_eq!(
            DeepLinkAction::OpenChat {
                chat_id: "x".into()
            }
            .event_name(),
            "deep-link-open-chat"
        );
    }
}
