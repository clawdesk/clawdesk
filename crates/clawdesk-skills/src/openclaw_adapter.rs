//! Legacy → ClawDesk skill adapter.
//!
//! Translates the `SKILL.md` format (YAML frontmatter + Markdown body)
//! into ClawDesk's `Skill` type (`SkillManifest` + prompt fragment). Enables
//! zero-modification porting of ~70% of the 52+ skills.
//!
//! ## legacy skill format
//!
//! ```text
//! ---
//! name: weather
//! description: Get current weather and forecasts.
//! homepage: https://wttr.in
//! metadata: { "openclaw": { "emoji": "🌤️", "requires": { "bins": ["curl"] } } }
//! ---
//!
//! # Weather
//! (markdown instructions — the prompt fragment)
//! ```
//!
//! ## Mapping
//!
//! | Legacy field              | ClawDesk field                |
//! |-----------------------------|-------------------------------|
//! | `name`                      | `SkillId("openclaw", name)`   |
//! | `description`               | `description` + `display_name`|
//! | body (Markdown)             | `prompt_fragment`             |
//! | `metadata.openclaw.requires.bins` | `required_tools` + tags |
//! | `metadata.openclaw.emoji`   | tag `emoji:<value>`           |
//! | `metadata.openclaw.always`  | `SkillTrigger::Always`        |
//! | `homepage`                  | tag `homepage:<url>`          |

use crate::definition::{
    Skill, SkillId, SkillManifest, SkillSource, SkillTrigger, SkillToolBinding,
};
use clawdesk_types::estimate_tokens;
use serde::Deserialize;
use sha2::{Sha256, Digest};
use std::path::Path;
use tracing::{debug, info, warn};

/// Well-known tool names that appear in legacy skill prompts.
/// These are agent-level tools that skills reference but don't own.
const KNOWN_AGENT_TOOLS: &[&str] = &[
    "web_search", "web_fetch", "file_read", "file_write", "file_list",
    "bash", "sh", "code_execute", "code_run", "memory_search",
    "memory_store", "task_store", "task_list", "system_check",
    "curl", "grep", "find", "cat", "ls", "mkdir", "rm",
];

/// Extract tool bindings from an legacy skill's prompt body and metadata.
///
/// legacy skills don't declare tools in frontmatter — they reference them
/// implicitly in the prompt body. We extract these references to populate
/// `provided_tools` so the executor can wire them up.
///
/// Strategy:
/// 1. `allowed-tools` frontmatter → explicit tool declarations
/// 2. Backtick-wrapped tool names in body → `tool_name` pattern
/// 3. `requires.bins` that aren't common system utils → CLI tool bindings
fn extract_tool_bindings(
    frontmatter: &OpenClawFrontmatter,
    body: &str,
    meta: &OpenClawMetadata,
    skill_name: &str,
) -> Vec<SkillToolBinding> {
    let mut tools = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // 1. Explicit allowed-tools from frontmatter
    if let Some(ref allowed) = frontmatter.allowed_tools {
        for tool in allowed {
            if seen.insert(tool.clone()) {
                tools.push(SkillToolBinding {
                    tool_name: tool.clone(),
                    description: format!("Tool allowed by {} skill", skill_name),
                    parameters_schema: serde_json::json!({"type": "object"}),
                });
            }
        }
    }

    // 2. Scan prompt body for backtick-wrapped tool references
    //    Pattern: `tool_name` where tool_name looks like a function/command
    let mut i = 0;
    let bytes = body.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'`' && (i + 1 < bytes.len()) && bytes[i + 1] != b'`' {
            // Single backtick — find closing backtick
            if let Some(end) = body[i + 1..].find('`') {
                let candidate = &body[i + 1..i + 1 + end];
                // Must look like a tool/command name: alphanumeric + underscores/hyphens, 2-30 chars
                if candidate.len() >= 2
                    && candidate.len() <= 30
                    && candidate.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                    && candidate.chars().next().map_or(false, |c| c.is_alphabetic())
                    && !KNOWN_AGENT_TOOLS.contains(&candidate)
                    && seen.insert(candidate.to_string())
                {
                    tools.push(SkillToolBinding {
                        tool_name: candidate.to_string(),
                        description: format!("CLI tool referenced by {} skill", skill_name),
                        parameters_schema: serde_json::json!({
                            "type": "object",
                            "properties": {
                                "args": {"type": "string", "description": "Command arguments"}
                            }
                        }),
                    });
                }
                i = i + 1 + end + 1;
                continue;
            }
        }
        i += 1;
    }

    // 3. Required bins that are skill-specific CLI tools (not common system utils)
    let system_bins: std::collections::HashSet<&str> = [
        "curl", "grep", "find", "cat", "ls", "mkdir", "rm", "cp", "mv",
        "sed", "awk", "sort", "head", "tail", "wc", "tr", "cut",
        "bash", "sh", "zsh", "python", "python3", "node", "npm",
    ].into_iter().collect();

    for bin in &meta.required_bins {
        if !system_bins.contains(bin.as_str()) && seen.insert(bin.clone()) {
            tools.push(SkillToolBinding {
                tool_name: bin.clone(),
                description: format!("Required binary for {} skill", skill_name),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "args": {"type": "string", "description": "Command arguments"}
                    }
                }),
            });
        }
    }

    // Also include anyBins as optional tool surfaces
    for bin in &meta.any_bins {
        if !system_bins.contains(bin.as_str()) && seen.insert(bin.clone()) {
            tools.push(SkillToolBinding {
                tool_name: bin.clone(),
                description: format!("Optional binary for {} skill (one of several)", skill_name),
                parameters_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "args": {"type": "string", "description": "Command arguments"}
                    }
                }),
            });
        }
    }

    tools
}

