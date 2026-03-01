//! Multi-layer skill loading with precedence resolution.
//!
//! ## Multi-Layer Loading (P1)
//!
//! Implements the 6-layer skill loading precedence:
//! ```text
//! Embedded < ExtraDirs < Bundled < Managed < PersonalAgents < ProjectAgents < Workspace
//! ```
//! Later layers override earlier ones by skill name — enabling user customization
//! of bundled skills without modifying the installation.
//!
//! ## Directory conventions 
//!
//! | Layer            | Path                                |
//! |------------------|-------------------------------------|
//! | Embedded         | (compiled into binary)              |
//! | Bundled          | (Rust `bundled.rs` skills)          |
//! | Managed          | `~/.clawdesk/skills/`               |
//! | PersonalAgents   | `~/.agents/skills/`                 |
//! | ProjectAgents    | `<workspace>/.agents/skills/`       |
//! | Workspace        | `<workspace>/skills/`               |
//!
//! ## Limits 
//! - `MAX_CANDIDATES_PER_ROOT = 300`
//! - `MAX_SKILLS_LOADED_PER_SOURCE = 200`
//! - `MAX_SKILL_FILE_BYTES = 256_000`

use crate::definition::{Skill, SkillId, SkillSource};
use crate::embedded;
use crate::openclaw_adapter::{self, AdapterConfig};
use crate::registry::SkillRegistry;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Maximum directory entries to scan per root directory.
const MAX_CANDIDATES_PER_ROOT: usize = 300;

/// Maximum skills loaded from a single source layer.
const MAX_SKILLS_LOADED_PER_SOURCE: usize = 200;

/// Maximum SKILL.md file size in bytes.
const MAX_SKILL_FILE_BYTES: u64 = 256_000;

/// Skill loading layer — determines override precedence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SkillLayer {
    /// Compiled into the binary SKILL.md files.
    Embedded = 0,
    /// Extra directories specified via config or CLI.
    ExtraDirs = 1,
    /// Rust-coded bundled skills (core/web-search, etc.).
    Bundled = 2,
    /// User-managed skills in `~/.clawdesk/skills/`.
    Managed = 3,
    /// Personal agent skills in `~/.agents/skills/`.
    PersonalAgents = 4,
    /// Project-level agent skills in `<workspace>/.agents/skills/`.
    ProjectAgents = 5,
    /// Workspace-level skills in `<workspace>/skills/`.
    Workspace = 6,
}

impl std::fmt::Display for SkillLayer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SkillLayer::Embedded => write!(f, "embedded"),
            SkillLayer::ExtraDirs => write!(f, "extra-dirs"),
            SkillLayer::Bundled => write!(f, "bundled"),
            SkillLayer::Managed => write!(f, "managed"),
            SkillLayer::PersonalAgents => write!(f, "personal-agents"),
            SkillLayer::ProjectAgents => write!(f, "project-agents"),
            SkillLayer::Workspace => write!(f, "workspace"),
        }
    }
}

/// Result of a layered skill loading operation.
#[derive(Debug)]
pub struct LayeredLoadResult {
    /// Skills loaded per layer.
    pub layer_counts: HashMap<SkillLayer, usize>,
    /// Total skills in the final registry (after override resolution).
    pub total: usize,
    /// Skills overridden by higher-priority layers.
    pub overrides: Vec<(String, SkillLayer, SkillLayer)>,
    /// Errors encountered during loading.
    pub errors: Vec<String>,
}

/// Configuration for layered skill loading.
#[derive(Debug, Clone)]
pub struct LayeredLoaderConfig {
    /// Workspace root directory (for project/workspace skills).
    pub workspace_dir: Option<PathBuf>,
    /// Extra directories to scan for skills.
    pub extra_dirs: Vec<PathBuf>,
    /// Whether to include embedded skills.
    pub include_embedded: bool,
    /// Whether to include Rust bundled skills.
    pub include_bundled: bool,
    /// Whether to auto-activate all loaded skills.
    pub auto_activate: bool,
    /// legacy adapter configuration.
    pub adapter_config: AdapterConfig,
}

