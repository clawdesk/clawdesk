//! Directive parser — inline annotations for model selection, reasoning, verbosity.
//!
//! Parses: `@model:gpt-4 @think:high @verbose:2 @tts:on`
//! Single-pass O(n) regex scan. Priority: inline > agent_config > channel_default > global.

use serde::{Deserialize, Serialize};

/// Parsed directives from a message.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Directives {
    pub model: Option<String>,
    pub think_level: Option<ThinkLevel>,
    pub verbose: Option<u8>,
    pub tts: Option<bool>,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkLevel {
    Off,
    Low,
    Medium,
    High,
}

impl std::str::FromStr for ThinkLevel {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "off" | "none" | "0" => Ok(Self::Off),
            "low" | "1" => Ok(Self::Low),
            "medium" | "med" | "2" => Ok(Self::Medium),
            "high" | "3" => Ok(Self::High),
            _ => Err(format!("unknown think level: {s}")),
        }
    }
}

/// Parse directives from a message string.
///
/// O(n) single-pass scan. Directives are `@key:value` patterns.
pub fn parse_directives(text: &str) -> (Directives, String) {
    let mut directives = Directives::default();
    let mut clean_parts = Vec::new();

    for word in text.split_whitespace() {
        if let Some(directive) = word.strip_prefix('@') {
            if let Some((key, value)) = directive.split_once(':') {
                match key.to_lowercase().as_str() {
                    "model" => directives.model = Some(value.to_string()),
                    "think" | "thinking" => {
                        directives.think_level = value.parse().ok();
                    }
                    "verbose" | "v" => {
                        directives.verbose = value.parse().ok();
                    }
                    "tts" => {
                        directives.tts = Some(value == "on" || value == "true" || value == "1");
                    }
                    "lang" | "language" => {
                        directives.language = Some(value.to_string());
                    }
                    _ => clean_parts.push(word.to_string()), // unknown directive, keep as text
                }
            } else {
                clean_parts.push(word.to_string());
            }
        } else {
            clean_parts.push(word.to_string());
        }
    }

    let clean_text = clean_parts.join(" ");
    (directives, clean_text)
}

/// Merge directives with priority: inline > agent_config > global.
pub fn merge_directives(inline: &Directives, agent: &Directives, global: &Directives) -> Directives {
    Directives {
        model: inline.model.clone()
            .or_else(|| agent.model.clone())
            .or_else(|| global.model.clone()),
        think_level: inline.think_level
            .or(agent.think_level)
            .or(global.think_level),
        verbose: inline.verbose.or(agent.verbose).or(global.verbose),
        tts: inline.tts.or(agent.tts).or(global.tts),
        language: inline.language.clone()
            .or_else(|| agent.language.clone())
            .or_else(|| global.language.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_model_directive() {
        let (d, clean) = parse_directives("hello @model:gpt-4o world");
        assert_eq!(d.model, Some("gpt-4o".into()));
        assert_eq!(clean, "hello world");
    }

    #[test]
    fn parse_multiple_directives() {
        let (d, clean) = parse_directives("@think:high @tts:on explain this");
        assert_eq!(d.think_level, Some(ThinkLevel::High));
        assert_eq!(d.tts, Some(true));
        assert_eq!(clean, "explain this");
    }

    #[test]
    fn no_directives_passthrough() {
        let (d, clean) = parse_directives("just a normal message");
        assert!(d.model.is_none());
        assert_eq!(clean, "just a normal message");
    }

    #[test]
    fn merge_priority() {
        let inline = Directives { model: Some("gpt-4".into()), ..Default::default() };
        let agent = Directives { model: Some("claude".into()), think_level: Some(ThinkLevel::Low), ..Default::default() };
        let global = Directives { verbose: Some(1), ..Default::default() };
        let merged = merge_directives(&inline, &agent, &global);
        assert_eq!(merged.model, Some("gpt-4".into())); // inline wins
        assert_eq!(merged.think_level, Some(ThinkLevel::Low)); // from agent
        assert_eq!(merged.verbose, Some(1)); // from global
    }
}
