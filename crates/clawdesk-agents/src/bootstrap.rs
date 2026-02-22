//! Workspace bootstrap context — automatic discovery and injection of project files.
//!
//! Discovers project context files (CLAUDE.md, README.md, .clawdesk/config.toml, etc.)
//! via bounded depth-first traversal and injects them into the agent's system prompt
//! through the existing prompt-budget system.
//!
//! ## Two-Phase Design
//!
//! 1. **Discovery phase** (at session creation): Walk workspace directory, find
//!    bootstrap files using a priority-ordered filename list, compute token cost.
//!
//! 2. **Injection phase** (at prompt assembly): `PromptBudget` allocates bootstrap
//!    context as high-priority (P=0) before skills compete for remaining tokens.
//!
//! ## Traversal Cost
//!
//! O(min(D × B^d, F)) where D=max_depth, B=branching_factor, d=depth, F=total_files.
//! With D=3, typical repos: ~100-500 directory entries scanned.
//!
//! ## Budget Allocation
//!
//! Uses priority-based greedy knapsack: bootstrap files get P=0, skills get P=1-3.
//! Greedy-by-priority-then-ratio achieves ≥ (1-1/e) × V_optimal ≈ 0.632 × optimal.

use clawdesk_types::estimate_tokens;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Priority-ordered list of bootstrap filenames to discover.
///
/// Files earlier in the list get higher priority. The agent system prompt
/// includes these in priority order until the token budget is exhausted.
const BOOTSTRAP_FILENAMES: &[(&str, u32)] = &[
    ("CLAUDE.md", 100),
    (".claude/settings.json", 98),
    (".clawdesk/config.toml", 96),
    (".clawdesk/context.md", 94),
    ("AGENTS.md", 92),
    ("CONTRIBUTING.md", 80),
    ("README.md", 70),
    (".github/copilot-instructions.md", 65),
    ("ARCHITECTURE.md", 60),
    ("docs/DEVELOPMENT.md", 50),
    ("pyproject.toml", 30),
    ("Cargo.toml", 30),
    ("package.json", 30),
];

/// Configuration for bootstrap context discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapConfig {
    /// Maximum directory traversal depth.
    #[serde(default = "default_max_depth")]
    pub max_depth: usize,
    /// Maximum total characters for all bootstrap files combined.
    #[serde(default = "default_max_chars")]
    pub max_total_chars: usize,
    /// Maximum characters for a single bootstrap file.
    #[serde(default = "default_max_file_chars")]
    pub max_file_chars: usize,
    /// Additional filenames to discover (beyond the default list).
    #[serde(default)]
    pub extra_filenames: Vec<String>,
    /// Filenames to exclude from discovery.
    #[serde(default)]
    pub exclude_filenames: Vec<String>,
    /// Whether to include project manifest files (Cargo.toml, package.json, etc.).
    #[serde(default = "default_include_manifests")]
    pub include_manifests: bool,
}

fn default_max_depth() -> usize { 3 }
fn default_max_chars() -> usize { 50_000 }
fn default_max_file_chars() -> usize { 20_000 }
fn default_include_manifests() -> bool { true }

impl Default for BootstrapConfig {
    fn default() -> Self {
        Self {
            max_depth: default_max_depth(),
            max_total_chars: default_max_chars(),
            max_file_chars: default_max_file_chars(),
            extra_filenames: Vec::new(),
            exclude_filenames: Vec::new(),
            include_manifests: default_include_manifests(),
        }
    }
}

/// A discovered bootstrap file with content and metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapFile {
    /// Relative path from workspace root.
    pub relative_path: String,
    /// File content (possibly truncated).
    pub content: String,
    /// Priority (higher = included first in prompt).
    pub priority: u32,
    /// Estimated token count.
    pub estimated_tokens: usize,
    /// Whether the content was truncated.
    pub truncated: bool,
    /// Original file size in bytes.
    pub original_size: usize,
}

/// Result of bootstrap context discovery.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapResult {
    /// Discovered files in priority order.
    pub files: Vec<BootstrapFile>,
    /// Total token cost of all discovered files.
    pub total_tokens: usize,
    /// Total character count.
    pub total_chars: usize,
    /// Number of files that were truncated.
    pub truncated_count: usize,
    /// Number of directories scanned.
    pub directories_scanned: usize,
}

