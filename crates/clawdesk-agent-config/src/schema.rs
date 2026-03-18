//! Agent configuration schema — the product type that defines an agent.
//!
//! AgentConfig = Name × Model × FallbackChain × SystemPrompt × Capabilities × Tools × ResourceLimits × Channels

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Top-level agent configuration parsed from a TOML file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Core agent identity.
    pub agent: AgentIdentity,

    /// LLM model configuration and fallback chain.
    pub model: ModelConfig,

    /// System prompt definition.
    pub system_prompt: SystemPromptConfig,

    /// Trait composition for persona algebra.
    #[serde(default)]
    pub traits: TraitConfig,

    /// Tool and resource capabilities (security boundary).
    #[serde(default)]
    pub capabilities: CapabilityConfig,

    /// Resource limits and budgets.
    #[serde(default)]
    pub resources: ResourceConfig,

    /// Channel-specific overrides.
    #[serde(default)]
    pub channels: HashMap<String, ChannelOverride>,

    /// Bootstrap context configuration.
    #[serde(default)]
    pub bootstrap: Option<BootstrapConfig>,

    /// Metadata for marketplace discovery.
    #[serde(default)]
    pub metadata: Option<MetadataConfig>,
}

/// Agent identity and description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentIdentity {
    /// Unique agent name (kebab-case, e.g., "data-analyst").
    pub name: String,

    /// Human-readable description of the agent's purpose.
    pub description: String,

    /// Semantic version of this agent config.
    #[serde(default = "default_version")]
    pub version: String,

    /// Author of this agent configuration.
    #[serde(default)]
    pub author: Option<String>,

    /// Tags for categorization and search.
    #[serde(default)]
    pub tags: Vec<String>,

    /// URL to an icon image.
    #[serde(default)]
    pub icon: Option<String>,
}

/// LLM model selection and fallback chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    /// Primary provider. Use `"auto"` to defer to the user's configured
    /// provider preference or the TurnRouter's bandit-based model selection.
    /// Explicit values: `"anthropic"`, `"openai"`, `"gemini"`, `"ollama"`, etc.
    #[serde(default = "default_provider")]
    pub provider: String,

    /// Primary model name. Use `"auto"` to let the TurnRouter (LinUCB bandit)
    /// select the optimal model per-turn based on task features. Use `"default"`
    /// to use the user's configured default model. Explicit values are also
    /// accepted (e.g., `"claude-sonnet-4-20250514"`).
    #[serde(default = "default_model")]
    pub model: String,

    /// Ordered fallback chain: tried in sequence if primary fails.
    /// Format: "provider:model" (e.g., "openai:gpt-4o").
    #[serde(default)]
    pub fallback: Vec<String>,

    /// Sampling temperature (0.0 = deterministic, 1.0 = creative).
    #[serde(default = "default_temperature")]
    pub temperature: f64,

    /// Maximum output tokens per response.
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,

    /// Top-p nucleus sampling.
    #[serde(default)]
    pub top_p: Option<f64>,
}

/// System prompt configuration with template support.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemPromptConfig {
    /// The system prompt content. Supports {{variable}} template syntax.
    pub content: String,

    /// Additional context sections appended after the main prompt.
    #[serde(default)]
    pub sections: Vec<PromptSection>,
}

/// A named section appended to the system prompt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptSection {
    pub name: String,
    pub content: String,
    /// Priority: required, high, medium, low, optional.
    #[serde(default = "default_priority")]
    pub priority: String,
}

/// Trait composition for the agent persona algebra.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TraitConfig {
    /// Persona traits (max 3): concise, verbose, friendly, formal, academic.
    #[serde(default)]
    pub persona: Vec<String>,

    /// Methodology traits: first-principles, evidence-based, creative-brainstorm, systematic.
    #[serde(default)]
    pub methodology: Vec<String>,

    /// Domain expertise: legal, financial, medical, engineering, data-science, etc.
    #[serde(default)]
    pub domain: Vec<String>,

    /// Output format: structured-report, conversational, code-first.
    #[serde(default)]
    pub output: Vec<String>,

    /// Constraints: no-financial-advice, no-legal-advice, hipaa-compliant.
    #[serde(default)]
    pub constraints: Vec<String>,
}

/// Capability declarations for security enforcement (Bell-LaPadula model).
///
/// effective_caps(agent) = declared_caps(agent) ∩ system_policy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityConfig {
    /// Allowed tool names. Use ["*"] for all tools (dangerous).
    #[serde(default)]
    pub tools: Vec<String>,

    /// Denied tool names (overrides allow).
    #[serde(default)]
    pub deny_tools: Vec<String>,

    /// Network access patterns: ["*"], ["api.example.com"], etc.
    #[serde(default)]
    pub network: Vec<String>,

    /// Memory write patterns: ["self.*"], ["shared.team.*"], etc.
    #[serde(default)]
    pub memory_write: Vec<String>,

    /// Memory read patterns.
    #[serde(default)]
    pub memory_read: Vec<String>,

    /// Shell command patterns: ["python *"], ["git *"], etc.
    #[serde(default)]
    pub shell: Vec<String>,

    /// Filesystem read patterns.
    #[serde(default)]
    pub filesystem_read: Vec<String>,

    /// Filesystem write patterns.
    #[serde(default)]
    pub filesystem_write: Vec<String>,
}

