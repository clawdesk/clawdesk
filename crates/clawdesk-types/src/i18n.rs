//! Internationalization (i18n) framework with Fluent-like message resolution.
//!
//! Zero-dependency on async runtimes — uses `std::sync::RwLock`.
//!
//! ## Locale Negotiation (RFC 4647)
//!
//! O(n × m) where n = user's preferred locales, m = available locales.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::RwLock;

// ─────────────────────────────────────────────────────────────────────────────
// Types
// ─────────────────────────────────────────────────────────────────────────────

/// A locale identifier (e.g., "en-US", "zh-CN", "ja").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Locale(pub String);

impl Locale {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Language subtag (e.g., "en" from "en-US").
    pub fn language(&self) -> &str {
        self.0.split('-').next().unwrap_or(&self.0)
    }

    /// Full tag.
    pub fn tag(&self) -> &str {
        &self.0
    }
}

impl Default for Locale {
    fn default() -> Self {
        Self("en".to_string())
    }
}

impl std::fmt::Display for Locale {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A message bundle for a single locale.
#[derive(Debug, Clone, Default)]
pub struct MessageBundle {
    pub locale: Locale,
    pub messages: HashMap<String, String>,
}

impl MessageBundle {
    pub fn new(locale: Locale) -> Self {
        Self {
            locale,
            messages: HashMap::new(),
        }
    }

    pub fn add(&mut self, id: impl Into<String>, pattern: impl Into<String>) {
        self.messages.insert(id.into(), pattern.into());
    }

    /// Resolve a message with `{arg}` substitution. O(m × k).
    pub fn format(&self, id: &str, args: &HashMap<String, String>) -> Option<String> {
        let pattern = self.messages.get(id)?;
        let mut result = pattern.clone();
        for (key, value) in args {
            result = result.replace(&format!("{{{key}}}"), value);
        }
        Some(result)
    }

    pub fn get(&self, id: &str) -> Option<&str> {
        self.messages.get(id).map(|s| s.as_str())
    }

    /// Parse from Fluent-like format (key = value, one per line).
    pub fn parse_ftl(locale: Locale, content: &str) -> Self {
        let mut bundle = Self::new(locale);
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                bundle.add(key.trim(), value.trim());
            }
        }
        bundle
    }
}

/// Arguments for message formatting.
pub type MessageArgs = HashMap<String, String>;

// ─────────────────────────────────────────────────────────────────────────────
// Locale Negotiation
// ─────────────────────────────────────────────────────────────────────────────

/// Negotiate best locale from preferences. RFC 4647: O(n × m).
pub fn negotiate_locale(
    preferred: &[Locale],
    available: &[Locale],
    default: &Locale,
) -> Locale {
    for pref in preferred {
        for avail in available {
            if pref.tag() == avail.tag() {
                return avail.clone();
            }
        }
    }
    for pref in preferred {
        for avail in available {
            if pref.language() == avail.language() {
                return avail.clone();
            }
        }
    }
    default.clone()
}

// ─────────────────────────────────────────────────────────────────────────────
// i18n Manager
// ─────────────────────────────────────────────────────────────────────────────

/// Internationalization manager (sync, no async deps).
pub struct I18nManager {
    bundles: RwLock<HashMap<String, MessageBundle>>,
    available: Vec<Locale>,
    default_locale: Locale,
    active: RwLock<Locale>,
}

impl I18nManager {
    pub fn new(default_locale: Locale) -> Self {
        Self {
            bundles: RwLock::new(HashMap::new()),
            available: vec![default_locale.clone()],
            default_locale: default_locale.clone(),
            active: RwLock::new(default_locale),
        }
    }

    pub fn register_bundle(&self, bundle: MessageBundle) {
        let tag = bundle.locale.tag().to_string();
        let mut bundles = self.bundles.write().unwrap();
        bundles.insert(tag, bundle);
    }

    pub fn add_available(&mut self, locale: Locale) {
        if !self.available.contains(&locale) {
            self.available.push(locale);
        }
    }

    pub fn set_preferred(&self, preferred: &[Locale]) {
        let negotiated = negotiate_locale(preferred, &self.available, &self.default_locale);
        *self.active.write().unwrap() = negotiated;
    }

    pub fn active_locale(&self) -> Locale {
        self.active.read().unwrap().clone()
    }

    /// Format a message in the active locale.
    pub fn format(&self, id: &str, args: &MessageArgs) -> String {
        let locale = self.active.read().unwrap();
        self.format_in(locale.tag(), id, args)
    }