// ─────────────────────────────────────────────────────────────
// legacy frontmatter types (deserialized from YAML)
// ─────────────────────────────────────────────────────────────

/// Raw YAML frontmatter from a legacy `SKILL.md`.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct OpenClawFrontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    pub homepage: Option<String>,
    pub license: Option<String>,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
    /// Optional: restrict which tools the skill can use.
    #[serde(rename = "allowed-tools", default)]
    pub allowed_tools: Option<Vec<String>>,
    /// Optional: disable model invocation (user-only slash command).
    #[serde(rename = "disable-model-invocation", default)]
    pub disable_model_invocation: Option<bool>,
    /// Optional: whether the skill is user-invocable as a command.
    #[serde(rename = "user-invocable", default)]
    pub user_invocable: Option<bool>,
}

/// Resolved legacy metadata from the `metadata.openclaw` block.
#[derive(Debug, Clone, Default)]
pub struct OpenClawMetadata {
    pub always: bool,
    pub emoji: Option<String>,
    pub homepage: Option<String>,
    pub primary_env: Option<String>,
    pub required_bins: Vec<String>,
    pub any_bins: Vec<String>,
    pub required_env: Vec<String>,
    pub required_config: Vec<String>,
    pub os_filters: Vec<String>,
    pub install_specs: Vec<serde_json::Value>,
}

// ─────────────────────────────────────────────────────────────
// Parsing
// ─────────────────────────────────────────────────────────────

/// Parse a `SKILL.md` file into frontmatter + body.
///
/// Handles both `---` (standard YAML) and `` ```skill `` fenced blocks.
/// Falls back to regex-based parsing if serde_yaml chokes on inline JSON
/// or colons in description values.
pub fn parse_skill_md(content: &str) -> Result<(OpenClawFrontmatter, String), AdapterError> {
    let trimmed = content.trim();

    // Detect fenced-block format: ```skill\n---\n...\n---\n...\n```
    // or standard ---\n...\n--- format
    let (yaml_str, body) = if trimmed.starts_with("```") {
        // Fenced block: skip the opening ``` line
        let after_fence = trimmed
            .find('\n')
            .map(|i| &trimmed[i + 1..])
            .unwrap_or("");
        // Remove trailing ``` if present
        let after_fence = after_fence
            .strip_suffix("```")
            .unwrap_or(after_fence)
            .trim();
        extract_yaml_body(after_fence)?
    } else if trimmed.starts_with("---") {
        extract_yaml_body(trimmed)?
    } else {
        // Try the full content as just a YAML-frontmatter document
        // Some formats use ````skill blocks
        let cleaned = trimmed
            .trim_start_matches("````skill")
            .trim_start_matches("```skill")
            .trim_end_matches("````")
            .trim_end_matches("```")
            .trim();
        if cleaned.starts_with("---") {
            extract_yaml_body(cleaned)?
        } else {
            return Err(AdapterError::NoFrontmatter);
        }
    };

    // Try serde_yaml first; fall back to regex on parse failure.
    let frontmatter = match serde_yaml::from_str::<OpenClawFrontmatter>(&yaml_str) {
        Ok(fm) => fm,
        Err(_) => parse_frontmatter_regex(&yaml_str)?,
    };

    Ok((frontmatter, body.to_string()))
}

/// Extract YAML frontmatter and body from a `---` delimited string.
fn extract_yaml_body(s: &str) -> Result<(String, String), AdapterError> {
    let s = s.strip_prefix("---").ok_or(AdapterError::NoFrontmatter)?;
    let end = s.find("\n---").ok_or(AdapterError::NoFrontmatter)?;
    let yaml = &s[..end];
    let body = &s[end + 4..]; // skip "\n---"
    Ok((yaml.trim().to_string(), body.trim().to_string()))
}

/// Regex-based fallback parser for frontmatter that serde_yaml can't handle.
///
/// Handles cases like colons in description values (`Triggers: ...`) or
/// inline JSON metadata that confuses the YAML parser.
fn parse_frontmatter_regex(yaml: &str) -> Result<OpenClawFrontmatter, AdapterError> {
    let mut fm = OpenClawFrontmatter::default();

    for line in yaml.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Match `key: value` — take only the first colon as separator.
        if let Some(colon_pos) = line.find(':') {
            let key = line[..colon_pos].trim();
            let value = line[colon_pos + 1..].trim();

            match key {
                "name" => fm.name = Some(value.to_string()),
                "description" => fm.description = Some(value.to_string()),
                "homepage" => fm.homepage = Some(value.to_string()),
                "license" => fm.license = Some(value.to_string()),
                "metadata" => {
                    // Try to parse the rest as JSON.
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(value) {
                        fm.metadata = Some(v);
                    }
                }
                _ => {} // skip unknown keys
            }
        }
    }

    if fm.name.is_none() {
        return Err(AdapterError::YamlParse(
            "regex fallback: no 'name' field found".to_string(),
        ));
    }

    Ok(fm)
}

