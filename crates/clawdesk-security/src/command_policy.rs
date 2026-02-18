//! Command policy engine with trie-based prefix matching.
//!
//! The engine classifies commands into risk levels, determines whether
//! explicit approval is required, and can hard-deny dangerous patterns.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Risk level for command execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Low,
    Medium,
    High,
}

/// A single command policy rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyRule {
    /// Prefix to match against normalized command text.
    pub prefix: String,
    pub risk: RiskLevel,
    /// Whether human approval is required for this command.
    pub requires_approval: bool,
    /// Whether this command is denied outright.
    pub deny: bool,
    /// Optional human-readable reason.
    pub reason: Option<String>,
}

/// Engine configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandPolicyConfig {
    pub default_risk: RiskLevel,
    pub default_requires_approval: bool,
    pub rules: Vec<PolicyRule>,
}

impl Default for CommandPolicyConfig {
    fn default() -> Self {
        Self {
            default_risk: RiskLevel::Medium,
            default_requires_approval: true,
            rules: vec![
                PolicyRule {
                    prefix: "rm -rf /".to_string(),
                    risk: RiskLevel::High,
                    requires_approval: true,
                    deny: true,
                    reason: Some("destructive root deletion denied".to_string()),
                },
                PolicyRule {
                    prefix: "mkfs".to_string(),
                    risk: RiskLevel::High,
                    requires_approval: true,
                    deny: true,
                    reason: Some("filesystem formatting denied".to_string()),
                },
                PolicyRule {
                    prefix: "shutdown".to_string(),
                    risk: RiskLevel::High,
                    requires_approval: true,
                    deny: true,
                    reason: Some("host shutdown denied".to_string()),
                },
                PolicyRule {
                    prefix: "reboot".to_string(),
                    risk: RiskLevel::High,
                    requires_approval: true,
                    deny: true,
                    reason: Some("host reboot denied".to_string()),
                },
                PolicyRule {
                    prefix: "sudo ".to_string(),
                    risk: RiskLevel::High,
                    requires_approval: true,
                    deny: false,
                    reason: Some("privileged command".to_string()),
                },
                PolicyRule {
                    prefix: "rm ".to_string(),
                    risk: RiskLevel::High,
                    requires_approval: true,
                    deny: false,
                    reason: Some("destructive file operation".to_string()),
                },
                PolicyRule {
                    prefix: "curl ".to_string(),
                    risk: RiskLevel::Medium,
                    requires_approval: true,
                    deny: false,
                    reason: Some("network egress".to_string()),
                },
                PolicyRule {
                    prefix: "wget ".to_string(),
                    risk: RiskLevel::Medium,
                    requires_approval: true,
                    deny: false,
                    reason: Some("network egress".to_string()),
                },
                PolicyRule {
                    prefix: "ls".to_string(),
                    risk: RiskLevel::Low,
                    requires_approval: false,
                    deny: false,
                    reason: Some("read-only listing".to_string()),
                },
                PolicyRule {
                    prefix: "cat ".to_string(),
                    risk: RiskLevel::Low,
                    requires_approval: false,
                    deny: false,
                    reason: Some("read-only file read".to_string()),
                },
                PolicyRule {
                    prefix: "pwd".to_string(),
                    risk: RiskLevel::Low,
                    requires_approval: false,
                    deny: false,
                    reason: Some("read-only working directory".to_string()),
                },
            ],
        }
    }
}

#[derive(Debug, Default, Clone)]
struct TrieNode {
    children: HashMap<char, TrieNode>,
    rule_index: Option<usize>,
}

impl TrieNode {
    fn insert(&mut self, prefix: &str, rule_index: usize) {
        let mut node = self;
        for ch in prefix.chars() {
            node = node.children.entry(ch).or_default();
        }
        node.rule_index = Some(rule_index);
    }