impl Default for LayeredLoaderConfig {
    fn default() -> Self {
        Self {
            workspace_dir: None,
            extra_dirs: vec![],
            include_embedded: true,
            include_bundled: true,
            auto_activate: true,
            adapter_config: AdapterConfig::default(),
        }
    }
}

/// Multi-layer skill loader with override precedence.
pub struct LayeredLoader {
    config: LayeredLoaderConfig,
}

impl LayeredLoader {
    pub fn new(config: LayeredLoaderConfig) -> Self {
        Self { config }
    }

    /// Load skills from all layers with precedence resolution.
    ///
    /// Later layers override earlier ones by skill name.
    /// Returns a fully populated registry and loading statistics.
    pub async fn load_all(&self) -> (SkillRegistry, LayeredLoadResult) {
        let mut skill_map: HashMap<String, (SkillLayer, Skill, SkillSource)> = HashMap::new();
        let mut result = LayeredLoadResult {
            layer_counts: HashMap::new(),
            total: 0,
            overrides: vec![],
            errors: vec![],
        };

        // Layer 0: Embedded legacy skills
        if self.config.include_embedded {
            let count = self.load_embedded_layer(&mut skill_map, &mut result);
            result.layer_counts.insert(SkillLayer::Embedded, count);
        }

        // Layer 1: Extra directories
        for dir in &self.config.extra_dirs {
            let count = self
                .load_directory_layer(dir, SkillLayer::ExtraDirs, &mut skill_map, &mut result)
                .await;
            *result.layer_counts.entry(SkillLayer::ExtraDirs).or_insert(0) += count;
        }

        // Layer 2: Rust bundled skills
        if self.config.include_bundled {
            let count = self.load_bundled_layer(&mut skill_map, &mut result);
            result.layer_counts.insert(SkillLayer::Bundled, count);
        }

        // Layer 3: Managed (~/.clawdesk/skills/)
        let managed_dir = home_dir().join(".clawdesk").join("skills");
        if managed_dir.exists() {
            let count = self
                .load_directory_layer(&managed_dir, SkillLayer::Managed, &mut skill_map, &mut result)
                .await;
            result.layer_counts.insert(SkillLayer::Managed, count);
        }

        // Layer 4: Personal agents (~/.agents/skills/)
        let personal_dir = home_dir().join(".agents").join("skills");
        if personal_dir.exists() {
            let count = self
                .load_directory_layer(
                    &personal_dir,
                    SkillLayer::PersonalAgents,
                    &mut skill_map,
                    &mut result,
                )
                .await;
            result.layer_counts.insert(SkillLayer::PersonalAgents, count);
        }

        // Layer 5 & 6: Project and workspace (if workspace_dir set)
        if let Some(ref ws) = self.config.workspace_dir {
            // Project agents: <workspace>/.agents/skills/
            let project_dir = ws.join(".agents").join("skills");
            if project_dir.exists() {
                let count = self
                    .load_directory_layer(
                        &project_dir,
                        SkillLayer::ProjectAgents,
                        &mut skill_map,
                        &mut result,
                    )
                    .await;
                result.layer_counts.insert(SkillLayer::ProjectAgents, count);
            }

            // Workspace: <workspace>/skills/
            let workspace_skills = ws.join("skills");
            if workspace_skills.exists() {
                let count = self
                    .load_directory_layer(
                        &workspace_skills,
                        SkillLayer::Workspace,
                        &mut skill_map,
                        &mut result,
                    )
                    .await;
                result.layer_counts.insert(SkillLayer::Workspace, count);
            }
        }

        // Build final registry
        let mut registry = SkillRegistry::new();
        for (_name, (layer, skill, source)) in &skill_map {
            let id = skill.manifest.id.clone();
            registry.register(skill.clone(), source.clone());
            if self.config.auto_activate {
                let _ = registry.activate(&id);
            }
            debug!(skill = %id, layer = %layer, "registered skill from layer");
        }

        result.total = skill_map.len();

        info!(
            total = result.total,
            layers = ?result.layer_counts,
            overrides = result.overrides.len(),
            errors = result.errors.len(),
            "layered skill loading complete"
        );

        (registry, result)
    }