    /// Format in a specific locale with fallback to default.
    pub fn format_in(&self, locale_tag: &str, id: &str, args: &MessageArgs) -> String {
        let bundles = self.bundles.read().unwrap();
        if let Some(bundle) = bundles.get(locale_tag) {
            if let Some(msg) = bundle.format(id, args) {
                return msg;
            }
        }
        if let Some(bundle) = bundles.get(self.default_locale.tag()) {
            if let Some(msg) = bundle.format(id, args) {
                return msg;
            }
        }
        id.to_string()
    }

    pub fn get(&self, id: &str) -> String {
        self.format(id, &HashMap::new())
    }

    pub fn available_locales(&self) -> &[Locale] {
        &self.available
    }
}

/// Default English message bundle.
pub fn default_en_bundle() -> MessageBundle {
    let mut b = MessageBundle::new(Locale::new("en"));
    b.add("welcome", "Welcome to ClawDesk!");
    b.add("error.generic", "An error occurred: {error}");
    b.add("error.network", "Network error: {detail}");
    b.add("error.auth", "Authentication failed");
    b.add("error.rate_limit", "Rate limited. Please wait {seconds} seconds.");
    b.add("error.content_too_large", "Content exceeds maximum size ({size} bytes)");
    b.add("session.created", "New session started");
    b.add("session.ended", "Session ended");
    b.add("agent.thinking", "Thinking...");
    b.add("agent.tool_use", "Using tool: {tool}");
    b.add("agent.complete", "Done");
    b.add("channel.connected", "Connected to {channel}");
    b.add("channel.disconnected", "Disconnected from {channel}");
    b.add("dm.pairing_required", "DM pairing required. Your code: {code}");
    b.add("dm.denied", "DM access denied for this channel");
    b.add("security.blocked", "Content blocked: {reason}");
    b.add("security.flagged", "Content flagged for review");
    b
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_ftl_format() {
        let content = "# Comment\nwelcome = Welcome!\nerror = An error: {detail}\n";
        let bundle = MessageBundle::parse_ftl(Locale::new("en"), content);
        assert_eq!(bundle.get("welcome"), Some("Welcome!"));
    }

    #[test]
    fn format_with_args() {
        let mut bundle = MessageBundle::new(Locale::new("en"));
        bundle.add("greeting", "Hello, {name}! You have {count} messages.");
        let mut args = HashMap::new();
        args.insert("name".to_string(), "Alice".to_string());
        args.insert("count".to_string(), "5".to_string());
        let result = bundle.format("greeting", &args).unwrap();
        assert_eq!(result, "Hello, Alice! You have 5 messages.");
    }

    #[test]
    fn locale_negotiation_exact() {
        let preferred = vec![Locale::new("zh-CN")];
        let available = vec![Locale::new("en"), Locale::new("zh-CN"), Locale::new("ja")];
        assert_eq!(negotiate_locale(&preferred, &available, &Locale::new("en")).tag(), "zh-CN");
    }

    #[test]
    fn locale_negotiation_language_fallback() {
        let preferred = vec![Locale::new("en-GB")];
        let available = vec![Locale::new("en"), Locale::new("zh")];
        assert_eq!(negotiate_locale(&preferred, &available, &Locale::new("en")).tag(), "en");
    }

    #[test]
    fn locale_negotiation_default() {
        let preferred = vec![Locale::new("fr")];
        let available = vec![Locale::new("en"), Locale::new("zh")];
        assert_eq!(negotiate_locale(&preferred, &available, &Locale::new("en")).tag(), "en");
    }

    #[test]
    fn i18n_manager_basic() {
        let mgr = I18nManager::new(Locale::new("en"));
        mgr.register_bundle(default_en_bundle());
        assert_eq!(mgr.get("welcome"), "Welcome to ClawDesk!");

        let mut args = HashMap::new();
        args.insert("error".to_string(), "timeout".to_string());
        assert_eq!(mgr.format("error.generic", &args), "An error occurred: timeout");
    }

    #[test]
    fn missing_message_returns_id() {
        let mgr = I18nManager::new(Locale::new("en"));
        mgr.register_bundle(default_en_bundle());
        assert_eq!(mgr.get("nonexistent.key"), "nonexistent.key");
    }

    #[test]
    fn multi_locale() {
        let mgr = I18nManager::new(Locale::new("en"));
        mgr.register_bundle(default_en_bundle());
        let mut ja = MessageBundle::new(Locale::new("ja"));
        ja.add("welcome", "ClawDeskへようこそ！");
        mgr.register_bundle(ja);

        assert_eq!(mgr.get("welcome"), "Welcome to ClawDesk!");
        assert_eq!(mgr.format_in("ja", "welcome", &HashMap::new()), "ClawDeskへようこそ！");
    }
}