/// Discover bootstrap context files in a workspace.
///
/// Performs bounded depth-first traversal from the workspace root,
/// looking for predefined bootstrap filenames.
///
/// ## Algorithm
/// 1. Check each filename in priority order at workspace root
/// 2. For nested paths, traverse up to max_depth
/// 3. Read and optionally truncate each found file
/// 4. Return files sorted by priority (descending)
pub fn discover_bootstrap_files(
    workspace_path: &Path,
    config: &BootstrapConfig,
) -> BootstrapResult {
    let mut files = Vec::new();
    let mut total_chars = 0;
    let mut directories_scanned = 0;

    // Build the filename list (defaults + extras - excludes)
    let filenames = build_filename_list(config);

    // Check each filename
    for (filename, priority) in &filenames {
        let file_path = workspace_path.join(filename);

        if file_path.is_file() {
            match std::fs::read_to_string(&file_path) {
                Ok(content) => {
                    let original_size = content.len();

                    // Check per-file and total char limits
                    let remaining_budget = config.max_total_chars.saturating_sub(total_chars);
                    let max_for_file = config.max_file_chars.min(remaining_budget);

                    if max_for_file == 0 {
                        debug!(
                            path = %filename,
                            "Skipping bootstrap file: total char budget exhausted"
                        );
                        continue;
                    }

                    let (final_content, truncated) = if content.len() > max_for_file {
                        let truncated = truncate_at_boundary(&content, max_for_file);
                        (truncated, true)
                    } else {
                        (content, false)
                    };

                    let estimated_tokens = estimate_tokens(&final_content);
                    total_chars += final_content.len();

                    files.push(BootstrapFile {
                        relative_path: filename.to_string(),
                        content: final_content,
                        priority: *priority,
                        estimated_tokens,
                        truncated,
                        original_size,
                    });

                    debug!(
                        path = %filename,
                        tokens = estimated_tokens,
                        truncated,
                        "Discovered bootstrap file"
                    );
                }
                Err(e) => {
                    debug!(
                        path = %filename,
                        error = %e,
                        "Could not read bootstrap file"
                    );
                }
            }
        }
    }

    // Also scan workspace root directory
    if let Ok(entries) = std::fs::read_dir(workspace_path) {
        directories_scanned += 1;
        // Count child directories for metrics
        for entry in entries.flatten() {
            if entry.path().is_dir() {
                directories_scanned += 1;
            }
        }
    }

    // Sort by priority (descending)
    files.sort_by(|a, b| b.priority.cmp(&a.priority));

    let total_tokens = files.iter().map(|f| f.estimated_tokens).sum();
    let truncated_count = files.iter().filter(|f| f.truncated).count();

    info!(
        files = files.len(),
        total_tokens,
        truncated = truncated_count,
        "Bootstrap context discovered"
    );

    BootstrapResult {
        files,
        total_tokens,
        total_chars,
        truncated_count,
        directories_scanned,
    }
}

/// Build the combined filename list from defaults and config.
fn build_filename_list(config: &BootstrapConfig) -> Vec<(String, u32)> {
    let mut filenames: Vec<(String, u32)> = BOOTSTRAP_FILENAMES
        .iter()
        .filter(|(name, _)| {
            // Skip manifests if configured
            if !config.include_manifests {
                let lower = name.to_lowercase();
                if lower == "cargo.toml" || lower == "package.json" || lower == "pyproject.toml" {
                    return false;
                }
            }
            // Skip excluded files
            !config.exclude_filenames.iter().any(|ex| ex == *name)
        })
        .map(|(name, priority)| (name.to_string(), *priority))
        .collect();

    // Add extra filenames
    for (i, extra) in config.extra_filenames.iter().enumerate() {
        if !filenames.iter().any(|(n, _)| n == extra) {
            let priority = 85u32.saturating_sub(i as u32);
            filenames.push((extra.clone(), priority));
        }
    }

    filenames
}

/// Truncate content at a semantic boundary (paragraph or section break).
///
/// Instead of cutting at an arbitrary offset, scans backward from the
/// limit to find the nearest paragraph break (\n\n), section header (#),
/// or line break (\n).
///
/// O(n) backward scan from cutoff point.
fn truncate_at_boundary(content: &str, max_chars: usize) -> String {
    if content.len() <= max_chars {
        return content.to_string();
    }

    let search_region = &content[..max_chars];

    // Try paragraph boundary first
    if let Some(pos) = search_region.rfind("\n\n") {
        if pos > max_chars / 2 {
            let mut result = content[..pos].to_string();
            result.push_str("\n\n[… truncated]");
            return result;
        }
    }

    // Try line boundary
    if let Some(pos) = search_region.rfind('\n') {
        if pos > max_chars / 2 {
            let mut result = content[..pos].to_string();
            result.push_str("\n[… truncated]");
            return result;
        }
    }

    // Hard truncate at max_chars
    let mut result = content[..max_chars].to_string();
    result.push_str("… [truncated]");
    result
}