impl Default for CapabilityConfig {
    fn default() -> Self {
        Self {
            tools: vec![
                "read_file".into(),
                "list_directory".into(),
                "search_files".into(),
                "web_search".into(),
            ],
            deny_tools: Vec::new(),
            network: Vec::new(),
            memory_write: vec!["self.*".into()],
            memory_read: vec!["self.*".into()],
            shell: Vec::new(),
            filesystem_read: vec!["**".into()],
            filesystem_write: Vec::new(),
        }
    }
}

/// Resource limits and token budgets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceConfig {
    /// Maximum tokens consumed per hour across all requests.
    #[serde(default = "default_tokens_per_hour")]
    pub max_tokens_per_hour: u64,

    /// Maximum tool call iterations per request.
    #[serde(default = "default_max_tool_iterations")]
    pub max_tool_iterations: u32,

    /// Request timeout in seconds.
    #[serde(default = "default_timeout")]
    pub timeout_seconds: u64,

    /// Maximum concurrent requests.
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_requests: u32,

    /// Maximum context window usage (tokens).
    #[serde(default = "default_context_limit")]
    pub context_limit: usize,

    /// Enable streaming responses.
    #[serde(default = "default_true")]
    pub enable_streaming: bool,
}

impl Default for ResourceConfig {
    fn default() -> Self {
        Self {
            max_tokens_per_hour: default_tokens_per_hour(),
            max_tool_iterations: default_max_tool_iterations(),
            timeout_seconds: default_timeout(),
            max_concurrent_requests: default_max_concurrent(),
            context_limit: default_context_limit(),
            enable_streaming: true,
        }
    }
}

/// Channel-specific configuration overrides.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelOverride {
    /// Override system prompt for this channel.
    #[serde(default)]
    pub system_prompt_append: Option<String>,

    /// Override max message length.
    #[serde(default)]
    pub max_message_length: Option<usize>,

    /// Override history limit.
    #[serde(default)]
    pub history_limit: Option<usize>,

    /// Override markup format.
    #[serde(default)]
    pub markup_format: Option<String>,

    /// Extra channel-specific instructions.
    #[serde(default)]
    pub extra_instructions: Option<String>,
}

/// Bootstrap context discovery configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapConfig {
    #[serde(default = "default_bootstrap_depth")]
    pub max_depth: usize,

    #[serde(default = "default_bootstrap_chars")]
    pub max_total_chars: usize,

    #[serde(default)]
    pub extra_filenames: Vec<String>,

    #[serde(default)]
    pub exclude_filenames: Vec<String>,
}

/// Metadata for marketplace discovery and indexing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataConfig {
    /// Category for marketplace listing.
    #[serde(default)]
    pub category: Option<String>,

    /// Detailed description for semantic search.
    #[serde(default)]
    pub long_description: Option<String>,

    /// Example prompts that demonstrate the agent's capabilities.
    #[serde(default)]
    pub example_prompts: Vec<String>,

    /// Required minimum provider capabilities.
    #[serde(default)]
    pub requires: Vec<String>,
}

// ── Default value functions ──

fn default_version() -> String {
    "1.0.0".into()
}
fn default_provider() -> String {
    "auto".into()
}
fn default_model() -> String {
    "auto".into()
}
fn default_temperature() -> f64 {
    0.7
}
fn default_max_tokens() -> u32 {
    4096
}
fn default_priority() -> String {
    "medium".into()
}
fn default_tokens_per_hour() -> u64 {
    500_000
}
fn default_max_tool_iterations() -> u32 {
    25
}
fn default_timeout() -> u64 {
    300
}
fn default_max_concurrent() -> u32 {
    5
}
fn default_context_limit() -> usize {
    128_000
}
fn default_true() -> bool {
    true
}
fn default_bootstrap_depth() -> usize {
    3
}
fn default_bootstrap_chars() -> usize {
    50_000
}

impl AgentConfig {
    /// Parse an agent config from a TOML string.
    pub fn from_toml(s: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(s)
    }

    /// Serialize to TOML string.
    pub fn to_toml(&self) -> Result<String, toml::ser::Error> {
        toml::to_string_pretty(self)
    }

    /// Returns the model routing string (e.g., "anthropic:claude-sonnet-4-20250514").
    pub fn model_route(&self) -> String {
        format!("{}:{}", self.model.provider, self.model.model)
    }
}