/// Resolve the `metadata.openclaw` block into structured metadata.
pub fn resolve_metadata(frontmatter: &OpenClawFrontmatter) -> OpenClawMetadata {
    let mut meta = OpenClawMetadata::default();

    let Some(metadata_val) = &frontmatter.metadata else {
        return meta;
    };

    let Some(openclaw_block) = metadata_val.get("openclaw") else {
        return meta;
    };

    // always
    if let Some(always) = openclaw_block.get("always").and_then(|v| v.as_bool()) {
        meta.always = always;
    }

    // emoji
    if let Some(emoji) = openclaw_block.get("emoji").and_then(|v| v.as_str()) {
        meta.emoji = Some(emoji.to_string());
    }

    // homepage (can also come from top-level frontmatter)
    if let Some(hp) = openclaw_block.get("homepage").and_then(|v| v.as_str()) {
        meta.homepage = Some(hp.to_string());
    } else if let Some(hp) = &frontmatter.homepage {
        meta.homepage = Some(hp.clone());
    }

    // primaryEnv
    if let Some(pe) = openclaw_block.get("primaryEnv").and_then(|v| v.as_str()) {
        meta.primary_env = Some(pe.to_string());
    }

    // requires
    if let Some(requires) = openclaw_block.get("requires") {
        // bins
        if let Some(bins) = requires.get("bins").and_then(|v| v.as_array()) {
            meta.required_bins = bins
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
        }
        // anyBins
        if let Some(bins) = requires.get("anyBins").and_then(|v| v.as_array()) {
            meta.any_bins = bins
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
        }
        // env
        if let Some(env) = requires.get("env").and_then(|v| v.as_array()) {
            meta.required_env = env
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
        }
        // config
        if let Some(config) = requires.get("config").and_then(|v| v.as_array()) {
            meta.required_config = config
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
        }
    }

    // os
    if let Some(os_val) = openclaw_block.get("os") {
        if let Some(arr) = os_val.as_array() {
            meta.os_filters = arr
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect();
        }
    }

    // install
    if let Some(install) = openclaw_block.get("install").and_then(|v| v.as_array()) {
        meta.install_specs = install.clone();
    }

    meta
}

// ─────────────────────────────────────────────────────────────
// Conversion
// ─────────────────────────────────────────────────────────────

/// Adaptation result — a ClawDesk `Skill` plus diagnostic info.
#[derive(Debug)]
pub struct AdaptedSkill {
    pub skill: Skill,
    pub tier: AdaptationTier,
    pub warnings: Vec<String>,
    /// Original legacy metadata for reference.
    pub openclaw_meta: OpenClawMetadata,
}

/// Adaptation tier — how cleanly the skill ported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdaptationTier {
    /// Works with zero modification through the adapter (~70%).
    Direct,
    /// Needs context patches (env vars, config references, etc.) (~20%).
    ContextPatch,
    /// Needs manual rewrite (platform-specific, binary deps, etc.) (~10%).
    NeedsRewrite,
}

impl std::fmt::Display for AdaptationTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdaptationTier::Direct => write!(f, "direct"),
            AdaptationTier::ContextPatch => write!(f, "context-patch"),
            AdaptationTier::NeedsRewrite => write!(f, "needs-rewrite"),
        }
    }
}

/// Configuration for the adapter.
#[derive(Debug, Clone)]
pub struct AdapterConfig {
    /// Namespace prefix for adapted skills (default: "openclaw").
    pub namespace: String,
    /// Default priority weight for adapted skills.
    pub default_priority: f64,
    /// Default version for adapted skills.
    pub default_version: String,
    /// Author attribution.
    pub author: String,
}

impl Default for AdapterConfig {
    fn default() -> Self {
        Self {
            namespace: "openclaw".to_string(),
            default_priority: 0.5,
            default_version: "1.0.0-ported".to_string(),
            author: "OpenClaw (MIT, auto-adapted)".to_string(),
        }
    }
}