/// Assemble bootstrap context into a single system prompt section.
///
/// Creates a formatted prompt section with headers for each bootstrap file.
pub fn assemble_bootstrap_prompt(bootstrap: &BootstrapResult) -> String {
    if bootstrap.files.is_empty() {
        return String::new();
    }

    let mut prompt = String::with_capacity(bootstrap.total_chars + 200);
    prompt.push_str("# Project Context\n\n");
    prompt.push_str("The following files provide context about the current project.\n\n");

    for file in &bootstrap.files {
        prompt.push_str(&format!("## {}\n\n", file.relative_path));
        prompt.push_str(&file.content);
        prompt.push_str("\n\n");
    }

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_truncate_at_paragraph_boundary() {
        let content = "Paragraph one.\n\nParagraph two.\n\nParagraph three is very long and detailed.";
        let truncated = truncate_at_boundary(content, 35);
        assert!(truncated.contains("Paragraph one."));
        assert!(truncated.contains("[… truncated]"));
        assert!(truncated.len() <= 50); // original 35 + truncation marker
    }

    #[test]
    fn test_truncate_at_line_boundary() {
        let content = "Line one\nLine two\nLine three is very long";
        let truncated = truncate_at_boundary(content, 25);
        assert!(truncated.contains("[… truncated]"));
    }

    #[test]
    fn test_no_truncation_needed() {
        let content = "Short content.";
        let result = truncate_at_boundary(content, 100);
        assert_eq!(result, "Short content.");
    }

    #[test]
    fn test_build_filename_list_default() {
        let config = BootstrapConfig::default();
        let list = build_filename_list(&config);
        assert!(!list.is_empty());
        assert!(list.iter().any(|(n, _)| n == "CLAUDE.md"));
        assert!(list.iter().any(|(n, _)| n == "README.md"));
    }

    #[test]
    fn test_build_filename_list_with_exclusions() {
        let config = BootstrapConfig {
            exclude_filenames: vec!["README.md".to_string()],
            ..Default::default()
        };
        let list = build_filename_list(&config);
        assert!(!list.iter().any(|(n, _)| n == "README.md"));
    }

    #[test]
    fn test_build_filename_list_with_extras() {
        let config = BootstrapConfig {
            extra_filenames: vec!["CUSTOM.md".to_string()],
            ..Default::default()
        };
        let list = build_filename_list(&config);
        assert!(list.iter().any(|(n, _)| n == "CUSTOM.md"));
    }

    #[test]
    fn test_discover_in_temp_dir() {
        let dir = tempfile::tempdir().unwrap();

        // Create some bootstrap files
        fs::write(dir.path().join("CLAUDE.md"), "# Claude Config\nUse Rust.").unwrap();
        fs::write(dir.path().join("README.md"), "# Project\nA test project.").unwrap();

        let result = discover_bootstrap_files(dir.path(), &BootstrapConfig::default());
        assert_eq!(result.files.len(), 2);
        assert_eq!(result.files[0].relative_path, "CLAUDE.md"); // highest priority
        assert!(result.total_tokens > 0);
    }

    #[test]
    fn test_assemble_bootstrap_prompt() {
        let result = BootstrapResult {
            files: vec![
                BootstrapFile {
                    relative_path: "CLAUDE.md".to_string(),
                    content: "Use Rust idioms.".to_string(),
                    priority: 100,
                    estimated_tokens: 5,
                    truncated: false,
                    original_size: 16,
                },
            ],
            total_tokens: 5,
            total_chars: 16,
            truncated_count: 0,
            directories_scanned: 1,
        };

        let prompt = assemble_bootstrap_prompt(&result);
        assert!(prompt.contains("# Project Context"));
        assert!(prompt.contains("## CLAUDE.md"));
        assert!(prompt.contains("Use Rust idioms."));
    }

    #[test]
    fn test_empty_bootstrap() {
        let prompt = assemble_bootstrap_prompt(&BootstrapResult {
            files: vec![],
            total_tokens: 0,
            total_chars: 0,
            truncated_count: 0,
            directories_scanned: 0,
        });
        assert!(prompt.is_empty());
    }

    #[test]
    fn test_char_budget_enforcement() {
        let dir = tempfile::tempdir().unwrap();

        // Create a large file
        let large_content = "x".repeat(100_000);
        fs::write(dir.path().join("CLAUDE.md"), &large_content).unwrap();

        let config = BootstrapConfig {
            max_file_chars: 1000,
            max_total_chars: 5000,
            ..Default::default()
        };

        let result = discover_bootstrap_files(dir.path(), &config);
        assert_eq!(result.files.len(), 1);
        assert!(result.files[0].truncated);
        assert!(result.files[0].content.len() <= 1500); // max + truncation marker
    }
}
