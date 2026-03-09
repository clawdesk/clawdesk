//! Snapshot tests for agent configuration serialization.
//!
//! Uses `insta` to verify that the TOML serialization of AgentConfig
//! doesn't regress unexpectedly when schema types change.

use clawdesk_agent_config::*;
use std::collections::HashMap;

/// Build a representative agent config for snapshot testing.
fn sample_config() -> AgentConfig {
    AgentConfig {
        agent: AgentIdentity {
            name: "code-reviewer".into(),
            description: "Reviews code changes and suggests improvements".into(),
            version: "1.0.0".into(),
            author: Some("ClawDesk Team".into()),
            tags: vec!["coding".into(), "review".into(), "quality".into()],
            icon: None,
        },
        model: ModelConfig {
            provider: "anthropic".into(),
            model: "claude-sonnet-4-20250514".into(),
            fallback: vec!["openai:gpt-4o".into()],
            temperature: 0.3,
            max_tokens: 4096,
            top_p: None,
        },
        system_prompt: SystemPromptConfig {
            content: "You are an expert code reviewer. Analyze code for bugs, \
                      performance issues, and style violations."
                .into(),
            sections: vec![PromptSection {
                name: "style".into(),
                content: "Follow the project's existing code style.".into(),
                priority: "high".into(),
            }],
        },
        traits: TraitConfig {
            persona: vec!["precise".into(), "formal".into()],
            methodology: vec!["systematic".into()],
            domain: vec!["engineering".into()],
            output: vec!["structured-report".into()],
            constraints: vec![],
        },
        capabilities: CapabilityConfig {
            tools: vec![
                "read_file".into(),
                "search_files".into(),
                "web_search".into(),
            ],
            deny_tools: vec!["shell_exec".into()],
            network: vec!["*.github.com".into(), "api.openai.com".into()],
            memory_write: vec!["self.*".into()],
            memory_read: vec!["self.*".into(), "shared.team.*".into()],
            shell: vec![],
            filesystem_read: vec!["**".into()],
            filesystem_write: vec![],
        },
        resources: ResourceConfig {
            max_tokens_per_hour: 200_000,
            max_tool_iterations: 15,
            timeout_seconds: 300,
            max_concurrent_requests: 3,
            context_limit: 128_000,
            enable_streaming: true,
        },
        channels: HashMap::new(),
        bootstrap: None,
        metadata: Some(MetadataConfig {
            category: Some("development".into()),
            long_description: Some("Thorough code review agent".into()),
            example_prompts: vec!["Review this PR for bugs".into()],
            requires: vec![],
        }),
    }
}

#[test]
fn snapshot_agent_config_toml() {
    let config = sample_config();
    let toml_str = toml::to_string_pretty(&config).expect("serialize to TOML");
    insta::assert_snapshot!("agent_config_toml", toml_str);
}

#[test]
fn snapshot_minimal_config_toml() {
    let config = AgentConfig {
        agent: AgentIdentity {
            name: "minimal".into(),
            description: "Minimal agent".into(),
            version: "0.1.0".into(),
            author: None,
            tags: vec![],
            icon: None,
        },
        model: ModelConfig {
            provider: "openai".into(),
            model: "gpt-4o-mini".into(),
            fallback: vec![],
            temperature: 0.7,
            max_tokens: 4096,
            top_p: None,
        },
        system_prompt: SystemPromptConfig {
            content: "You are a helpful assistant.".into(),
            sections: vec![],
        },
        traits: TraitConfig::default(),
        capabilities: CapabilityConfig::default(),
        resources: ResourceConfig::default(),
        channels: HashMap::new(),
        bootstrap: None,
        metadata: None,
    };
    let toml_str = toml::to_string_pretty(&config).expect("serialize to TOML");
    insta::assert_snapshot!("minimal_config_toml", toml_str);
}

#[test]
fn snapshot_roundtrip_preserves_data() {
    let config = sample_config();
    let toml_str = toml::to_string_pretty(&config).expect("serialize");
    let parsed: AgentConfig = toml::from_str(&toml_str).expect("deserialize");
    let re_serialized = toml::to_string_pretty(&parsed).expect("re-serialize");
    insta::assert_snapshot!("roundtrip_toml", re_serialized);
}