    fn longest_prefix_match(&self, text: &str) -> Option<usize> {
        let mut node = self;
        let mut best = node.rule_index;
        for ch in text.chars() {
            let Some(next) = node.children.get(&ch) else {
                break;
            };
            node = next;
            if node.rule_index.is_some() {
                best = node.rule_index;
            }
        }
        best
    }
}

/// Classification output for a command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandDecision {
    pub risk: RiskLevel,
    pub requires_approval: bool,
    pub denied: bool,
    pub matched_prefix: Option<String>,
    pub reason: Option<String>,
}

impl CommandDecision {
    fn denied(reason: impl Into<String>) -> Self {
        Self {
            risk: RiskLevel::High,
            requires_approval: true,
            denied: true,
            matched_prefix: None,
            reason: Some(reason.into()),
        }
    }
}

/// Trie-backed command policy engine.
pub struct CommandPolicyEngine {
    config: CommandPolicyConfig,
    trie: TrieNode,
}

impl CommandPolicyEngine {
    pub fn new(config: CommandPolicyConfig) -> Self {
        let mut trie = TrieNode::default();
        for (idx, rule) in config.rules.iter().enumerate() {
            trie.insert(&normalize(&rule.prefix), idx);
        }
        Self { config, trie }
    }

    /// Classify an input command in O(L) where L is command length.
    pub fn classify(&self, command: &str) -> CommandDecision {
        let normalized = normalize(command);
        if normalized.is_empty() {
            return CommandDecision::denied("empty command");
        }

        if let Some(rule_idx) = self.trie.longest_prefix_match(&normalized) {
            let rule = &self.config.rules[rule_idx];
            return CommandDecision {
                risk: rule.risk,
                requires_approval: rule.requires_approval,
                denied: rule.deny,
                matched_prefix: Some(rule.prefix.clone()),
                reason: rule.reason.clone(),
            };
        }

        CommandDecision {
            risk: self.config.default_risk,
            requires_approval: self.config.default_requires_approval,
            denied: false,
            matched_prefix: None,
            reason: Some("no explicit rule matched".to_string()),
        }
    }
}

impl Default for CommandPolicyEngine {
    fn default() -> Self {
        Self::new(CommandPolicyConfig::default())
    }
}

fn normalize(input: &str) -> String {
    input.trim_start().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denies_hard_blocked_commands() {
        let engine = CommandPolicyEngine::default();
        let decision = engine.classify("rm -rf / --no-preserve-root");
        assert!(decision.denied);
        assert_eq!(decision.risk, RiskLevel::High);
    }

    #[test]
    fn longest_prefix_wins() {
        let config = CommandPolicyConfig {
            default_risk: RiskLevel::Medium,
            default_requires_approval: true,
            rules: vec![
                PolicyRule {
                    prefix: "git ".to_string(),
                    risk: RiskLevel::Medium,
                    requires_approval: true,
                    deny: false,
                    reason: None,
                },
                PolicyRule {
                    prefix: "git status".to_string(),
                    risk: RiskLevel::Low,
                    requires_approval: false,
                    deny: false,
                    reason: None,
                },
            ],
        };
        let engine = CommandPolicyEngine::new(config);
        let decision = engine.classify("git status --short");
        assert_eq!(decision.risk, RiskLevel::Low);
        assert!(!decision.requires_approval);
    }

    #[test]
    fn low_risk_read_only_commands_are_auto_allowed() {
        let engine = CommandPolicyEngine::default();
        let decision = engine.classify("ls -la");
        assert!(!decision.denied);
        assert_eq!(decision.risk, RiskLevel::Low);
        assert!(!decision.requires_approval);
    }

    #[test]
    fn unknown_commands_fall_back_to_default_policy() {
        let engine = CommandPolicyEngine::default();
        let decision = engine.classify("some-unknown-tool --flag");
        assert!(!decision.denied);
        assert_eq!(decision.risk, RiskLevel::Medium);
        assert!(decision.requires_approval);
    }
}