/// Convert an legacy skill (frontmatter + body) into a ClawDesk `Skill`.
pub fn adapt_skill(
    frontmatter: &OpenClawFrontmatter,
    body: &str,
    config: &AdapterConfig,
) -> Result<AdaptedSkill, AdapterError> {
    let name = frontmatter
        .name
        .as_deref()
        .ok_or(AdapterError::MissingField("name"))?;
    let description = frontmatter
        .description
        .as_deref()
        .ok_or(AdapterError::MissingField("description"))?;

    let meta = resolve_metadata(frontmatter);
    let mut warnings = Vec::new();

    // ── Build tags ──
    let mut tags = vec!["openclaw".to_string(), "ported".to_string()];
    if let Some(ref emoji) = meta.emoji {
        tags.push(format!("emoji:{}", emoji));
    }
    if let Some(ref hp) = meta.homepage {
        tags.push(format!("homepage:{}", hp));
    }
    for bin in &meta.required_bins {
        tags.push(format!("requires-bin:{}", bin));
    }
    for env in &meta.required_env {
        tags.push(format!("requires-env:{}", env));
    }
    for cfg in &meta.required_config {
        tags.push(format!("requires-config:{}", cfg));
    }
    if let Some(ref pe) = meta.primary_env {
        tags.push(format!("primary-env:{}", pe));
    }
    for os_filter in &meta.os_filters {
        tags.push(format!("os:{}", os_filter));
    }

    // ── Determine trigger ──
    let triggers = if meta.always {
        vec![SkillTrigger::Always]
    } else {
        // Map name to keyword trigger + always fallback.
        // Legacy gating is load-time (requirements); in ClawDesk we use
        // keyword matching on the skill name so it activates contextually.
        let keywords: Vec<String> = name
            .split('-')
            .filter(|w| w.len() > 2)
            .map(|w| w.to_lowercase())
            .collect();
        if keywords.is_empty() {
            vec![SkillTrigger::Always]
        } else {
            vec![
                SkillTrigger::Keywords {
                    words: keywords,
                    threshold: 0.3,
                },
                SkillTrigger::Command {
                    command: format!("/{}", name),
                },
            ]
        }
    };

    // ── Determine display name ──
    let display_name = name
        .split('-')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().to_string() + c.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    // ── Classify adaptation tier ──
    let tier = classify_tier(&meta, frontmatter, &mut warnings);

    // ── Build prompt fragment ──
    // Prefix with origin attribution comment for traceability.
    let prompt_fragment = if body.is_empty() {
        description.to_string()
    } else {
        format!(
            "<!-- Ported from legacy skill '{}' (MIT) -->\n\n{}",
            name, body
        )
    };

    // ── Content hash ──
    let mut hasher = Sha256::new();
    hasher.update(prompt_fragment.as_bytes());
    let content_hash = hex::encode(hasher.finalize());

    let estimated_tokens = estimate_tokens(&prompt_fragment);

    let manifest = SkillManifest {
        id: SkillId::new(&config.namespace, name),
        display_name,
        description: description.to_string(),
        version: config.default_version.clone(),
        author: Some(config.author.clone()),
        dependencies: vec![],
        required_tools: meta.required_bins.clone(),
        parameters: vec![],
        triggers,
        estimated_tokens,
        priority_weight: config.default_priority,
        tags,
        signature: None,
        publisher_key: None,
        content_hash: Some(content_hash),
        schema_version: 1,
    };

    // ── Extract tool bindings ──
    let provided_tools = extract_tool_bindings(frontmatter, &prompt_fragment, &meta, name);

    let skill = Skill {
        manifest,
        prompt_fragment,
        provided_tools,
        parameter_values: serde_json::Value::Null,
        source_path: None,
    };

    Ok(AdaptedSkill {
        skill,
        tier,
        warnings,
        openclaw_meta: meta,
    })
}

/// Classify a skill into an adaptation tier based on its requirements.
///
/// Tier logic:
/// - **Direct**: Works as-is. `required_bins`, `any_bins`, and `install_specs`
///   are just CLI tools referenced in the prompt — the skill is documentation
///   for using them. If the tool isn't installed, the user gets a normal error.
/// - **ContextPatch**: Needs env var injection or config mapping to function.
///   Environment variables (API keys) and config references need ClawDesk-side
///   wiring.
/// - **NeedsRewrite**: OS-restricted or platform-specific; prompt references
///   APIs/tools that only exist on certain platforms.
fn classify_tier(
    meta: &OpenClawMetadata,
    _frontmatter: &OpenClawFrontmatter,
    warnings: &mut Vec<String>,
) -> AdaptationTier {
    // Tier 3: Needs rewrite — platform-specific.
    if !meta.os_filters.is_empty() {
        warnings.push(format!(
            "OS-restricted skill ({}); may need platform-specific rewrite",
            meta.os_filters.join(", ")
        ));
        return AdaptationTier::NeedsRewrite;
    }

    // Tier 2: Context patch — needs env vars or config mapping.
    // These require ClawDesk-side integration to inject secrets or settings.
    let needs_env = !meta.required_env.is_empty() || meta.primary_env.is_some();
    let needs_config = !meta.required_config.is_empty();

    if needs_config {
        warnings.push(format!(
            "Requires ClawDesk config mapping: {}",
            meta.required_config.join(", ")
        ));
        if needs_env {
            warnings.push(format!(
                "Requires environment variables: {}",
                meta.required_env.join(", ")
            ));
        }
        return AdaptationTier::ContextPatch;
    }

    if needs_env {
        warnings.push(format!(
            "Requires environment variables: {}",
            meta.required_env.join(", ")
        ));
        return AdaptationTier::ContextPatch;
    }

    // Tier 1: Direct — works as-is.
    // `required_bins`, `any_bins`, and `install_specs` are just CLI tool
    // references in the prompt documentation. The skill functions fine
    // as a prompt fragment regardless of whether the tools are installed.
    AdaptationTier::Direct
}

// ─────────────────────────────────────────────────────────────
// Directory-level adapter
// ─────────────────────────────────────────────────────────────