    /// Load embedded legacy skills (Layer 0).
    fn load_embedded_layer(
        &self,
        skill_map: &mut HashMap<String, (SkillLayer, Skill, SkillSource)>,
        result: &mut LayeredLoadResult,
    ) -> usize {
        let skills = embedded::embedded_skills();
        let mut count = 0;

        for (name, content) in skills {
            match openclaw_adapter::parse_skill_md(content)
                .and_then(|(fm, body)| {
                    openclaw_adapter::adapt_skill(&fm, &body, &self.config.adapter_config)
                })
            {
                Ok(adapted) => {
                    let skill_name = adapted.skill.manifest.id.name().to_string();
                    insert_with_precedence(
                        skill_map,
                        &skill_name,
                        SkillLayer::Embedded,
                        adapted.skill,
                        SkillSource::Builtin,
                        result,
                    );
                    count += 1;
                }
                Err(e) => {
                    result.errors.push(format!("embedded/{}: {}", name, e));
                }
            }
        }

        count
    }

    /// Load Rust bundled skills (Layer 2).
    fn load_bundled_layer(
        &self,
        skill_map: &mut HashMap<String, (SkillLayer, Skill, SkillSource)>,
        result: &mut LayeredLoadResult,
    ) -> usize {
        let bundled_registry = crate::bundled::load_bundled_skills();
        let entries: Vec<_> = bundled_registry.list();
        let mut count = 0;

        for info in &entries {
            if let Some(entry) = bundled_registry.get(&info.id) {
                let skill_name = info.id.name().to_string();
                let skill = (*entry.skill).clone();
                insert_with_precedence(
                    skill_map,
                    &skill_name,
                    SkillLayer::Bundled,
                    skill,
                    SkillSource::Builtin,
                    result,
                );
                count += 1;
            }
        }

        count
    }

    /// Load skills from a filesystem directory (Layers 1, 3-6).
    async fn load_directory_layer(
        &self,
        dir: &Path,
        layer: SkillLayer,
        skill_map: &mut HashMap<String, (SkillLayer, Skill, SkillSource)>,
        result: &mut LayeredLoadResult,
    ) -> usize {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(e) => {
                result.errors.push(format!("{}: read_dir: {}", dir.display(), e));
                return 0;
            }
        };

        let mut candidates: Vec<_> = entries.flatten().collect();
        candidates.truncate(MAX_CANDIDATES_PER_ROOT);

        let mut count = 0;
        for entry in candidates {
            if count >= MAX_SKILLS_LOADED_PER_SOURCE {
                break;
            }

            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            // Try SKILL.md first, then skill.toml+prompt.md
            let skill_md = path.join("SKILL.md");
            let skill_toml = path.join("skill.toml");

            let skill_result = if skill_md.exists() {
                self.load_openclaw_skill(&skill_md).await
            } else if skill_toml.exists() {
                self.load_native_skill(&path).await
            } else {
                // Check for nested skills root (the resolveNestedSkillsRoot)
                let nested = path.join("skills");
                if nested.is_dir() {
                    // Recurse into nested skills directory
                    let nested_count = Box::pin(self.load_directory_layer(
                        &nested,
                        layer,
                        skill_map,
                        result,
                    ))
                    .await;
                    count += nested_count;
                }
                continue;
            };

            match skill_result {
                Ok(skill) => {
                    let skill_name = skill.manifest.id.name().to_string();
                    let source = SkillSource::Local {
                        path: path.to_string_lossy().to_string(),
                    };
                    insert_with_precedence(skill_map, &skill_name, layer, skill, source, result);
                    count += 1;
                }
                Err(e) => {
                    result
                        .errors
                        .push(format!("{}: {}", path.display(), e));
                }
            }
        }

        debug!(layer = %layer, dir = %dir.display(), count, "loaded skills from directory");
        count
    }

    /// Load a single SKILL.md file.
    async fn load_openclaw_skill(&self, path: &Path) -> Result<Skill, String> {
        // Size check
        let metadata = std::fs::metadata(path)
            .map_err(|e| format!("stat {}: {}", path.display(), e))?;
        if metadata.len() > MAX_SKILL_FILE_BYTES {
            return Err(format!(
                "SKILL.md too large ({} bytes, max {})",
                metadata.len(),
                MAX_SKILL_FILE_BYTES
            ));
        }

        let content = tokio::fs::read_to_string(path)
            .await
            .map_err(|e| format!("read {}: {}", path.display(), e))?;

        let (fm, body) =
            openclaw_adapter::parse_skill_md(&content).map_err(|e| format!("parse: {}", e))?;

        let adapted = openclaw_adapter::adapt_skill(&fm, &body, &self.config.adapter_config)
            .map_err(|e| format!("adapt: {}", e))?;

        let mut skill = adapted.skill;
        skill.source_path = path.parent().map(|p| p.to_string_lossy().to_string());
        Ok(skill)
    }

    /// Load a native ClawDesk skill (skill.toml + prompt.md).
    async fn load_native_skill(&self, dir: &Path) -> Result<Skill, String> {
        let manifest_path = dir.join("skill.toml");
        let prompt_path = dir.join("prompt.md");

        let manifest_str = tokio::fs::read_to_string(&manifest_path)
            .await
            .map_err(|e| format!("read {}: {}", manifest_path.display(), e))?;

        let manifest: crate::definition::SkillManifest =
            toml::from_str(&manifest_str).map_err(|e| format!("TOML: {}", e))?;

        let prompt_fragment = if prompt_path.exists() {
            tokio::fs::read_to_string(&prompt_path)
                .await
                .map_err(|e| format!("read {}: {}", prompt_path.display(), e))?
        } else {
            manifest.description.clone()
        };

        Ok(Skill {
            manifest,
            prompt_fragment,
            provided_tools: vec![],
            parameter_values: serde_json::Value::Null,
            source_path: Some(dir.to_string_lossy().to_string()),
        })
    }
}

/// Insert a skill with override tracking.
///
/// If the skill name already exists from a lower-priority layer,
/// the higher-priority layer wins and the override is recorded.
fn insert_with_precedence(
    map: &mut HashMap<String, (SkillLayer, Skill, SkillSource)>,
    name: &str,
    layer: SkillLayer,
    skill: Skill,
    source: SkillSource,
    result: &mut LayeredLoadResult,
) {
    if let Some((existing_layer, _, _)) = map.get(name) {
        if layer >= *existing_layer {
            result.overrides.push((
                name.to_string(),
                *existing_layer,
                layer,
            ));
            map.insert(name.to_string(), (layer, skill, source));
        }
        // Lower layer doesn't override — skip
    } else {
        map.insert(name.to_string(), (layer, skill, source));
    }
}

/// Get the user's home directory.
fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