/// Load an legacy skill from a directory containing `SKILL.md`.
pub async fn load_openclaw_skill(
    dir: &Path,
    config: &AdapterConfig,
) -> Result<AdaptedSkill, AdapterError> {
    let skill_md = dir.join("SKILL.md");

    if !skill_md.exists() {
        return Err(AdapterError::FileNotFound(
            skill_md.to_string_lossy().to_string(),
        ));
    }

    let content = tokio::fs::read_to_string(&skill_md)
        .await
        .map_err(|e| AdapterError::Io(format!("{}: {}", skill_md.display(), e)))?;

    let (frontmatter, body) = parse_skill_md(&content)?;
    let mut adapted = adapt_skill(&frontmatter, &body, config)?;

    // Set source path.
    adapted.skill.source_path = Some(dir.to_string_lossy().to_string());

    debug!(
        skill = %adapted.skill.manifest.id,
        tier = %adapted.tier,
        tokens = adapted.skill.manifest.estimated_tokens,
        warnings = adapted.warnings.len(),
        "adapted legacy skill"
    );

    Ok(adapted)
}

/// Batch-load all legacy skills from a directory tree.
///
/// Scans `root_dir` for subdirectories containing `SKILL.md` and adapts
/// each one. Returns results partitioned by tier.
pub async fn load_all_openclaw_skills(
    root_dir: &Path,
    config: &AdapterConfig,
) -> BatchAdaptResult {
    let mut result = BatchAdaptResult::default();

    let entries = match std::fs::read_dir(root_dir) {
        Ok(e) => e,
        Err(e) => {
            warn!(dir = %root_dir.display(), error = %e, "failed to read legacy skills dir");
            result.errors.push(format!("{}: {}", root_dir.display(), e));
            return result;
        }
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        match load_openclaw_skill(&path, config).await {
            Ok(adapted) => {
                result.total += 1;
                match adapted.tier {
                    AdaptationTier::Direct => result.direct += 1,
                    AdaptationTier::ContextPatch => result.context_patch += 1,
                    AdaptationTier::NeedsRewrite => result.needs_rewrite += 1,
                }
                result.skills.push(adapted);
            }
            Err(e) => {
                result.errors.push(format!("{}: {}", path.display(), e));
            }
        }
    }

    info!(
        total = result.total,
        direct = result.direct,
        context_patch = result.context_patch,
        needs_rewrite = result.needs_rewrite,
        errors = result.errors.len(),
        "batch-loaded legacy skills"
    );

    result
}

/// Result of batch-loading legacy skills.
#[derive(Debug, Default)]
pub struct BatchAdaptResult {
    pub total: usize,
    pub direct: usize,
    pub context_patch: usize,
    pub needs_rewrite: usize,
    pub skills: Vec<AdaptedSkill>,
    pub errors: Vec<String>,
}

impl BatchAdaptResult {
    /// Generate a triage report as a Markdown table.
    pub fn triage_report(&self) -> String {
        let mut report = String::new();

        report.push_str("# Legacy → ClawDesk Skill Triage Report\n\n");
        report.push_str(&format!(
            "**Total:** {} | **Direct:** {} ({:.0}%) | **Context-patch:** {} ({:.0}%) | **Needs-rewrite:** {} ({:.0}%)\n\n",
            self.total,
            self.direct,
            if self.total > 0 { self.direct as f64 / self.total as f64 * 100.0 } else { 0.0 },
            self.context_patch,
            if self.total > 0 { self.context_patch as f64 / self.total as f64 * 100.0 } else { 0.0 },
            self.needs_rewrite,
            if self.total > 0 { self.needs_rewrite as f64 / self.total as f64 * 100.0 } else { 0.0 },
        ));

        if !self.errors.is_empty() {
            report.push_str(&format!("**Errors:** {}\n\n", self.errors.len()));
        }

        report.push_str("| Skill | Tier | Tokens | Warnings |\n");
        report.push_str("|-------|------|--------|----------|\n");

        let mut sorted: Vec<&AdaptedSkill> = self.skills.iter().collect();
        sorted.sort_by_key(|s| s.skill.manifest.id.name().to_string());

        for adapted in &sorted {
            let warnings_str = if adapted.warnings.is_empty() {
                "—".to_string()
            } else {
                adapted.warnings.join("; ")
            };
            report.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                adapted.skill.manifest.id.name(),
                adapted.tier,
                adapted.skill.manifest.estimated_tokens,
                warnings_str,
            ));
        }

        report
    }
}

// ─────────────────────────────────────────────────────────────
// Export to ClawDesk format
// ─────────────────────────────────────────────────────────────

/// Export an adapted skill to the ClawDesk on-disk format
/// (creates `skill.toml` + `prompt.md` in a target directory).
pub async fn export_to_clawdesk_format(
    adapted: &AdaptedSkill,
    output_dir: &Path,
) -> Result<(), AdapterError> {
    let skill_name = adapted.skill.manifest.id.as_str().replace('/', "-");
    let skill_dir = output_dir.join(&skill_name);

    tokio::fs::create_dir_all(&skill_dir)
        .await
        .map_err(|e| AdapterError::Io(format!("mkdir {}: {}", skill_dir.display(), e)))?;

    // Write skill.toml
    let toml_str = toml::to_string_pretty(&adapted.skill.manifest)
        .map_err(|e| AdapterError::Serialize(format!("TOML: {}", e)))?;
    tokio::fs::write(skill_dir.join("skill.toml"), toml_str)
        .await
        .map_err(|e| AdapterError::Io(format!("write skill.toml: {}", e)))?;

    // Write prompt.md
    tokio::fs::write(skill_dir.join("prompt.md"), &adapted.skill.prompt_fragment)
        .await
        .map_err(|e| AdapterError::Io(format!("write prompt.md: {}", e)))?;

    debug!(
        skill = %adapted.skill.manifest.id,
        dir = %skill_dir.display(),
        "exported legacy skill to ClawDesk format"
    );

    Ok(())
}

// ─────────────────────────────────────────────────────────────
// Error types
// ─────────────────────────────────────────────────────────────

/// Errors from the legacy adapter.
#[derive(Debug, Clone)]
pub enum AdapterError {
    /// No YAML frontmatter found in the file.
    NoFrontmatter,
    /// YAML parsing failed.
    YamlParse(String),
    /// A required field is missing from the frontmatter.
    MissingField(&'static str),
    /// File not found.
    FileNotFound(String),
    /// IO error.
    Io(String),
    /// Serialization error.
    Serialize(String),
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterError::NoFrontmatter => write!(f, "no YAML frontmatter found"),
            AdapterError::YamlParse(e) => write!(f, "YAML parse error: {}", e),
            AdapterError::MissingField(name) => write!(f, "missing required field: {}", name),
            AdapterError::FileNotFound(path) => write!(f, "file not found: {}", path),
            AdapterError::Io(e) => write!(f, "IO error: {}", e),
            AdapterError::Serialize(e) => write!(f, "serialization error: {}", e),
        }
    }
}

impl std::error::Error for AdapterError {}

// ─────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const WEATHER_SKILL: &str = r#"````skill
---
name: weather
description: Get current weather and forecasts (no API key required).
homepage: https://wttr.in/:help
metadata: { "openclaw": { "emoji": "🌤️", "requires": { "bins": ["curl"] } } }
---

# Weather

Two free services, no API keys needed.

## wttr.in (primary)

Quick one-liner:

```bash
curl -s "wttr.in/London?format=3"
```
````"#;

    const NOTION_SKILL: &str = r#"````skill
---
name: notion
description: Notion API for creating and managing pages, databases, and blocks.
homepage: https://developers.notion.com
metadata:
  {
    "openclaw":
      { "emoji": "📝", "requires": { "env": ["NOTION_API_KEY"] }, "primaryEnv": "NOTION_API_KEY" },
  }
---

# Notion

Use the Notion API to create/read/update pages.
````"#;

    const CODING_AGENT_SKILL: &str = r#"````skill
---
name: coding-agent
description: Run Codex CLI, Claude Code, OpenCode, or Pi Coding Agent.
metadata:
  {
    "openclaw": { "emoji": "🧩", "requires": { "anyBins": ["claude", "codex", "opencode", "pi"] } },
  }
---

# Coding Agent (bash-first)

Use bash for all coding agent work.
````"#;

    const SLACK_SKILL: &str = r#"````skill
---
name: slack
description: Use when you need to control Slack from OpenClaw via the slack tool.
metadata: { "openclaw": { "emoji": "💬", "requires": { "config": ["channels.slack"] } } }
---

# Slack Actions

Use slack to react, manage pins, send/edit/delete messages.
````"#;

    const MINIMAL_SKILL: &str = "---\nname: hello\ndescription: A simple greeting skill.\n---\n\n# Hello\n\nSay hello.";

    const ALWAYS_ON_SKILL: &str = r#"---
name: oracle
description: Internal system oracle.
metadata: { "openclaw": { "always": true, "emoji": "🔮" } }
---

# Oracle