// ═══════════════════════════════════════════════════════════════════════════
// Tests
// ═══════════════════════════════════════════════════════════════════════════

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_ordering() {
        assert!(SkillLayer::Embedded < SkillLayer::Bundled);
        assert!(SkillLayer::Bundled < SkillLayer::Managed);
        assert!(SkillLayer::Managed < SkillLayer::PersonalAgents);
        assert!(SkillLayer::PersonalAgents < SkillLayer::ProjectAgents);
        assert!(SkillLayer::ProjectAgents < SkillLayer::Workspace);
    }

    #[test]
    fn override_precedence() {
        let mut map: HashMap<String, (SkillLayer, Skill, SkillSource)> = HashMap::new();
        let mut result = LayeredLoadResult {
            layer_counts: HashMap::new(),
            total: 0,
            overrides: vec![],
            errors: vec![],
        };

        let make_skill = |name: &str, display: &str| -> Skill {
            Skill {
                manifest: crate::definition::SkillManifest {
                    id: SkillId::new("test", name),
                    display_name: display.to_string(),
                    description: "test".to_string(),
                    version: "0.1.0".to_string(),
                    author: None,
                    dependencies: vec![],
                    required_tools: vec![],
                    parameters: vec![],
                    triggers: vec![],
                    estimated_tokens: 100,
                    priority_weight: 1.0,
                    tags: vec![],
                    signature: None,
                    publisher_key: None,
                    content_hash: None,
                    schema_version: 1,
                },
                prompt_fragment: "test".to_string(),
                provided_tools: vec![],
                parameter_values: serde_json::Value::Null,
                source_path: None,
            }
        };

        // Insert from embedded layer
        insert_with_precedence(
            &mut map,
            "weather",
            SkillLayer::Embedded,
            make_skill("weather", "Embedded Weather"),
            SkillSource::Builtin,
            &mut result,
        );

        // Override from managed layer
        insert_with_precedence(
            &mut map,
            "weather",
            SkillLayer::Managed,
            make_skill("weather", "Custom Weather"),
            SkillSource::Local { path: "/custom".into() },
            &mut result,
        );

        assert_eq!(map.len(), 1);
        assert_eq!(map["weather"].0, SkillLayer::Managed);
        assert_eq!(map["weather"].1.manifest.display_name, "Custom Weather");
        assert_eq!(result.overrides.len(), 1);
        assert_eq!(result.overrides[0].0, "weather");
    }

    #[test]
    fn lower_layer_does_not_override() {
        let mut map: HashMap<String, (SkillLayer, Skill, SkillSource)> = HashMap::new();
        let mut result = LayeredLoadResult {
            layer_counts: HashMap::new(),
            total: 0,
            overrides: vec![],
            errors: vec![],
        };

        let make_skill = |display: &str| -> Skill {
            Skill {
                manifest: crate::definition::SkillManifest {
                    id: SkillId::new("test", "skill"),
                    display_name: display.to_string(),
                    description: "test".to_string(),
                    version: "0.1.0".to_string(),
                    author: None,
                    dependencies: vec![],
                    required_tools: vec![],
                    parameters: vec![],
                    triggers: vec![],
                    estimated_tokens: 100,
                    priority_weight: 1.0,
                    tags: vec![],
                    signature: None,
                    publisher_key: None,
                    content_hash: None,
                    schema_version: 1,
                },
                prompt_fragment: "test".to_string(),
                provided_tools: vec![],
                parameter_values: serde_json::Value::Null,
                source_path: None,
            }
        };

        // Insert from workspace layer first
        insert_with_precedence(
            &mut map,
            "skill",
            SkillLayer::Workspace,
            make_skill("Workspace"),
            SkillSource::Local { path: "/ws".into() },
            &mut result,
        );

        // Try to insert from embedded layer — should NOT override
        insert_with_precedence(
            &mut map,
            "skill",
            SkillLayer::Embedded,
            make_skill("Embedded"),
            SkillSource::Builtin,
            &mut result,
        );

        assert_eq!(map["skill"].1.manifest.display_name, "Workspace");
        assert_eq!(result.overrides.len(), 0);
    }

    #[tokio::test]
    async fn load_all_includes_embedded_and_bundled() {
        let config = LayeredLoaderConfig {
            include_embedded: true,
            include_bundled: true,
            auto_activate: false,
            ..Default::default()
        };
        let loader = LayeredLoader::new(config);
        let (registry, result) = loader.load_all().await;

        // Should have at least bundled skills (15 Rust + 6 design = 21)
        let bundled_count = result.layer_counts.get(&SkillLayer::Bundled).copied().unwrap_or(0);
        assert!(bundled_count >= 15, "expected 15+ bundled skills, got {}", bundled_count);

        // If embedded skills are available, total should be higher
        if embedded::embedded_skill_count() > 0 {
            assert!(result.total >= 40, "expected 40+ total skills, got {}", result.total);
        }
    }

    #[test]
    fn layer_display() {
        assert_eq!(format!("{}", SkillLayer::Embedded), "embedded");
        assert_eq!(format!("{}", SkillLayer::Workspace), "workspace");
        assert_eq!(format!("{}", SkillLayer::PersonalAgents), "personal-agents");
    }
}