Always-on system skill.
"#;

    // ── Parse tests ──

    #[test]
    fn parse_standard_frontmatter() {
        let (fm, body) = parse_skill_md(MINIMAL_SKILL).unwrap();
        assert_eq!(fm.name.as_deref(), Some("hello"));
        assert_eq!(fm.description.as_deref(), Some("A simple greeting skill."));
        assert!(body.contains("# Hello"));
    }

    #[test]
    fn parse_fenced_block_format() {
        let (fm, body) = parse_skill_md(WEATHER_SKILL).unwrap();
        assert_eq!(fm.name.as_deref(), Some("weather"));
        assert!(body.contains("wttr.in"));
    }

    #[test]
    fn parse_notion_multiline_metadata() {
        let (fm, _body) = parse_skill_md(NOTION_SKILL).unwrap();
        assert_eq!(fm.name.as_deref(), Some("notion"));
        assert!(fm.metadata.is_some());
        let meta = resolve_metadata(&fm);
        assert_eq!(meta.required_env, vec!["NOTION_API_KEY"]);
        assert_eq!(meta.primary_env.as_deref(), Some("NOTION_API_KEY"));
        assert_eq!(meta.emoji.as_deref(), Some("📝"));
    }

    #[test]
    fn parse_no_frontmatter_errors() {
        let result = parse_skill_md("# Just some markdown\n\nNo frontmatter here.");
        assert!(matches!(result, Err(AdapterError::NoFrontmatter)));
    }

    #[test]
    fn parse_missing_name_errors() {
        let content = "---\ndescription: No name field\n---\n\n# Body\n";
        let (fm, body) = parse_skill_md(content).unwrap();
        let result = adapt_skill(&fm, &body, &AdapterConfig::default());
        assert!(matches!(result, Err(AdapterError::MissingField("name"))));
    }

    // ── Metadata resolution tests ──

    #[test]
    fn resolve_weather_metadata() {
        let (fm, _) = parse_skill_md(WEATHER_SKILL).unwrap();
        let meta = resolve_metadata(&fm);
        assert_eq!(meta.required_bins, vec!["curl"]);
        assert_eq!(meta.emoji.as_deref(), Some("🌤️"));
        assert!(!meta.always);
        assert!(meta.required_env.is_empty());
    }

    #[test]
    fn resolve_always_on_metadata() {
        let (fm, _) = parse_skill_md(ALWAYS_ON_SKILL).unwrap();
        let meta = resolve_metadata(&fm);
        assert!(meta.always);
        assert_eq!(meta.emoji.as_deref(), Some("🔮"));
    }

    #[test]
    fn resolve_coding_agent_any_bins() {
        let (fm, _) = parse_skill_md(CODING_AGENT_SKILL).unwrap();
        let meta = resolve_metadata(&fm);
        assert_eq!(meta.any_bins.len(), 4);
        assert!(meta.any_bins.contains(&"claude".to_string()));
        assert!(meta.any_bins.contains(&"codex".to_string()));
    }

    // ── Adaptation tests ──

    #[test]
    fn adapt_weather_direct_tier() {
        let (fm, body) = parse_skill_md(WEATHER_SKILL).unwrap();
        let adapted = adapt_skill(&fm, &body, &AdapterConfig::default()).unwrap();

        assert_eq!(adapted.skill.manifest.id.namespace(), "openclaw");
        assert_eq!(adapted.skill.manifest.id.name(), "weather");
        assert_eq!(adapted.skill.manifest.display_name, "Weather");
        assert_eq!(adapted.tier, AdaptationTier::Direct);
        assert!(adapted.skill.prompt_fragment.contains("wttr.in"));
        assert!(adapted.skill.prompt_fragment.contains("Ported from OpenClaw"));
        assert!(adapted.skill.manifest.tags.contains(&"openclaw".to_string()));
        assert!(adapted.skill.manifest.tags.contains(&"requires-bin:curl".to_string()));
        assert!(adapted.warnings.is_empty());
    }

    #[test]
    fn adapt_notion_context_patch_tier() {
        let (fm, body) = parse_skill_md(NOTION_SKILL).unwrap();
        let adapted = adapt_skill(&fm, &body, &AdapterConfig::default()).unwrap();

        assert_eq!(adapted.tier, AdaptationTier::ContextPatch);
        assert!(adapted.warnings.iter().any(|w| w.contains("NOTION_API_KEY")));
        assert!(adapted.skill.manifest.tags.contains(&"primary-env:NOTION_API_KEY".to_string()));
    }

    #[test]
    fn adapt_coding_agent_direct_tier() {
        let (fm, body) = parse_skill_md(CODING_AGENT_SKILL).unwrap();
        let adapted = adapt_skill(&fm, &body, &AdapterConfig::default()).unwrap();

        // anyBins are just CLI tool references — Direct tier.
        assert_eq!(adapted.tier, AdaptationTier::Direct);
        assert!(adapted.warnings.is_empty());
    }

    #[test]
    fn adapt_slack_context_patch_for_config() {
        let (fm, body) = parse_skill_md(SLACK_SKILL).unwrap();
        let adapted = adapt_skill(&fm, &body, &AdapterConfig::default()).unwrap();

        assert_eq!(adapted.tier, AdaptationTier::ContextPatch);
        assert!(adapted.warnings.iter().any(|w| w.contains("config mapping")));
    }

    #[test]
    fn adapt_always_on_trigger() {
        let (fm, body) = parse_skill_md(ALWAYS_ON_SKILL).unwrap();
        let adapted = adapt_skill(&fm, &body, &AdapterConfig::default()).unwrap();

        assert!(adapted
            .skill
            .manifest
            .triggers
            .iter()
            .any(|t| matches!(t, SkillTrigger::Always)));
    }

    #[test]
    fn adapt_minimal_skill_has_keyword_and_command_triggers() {
        let (fm, body) = parse_skill_md(MINIMAL_SKILL).unwrap();
        let adapted = adapt_skill(&fm, &body, &AdapterConfig::default()).unwrap();

        assert!(adapted
            .skill
            .manifest
            .triggers
            .iter()
            .any(|t| matches!(t, SkillTrigger::Keywords { .. })));
        assert!(adapted
            .skill
            .manifest
            .triggers
            .iter()
            .any(|t| matches!(t, SkillTrigger::Command { command } if command == "/hello")));
    }

    #[test]
    fn adapt_multi_word_display_name() {
        let (fm, body) = parse_skill_md(CODING_AGENT_SKILL).unwrap();
        let adapted = adapt_skill(&fm, &body, &AdapterConfig::default()).unwrap();
        assert_eq!(adapted.skill.manifest.display_name, "Coding Agent");
    }

    #[test]
    fn content_hash_is_deterministic() {
        let (fm, body) = parse_skill_md(WEATHER_SKILL).unwrap();
        let a = adapt_skill(&fm, &body, &AdapterConfig::default()).unwrap();
        let b = adapt_skill(&fm, &body, &AdapterConfig::default()).unwrap();
        assert_eq!(
            a.skill.manifest.content_hash,
            b.skill.manifest.content_hash
        );
        assert!(a.skill.manifest.content_hash.is_some());
    }

    #[test]
    fn custom_namespace_config() {
        let config = AdapterConfig {
            namespace: "custom".to_string(),
            ..Default::default()
        };
        let (fm, body) = parse_skill_md(MINIMAL_SKILL).unwrap();
        let adapted = adapt_skill(&fm, &body, &config).unwrap();
        assert_eq!(adapted.skill.manifest.id.namespace(), "custom");
    }

    #[test]
    fn token_cost_is_positive() {
        let (fm, body) = parse_skill_md(WEATHER_SKILL).unwrap();
        let adapted = adapt_skill(&fm, &body, &AdapterConfig::default()).unwrap();
        assert!(adapted.skill.token_cost() > 0);
    }

    // ── Triage report test ──

    #[test]
    fn triage_report_format() {
        let mut result = BatchAdaptResult::default();

        for content in [WEATHER_SKILL, NOTION_SKILL, CODING_AGENT_SKILL, SLACK_SKILL] {
            let (fm, body) = parse_skill_md(content).unwrap();
            let adapted = adapt_skill(&fm, &body, &AdapterConfig::default()).unwrap();
            match adapted.tier {
                AdaptationTier::Direct => result.direct += 1,
                AdaptationTier::ContextPatch => result.context_patch += 1,
                AdaptationTier::NeedsRewrite => result.needs_rewrite += 1,
            }
            result.total += 1;
            result.skills.push(adapted);
        }

        let report = result.triage_report();
        assert!(report.contains("Triage Report"));
        assert!(report.contains("weather"));
        assert!(report.contains("notion"));
        assert!(report.contains("| Skill |"));
        // At least weather and coding-agent should be direct.
        assert!(report.contains("direct"));
        // Notion (requires env) should be context-patch.
        assert!(report.contains("context-patch"));
    }

    // ── OS-filter rewrite tier ──

    #[test]
    fn os_restricted_skill_needs_rewrite() {
        let content = r#"---
name: macos-only
description: Only works on macOS.
metadata: { "openclaw": { "os": ["darwin"] } }
---

# macOS-Only Skill
"#;
        let (fm, body) = parse_skill_md(content).unwrap();
        let adapted = adapt_skill(&fm, &body, &AdapterConfig::default()).unwrap();
        assert_eq!(adapted.tier, AdaptationTier::NeedsRewrite);
        assert!(adapted.warnings.iter().any(|w| w.contains("OS-restricted")));
    }

    // ── Tool binding extraction tests ──

    #[test]
    fn tool_bindings_from_allowed_tools() {
        let content = r#"````skill
---
name: test-tools
description: Skill with allowed tools.
allowed-tools: ["web_fetch", "custom_tool"]
metadata: { "openclaw": { "emoji": "🔧" } }
---

# Test
Use `web_fetch` and `custom_tool`.
````"#;
        let (fm, body) = parse_skill_md(content).unwrap();
        let adapted = adapt_skill(&fm, &body, &AdapterConfig::default()).unwrap();
        // allowed-tools should produce tool bindings
        assert!(
            adapted.skill.provided_tools.iter().any(|t| t.tool_name == "web_fetch"),
            "should have web_fetch from allowed-tools"
        );
        assert!(
            adapted.skill.provided_tools.iter().any(|t| t.tool_name == "custom_tool"),
            "should have custom_tool from allowed-tools"
        );
    }

    #[test]
    fn tool_bindings_from_required_bins() {
        let (fm, body) = parse_skill_md(WEATHER_SKILL).unwrap();
        let adapted = adapt_skill(&fm, &body, &AdapterConfig::default()).unwrap();
        // curl is a system binary, should NOT be a provided tool
        assert!(
            !adapted.skill.provided_tools.iter().any(|t| t.tool_name == "curl"),
            "system binaries like curl should not be provided_tools"
        );
    }

    #[test]
    fn tool_bindings_from_skill_specific_bins() {
        let content = r#"---
name: himalaya-email
description: Email client using himalaya.
metadata: { "openclaw": { "requires": { "bins": ["himalaya"] } } }
---

# Email
Use `himalaya` to send emails.
"#;
        let (fm, body) = parse_skill_md(content).unwrap();
        let adapted = adapt_skill(&fm, &body, &AdapterConfig::default()).unwrap();
        // himalaya is skill-specific, should be a provided tool
        assert!(
            adapted.skill.provided_tools.iter().any(|t| t.tool_name == "himalaya"),
            "skill-specific binaries should be provided_tools"
        );
    }

    #[test]
    fn tool_bindings_from_any_bins() {
        let (fm, body) = parse_skill_md(CODING_AGENT_SKILL).unwrap();
        let adapted = adapt_skill(&fm, &body, &AdapterConfig::default()).unwrap();
        // anyBins like claude, codex should appear as provided tools
        assert!(
            adapted.skill.provided_tools.iter().any(|t| t.tool_name == "claude"),
            "anyBins should produce provided_tools"
        );
        assert!(
            adapted.skill.provided_tools.iter().any(|t| t.tool_name == "codex"),
            "anyBins should produce provided_tools"
        );
    }
}
