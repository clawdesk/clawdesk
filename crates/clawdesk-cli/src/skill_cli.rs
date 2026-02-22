//! Unified `clawdesk skill` CLI subcommand tree.
//!
//! Exposes all skill lifecycle operations through a single, discoverable
//! CLI surface: list, info, search, install, uninstall, update, create,
//! lint, test, audit, check, publish.
//!
//! ## Command dispatch
//!
//! Clap's derive macro generates O(1) enum-based routing (compile-time match).
//! Each subcommand delegates to either:
//! - **Gateway RPC** (list, info, search, install, uninstall, update, check) — tunnels
//!   through the running gateway to ensure single source of truth.
//! - **Local execution** (create, lint, test, audit, publish) — runs directly on
//!   the filesystem without requiring a running gateway.

use clap::Subcommand;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

/// All skill subcommands: `clawdesk skill <action>`.
#[derive(Subcommand)]
pub enum SkillAction {
    /// List all skills (from gateway or local registry).
    List {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
        /// Show only eligible skills.
        #[arg(long)]
        eligible: bool,
        /// Verbose output with trust and dep info.
        #[arg(long)]
        verbose: bool,
    },
    /// Show detailed info about a specific skill.
    Info {
        /// Skill name (e.g., "core/web-research").
        name: String,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Search the skill store catalog.
    Search {
        /// Search query text.
        query: String,
        /// Filter by category.
        #[arg(long)]
        category: Option<String>,
        /// Show only verified/official skills.
        #[arg(long)]
        verified: bool,
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Install a skill from the store or a ref.
    Install {
        /// Skill reference (e.g., "user/web-scraper@1.2.0").
        skill_ref: String,
        /// Force reinstall even if already installed.
        #[arg(long)]
        force: bool,
        /// Dry-run: show what would be installed without doing it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Uninstall a skill.
    Uninstall {
        /// Skill ID to uninstall.
        id: String,
    },
    /// Update installed skills.
    Update {
        /// Update all installed skills.
        #[arg(long)]
        all: bool,
        /// Specific skill ID to update (if --all not set).
        id: Option<String>,
    },
    /// Create a new skill scaffold.
    Create {
        /// Skill ID (e.g., "my-org/my-skill").
        id: String,
        /// Display name.
        #[arg(long)]
        name: Option<String>,
        /// Short description.
        #[arg(long)]
        description: Option<String>,
    },
    /// Lint skill definitions for errors and warnings.
    Lint {
        /// Directory containing skills (defaults to current dir).
        #[arg(long)]
        dir: Option<String>,
    },
    /// Test a skill with sample input (dry-run execution).
    Test {
        /// Directory containing the skill.
        dir: String,
        /// Sample user input to test against.
        #[arg(long)]
        input: String,
    },
    /// Audit installed skills (integrity, trust, freshness).
    Audit {
        /// Output as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Check skill eligibility against current environment.
    Check,
    /// Publish a skill package to the store.
    Publish {
        /// Directory containing the skill to publish.
        dir: String,
    },
}

/// Response from a gateway RPC skill list call.
#[derive(Debug, Deserialize)]
struct SkillListResponse {
    skills: Vec<SkillListEntry>,
    total: usize,
}

/// A single skill entry from the gateway list endpoint.
#[derive(Debug, Serialize, Deserialize)]
struct SkillListEntry {
    id: String,
    display_name: Option<String>,
    version: Option<String>,
    state: Option<String>,
    source: Option<String>,
    estimated_tokens: Option<usize>,
    priority_weight: Option<f64>,
    error: Option<String>,
}

/// Response from a gateway RPC skill search call.
#[derive(Debug, Deserialize)]
struct StoreSearchResponse {
    entries: Vec<StoreEntryResponse>,
    total_count: usize,
}

/// A store catalog entry as returned by the gateway.
#[derive(Debug, Serialize, Deserialize)]
struct StoreEntryResponse {
    skill_id: String,
    display_name: String,
    short_description: String,
    category: String,
    version: String,
    author: String,
    rating: f32,
    install_count: u64,
    verified: bool,
    install_state: String,
    tags: Vec<String>,
}

/// Response for skill install/uninstall operations.
#[derive(Debug, Deserialize)]
struct SkillOpResponse {
    status: Option<String>,
    error: Option<String>,
    id: Option<String>,
}

/// Eligibility check entry.
#[derive(Debug, Deserialize)]
struct EligibilityEntry {
    id: String,
    eligible: bool,
    missing: Vec<String>,
    remote: Option<String>,
}

/// Execute a skill subcommand.
///
/// Commands that query or mutate skill state tunnel through the gateway RPC.
/// Commands that operate on the local filesystem run directly.
pub async fn execute_skill_command(
    gateway_url: &str,
    action: SkillAction,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match action {
        SkillAction::List { json, eligible, verbose } => {
            cmd_skill_list(gateway_url, json, eligible, verbose).await
        }
        SkillAction::Info { name, json } => {
            cmd_skill_info(gateway_url, &name, json).await
        }
        SkillAction::Search { query, category, verified, json } => {
            cmd_skill_search(gateway_url, &query, category, verified, json).await
        }
        SkillAction::Install { skill_ref, force, dry_run } => {
            cmd_skill_install(gateway_url, &skill_ref, force, dry_run).await
        }
        SkillAction::Uninstall { id } => {
            cmd_skill_uninstall(gateway_url, &id).await
        }
        SkillAction::Update { all, id } => {
            cmd_skill_update(gateway_url, all, id).await
        }
        SkillAction::Create { id, name, description } => {
            cmd_skill_create(&id, name.as_deref(), description.as_deref()).await
        }
        SkillAction::Lint { dir } => {
            cmd_skill_lint(dir.as_deref()).await
        }
        SkillAction::Test { dir, input } => {
            cmd_skill_test(&dir, &input).await
        }
        SkillAction::Audit { json } => {
            cmd_skill_audit(gateway_url, json).await
        }
        SkillAction::Check => {
            cmd_skill_check(gateway_url).await
        }
        SkillAction::Publish { dir } => {
            cmd_skill_publish(gateway_url, &dir).await
        }
    }
}

// ── Gateway-RPC commands ─────────────────────────────────────

/// List skills via the gateway admin API.
async fn cmd_skill_list(
    base_url: &str,
    json_out: bool,
    _eligible: bool,
    verbose: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{}/api/v1/admin/skills", base_url);
    let client = reqwest::Client::new();
    let resp = client.get(&url).send().await
        .map_err(|e| format!("failed to connect to gateway at {url}: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        eprintln!("Error {status}: {body}");
        return Ok(());
    }

    let data: SkillListResponse = resp.json().await?;

    if json_out {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!(data.skills))?);
        return Ok(());
    }

    if data.skills.is_empty() {
        println!("No skills loaded.");
        return Ok(());
    }

    if verbose {
        println!(
            "{:<35} {:<12} {:<12} {:<10} {:<8} {}",
            "SKILL", "VERSION", "STATE", "SOURCE", "TOKENS", "ERROR"
        );
        println!("{}", "-".repeat(95));
        for s in &data.skills {
            println!(
                "{:<35} {:<12} {:<12} {:<10} {:<8} {}",
                s.id,
                s.version.as_deref().unwrap_or("-"),
                s.state.as_deref().unwrap_or("-"),
                s.source.as_deref().unwrap_or("-"),
                s.estimated_tokens.map(|t| t.to_string()).unwrap_or_else(|| "-".into()),
                s.error.as_deref().unwrap_or(""),
            );
        }
    } else {
        println!("{:<35} {:<12} {}", "SKILL", "VERSION", "STATE");
        println!("{}", "-".repeat(60));
        for s in &data.skills {
            println!(
                "{:<35} {:<12} {}",
                s.id,
                s.version.as_deref().unwrap_or("-"),
                s.state.as_deref().unwrap_or("-"),
            );
        }
    }
    println!("\n{} skill(s) total.", data.total);
    Ok(())
}

/// Show detailed info about a specific skill.
async fn cmd_skill_info(
    base_url: &str,
    name: &str,
    json_out: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{}/api/v1/skills/rpc", base_url);
    let body = serde_json::json!({
        "method": "info",
        "params": { "name": name },
    });

    let client = reqwest::Client::new();
    let resp = client.post(&url).json(&body).send().await
        .map_err(|e| format!("failed to connect to gateway: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        eprintln!("Error {status}: {text}");
        return Ok(());
    }

    let data: serde_json::Value = resp.json().await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&data)?);
    } else {
        let id = data.get("id").and_then(|v| v.as_str()).unwrap_or(name);
        let version = data.get("version").and_then(|v| v.as_str()).unwrap_or("-");
        let trust = data.get("trust_level").and_then(|v| v.as_str()).unwrap_or("unknown");
        let content_addr = data.get("content_address").and_then(|v| v.as_str()).unwrap_or("-");
        let source = data.get("source").and_then(|v| v.as_str()).unwrap_or("-");
        let tokens = data.get("estimated_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let deps = data.get("dependencies")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_else(|| "none".into());

        let trust_icon = match trust {
            "builtin" | "signed(trusted)" => "✓",
            "unsigned" => "⚠",
            _ => "?",
        };

        println!();
        println!("  {} v{}", id, version);
        println!("  Trust: {} {} ", trust_icon, trust);
        println!("  Content: {}", content_addr);
        println!("  Source: {}", source);
        println!("  Deps: {}", deps);
        println!("  Tokens: ~{} estimated", tokens);
        println!();
    }
    Ok(())
}

/// Search the store catalog.
async fn cmd_skill_search(
    base_url: &str,
    query: &str,
    category: Option<String>,
    verified: bool,
    json_out: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{}/api/v1/skills/rpc", base_url);
    let body = serde_json::json!({
        "method": "search",
        "params": {
            "query": query,
            "category": category,
            "verified_only": verified,
            "limit": 25,
        },
    });

    let client = reqwest::Client::new();
    let resp = client.post(&url).json(&body).send().await
        .map_err(|e| format!("failed to connect to gateway: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        eprintln!("Error {status}: {text}");
        return Ok(());
    }

    let data: StoreSearchResponse = resp.json().await?;

    if json_out {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!(data.entries))?);
        return Ok(());
    }

    if data.entries.is_empty() {
        println!("No skills found matching '{}'.", query);
        return Ok(());
    }

    println!(
        "{:<30} {:<12} {:<10} {:<6} {:<10} {}",
        "SKILL", "VERSION", "AUTHOR", "STARS", "INSTALLS", "STATUS"
    );
    println!("{}", "-".repeat(85));
    for e in &data.entries {
        let trust_mark = if e.verified { "✓" } else { " " };
        println!(
            "{}{:<29} {:<12} {:<10} {:<6} {:<10} {}",
            trust_mark,
            e.display_name,
            e.version,
            e.author,
            format!("{:.1}", e.rating),
            e.install_count,
            e.install_state,
        );
    }
    println!("\n{} result(s) (of {} total).", data.entries.len(), data.total_count);
    Ok(())
}

/// Install a skill via gateway RPC.
async fn cmd_skill_install(
    base_url: &str,
    skill_ref: &str,
    force: bool,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if dry_run {
        println!("Dry-run: would install '{}'", skill_ref);
        // In dry-run mode, just resolve and show the plan without executing
        let url = format!("{}/api/v1/skills/rpc", base_url);
        let body = serde_json::json!({
            "method": "install",
            "params": {
                "ref": skill_ref,
                "force": force,
                "dry_run": true,
            },
        });
        let client = reqwest::Client::new();
        let resp = client.post(&url).json(&body).send().await
            .map_err(|e| format!("failed to connect to gateway: {e}"))?;

        let data: serde_json::Value = resp.json().await?;
        if let Some(plan) = data.get("plan") {
            println!("{}", serde_json::to_string_pretty(plan)?);
        }
        return Ok(());
    }

    println!("Installing '{}'…", skill_ref);
    let url = format!("{}/api/v1/skills/rpc", base_url);
    let body = serde_json::json!({
        "method": "install",
        "params": {
            "ref": skill_ref,
            "force": force,
            "dry_run": false,
        },
    });

    let client = reqwest::Client::new();
    let resp = client.post(&url).json(&body).send().await
        .map_err(|e| format!("failed to connect to gateway: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        eprintln!("Install failed ({status}): {text}");
        return Ok(());
    }

    let result: SkillOpResponse = resp.json().await?;
    match result.status.as_deref() {
        Some("installed") => {
            println!("✓ Installed '{}'", result.id.as_deref().unwrap_or(skill_ref));
        }
        Some("already_installed") => {
            println!("Already installed. Use --force to reinstall.");
        }
        _ => {
            if let Some(err) = &result.error {
                eprintln!("Install error: {err}");
            }
        }
    }
    Ok(())
}

/// Uninstall a skill via gateway RPC.
async fn cmd_skill_uninstall(
    base_url: &str,
    id: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{}/api/v1/skills/rpc", base_url);
    let body = serde_json::json!({
        "method": "uninstall",
        "params": { "id": id },
    });

    let client = reqwest::Client::new();
    let resp = client.post(&url).json(&body).send().await
        .map_err(|e| format!("failed to connect to gateway: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        eprintln!("Uninstall failed ({status}): {text}");
        return Ok(());
    }

    let result: SkillOpResponse = resp.json().await?;
    match result.status.as_deref() {
        Some("uninstalled") => println!("✓ Uninstalled '{}'", id),
        _ => {
            if let Some(err) = &result.error {
                eprintln!("Uninstall error: {err}");
            }
        }
    }
    Ok(())
}

/// Update skills via gateway RPC.
async fn cmd_skill_update(
    base_url: &str,
    all: bool,
    id: Option<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if !all && id.is_none() {
        eprintln!("Specify --all or a skill ID to update.");
        return Ok(());
    }

    let url = format!("{}/api/v1/skills/rpc", base_url);
    let body = serde_json::json!({
        "method": "update",
        "params": {
            "all": all,
            "id": id,
        },
    });

    let client = reqwest::Client::new();
    let resp = client.post(&url).json(&body).send().await
        .map_err(|e| format!("failed to connect to gateway: {e}"))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        eprintln!("Update failed ({status}): {text}");
        return Ok(());
    }

    let data: serde_json::Value = resp.json().await?;
    let updated = data.get("updated").and_then(|v| v.as_u64()).unwrap_or(0);
    let skipped = data.get("skipped").and_then(|v| v.as_u64()).unwrap_or(0);
    println!("✓ Updated {} skill(s), {} already up to date.", updated, skipped);
    Ok(())
}

// ── Local commands (no gateway needed) ───────────────────────

/// Create a new skill scaffold (delegates to skill_author).
async fn cmd_skill_create(
    id: &str,
    name: Option<&str>,
    description: Option<&str>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use crate::skill_author;
    let base_dir = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let path = skill_author::cmd_skill_create(
        id,
        name,
        description,
        vec![],
        vec![],
        None,
        &base_dir,
    ).map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    println!("✓ Created skill scaffold at '{}'", path.display());
    Ok(())
}

/// Lint skills in a directory (delegates to skill_author).
async fn cmd_skill_lint(
    dir: Option<&str>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use crate::skill_author;
    let skills_dir = dir
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| ".".into()));
    let report = skill_author::cmd_skill_lint(&skills_dir)
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    println!(
        "Checked {} skill(s): {} error(s), {} warning(s).",
        report.skills_checked, report.errors, report.warnings
    );
    for diag in &report.diagnostics {
        println!("  [{}] {}: {}", diag.severity, diag.skill_id, diag.message);
    }
    Ok(())
}

/// Test a skill with sample input (delegates to skill_author).
async fn cmd_skill_test(
    dir: &str,
    input: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use crate::skill_author;
    let path = std::path::PathBuf::from(dir);
    let report = skill_author::cmd_skill_test(&path, input)
        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
    println!("Trigger score: {:.2}", report.trigger_score);
    println!("Estimated tokens: {}", report.estimated_tokens);
    println!("Bound tools: {}", report.bound_tools.join(", "));
    if !report.warnings.is_empty() {
        println!("Warnings:");
        for w in &report.warnings {
            println!("  ⚠ {}", w);
        }
    }
    println!("\n--- Prompt Preview ---\n{}", report.prompt_preview);
    Ok(())
}

/// Audit installed skills (integrity, trust chain, freshness).
async fn cmd_skill_audit(
    base_url: &str,
    json_out: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{}/api/v1/skills/rpc", base_url);
    let body = serde_json::json!({
        "method": "audit",
        "params": {},
    });

    let client = reqwest::Client::new();
    let resp = client.post(&url).json(&body).send().await
        .map_err(|e| format!("failed to connect to gateway: {e}"))?;

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        eprintln!("Audit failed: {text}");
        return Ok(());
    }

    let data: serde_json::Value = resp.json().await?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&data)?);
    } else {
        let total = data.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
        let verified = data.get("verified").and_then(|v| v.as_u64()).unwrap_or(0);
        let warnings = data.get("warnings").and_then(|v| v.as_u64()).unwrap_or(0);
        let merkle = data.get("merkle_root").and_then(|v| v.as_str()).unwrap_or("-");

        println!("Skill Audit Report");
        println!("==================");
        println!("Total skills:  {}", total);
        println!("Verified:      {}", verified);
        println!("Warnings:      {}", warnings);
        println!("Merkle root:   {}", merkle);

        if let Some(issues) = data.get("issues").and_then(|v| v.as_array()) {
            if !issues.is_empty() {
                println!("\nIssues:");
                for issue in issues {
                    let id = issue.get("skill_id").and_then(|v| v.as_str()).unwrap_or("?");
                    let msg = issue.get("message").and_then(|v| v.as_str()).unwrap_or("?");
                    println!("  ⚠ {}: {}", id, msg);
                }
            }
        }
    }
    Ok(())
}

/// Check skill eligibility against the current environment.
async fn cmd_skill_check(
    base_url: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("{}/api/v1/skills/rpc", base_url);
    let body = serde_json::json!({
        "method": "check",
        "params": {},
    });

    let client = reqwest::Client::new();
    let resp = client.post(&url).json(&body).send().await
        .map_err(|e| format!("failed to connect to gateway: {e}"))?;

    if !resp.status().is_success() {
        let text = resp.text().await.unwrap_or_default();
        eprintln!("Check failed: {text}");
        return Ok(());
    }

    let data: serde_json::Value = resp.json().await?;
    if let Some(skills) = data.get("skills").and_then(|v| v.as_array()) {
        println!("{:<30} {:<10} {}", "SKILL", "STATUS", "MISSING");
        println!("{}", "-".repeat(65));
        for s in skills {
            let id = s.get("id").and_then(|v| v.as_str()).unwrap_or("?");
            let eligible = s.get("eligible").and_then(|v| v.as_bool()).unwrap_or(false);
            let missing = s.get("missing")
                .and_then(|v| v.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let remote = s.get("remote").and_then(|v| v.as_str());

            let status = if eligible {
                if remote.is_some() {
                    format!("✓ Remote  ({})", remote.unwrap())
                } else {
                    "✓ Ready".to_string()
                }
            } else {
                "✗ Missing".to_string()
            };

            println!(
                "{:<30} {:<10} {}",
                id,
                status,
                if missing.is_empty() { "—".into() } else { missing },
            );
        }
    }
    Ok(())
}

/// Publish a skill package to the store.
async fn cmd_skill_publish(
    base_url: &str,
    dir: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // Build the package locally
    let path = std::path::PathBuf::from(dir);
    if !path.join("skill.toml").exists() && !path.join("SKILL.md").exists() {
        eprintln!("No skill.toml or SKILL.md found in '{}'.", dir);
        return Ok(());
    }

    println!("Packaging skill from '{}'…", dir);

    // Compute content hash for the directory
    use sha2::{Sha256, Digest};
    let mut hasher = Sha256::new();
    let mut file_count = 0usize;
    if let Ok(entries) = std::fs::read_dir(&path) {
        for entry in entries.flatten() {
            if let Ok(content) = std::fs::read(entry.path()) {
                hasher.update(&content);
                file_count += 1;
            }
        }
    }
    let hash = hex::encode(hasher.finalize());

    println!("  Files: {}", file_count);
    println!("  SHA-256: {}", &hash[..16]);

    // Upload to gateway
    let url = format!("{}/api/v1/skills/rpc", base_url);
    let body = serde_json::json!({
        "method": "publish",
        "params": {
            "dir": dir,
            "checksum": hash,
        },
    });

    let client = reqwest::Client::new();
    let resp = client.post(&url).json(&body).send().await
        .map_err(|e| format!("failed to connect to gateway: {e}"))?;

    if resp.status().is_success() {
        println!("✓ Skill published successfully.");
    } else {
        let text = resp.text().await.unwrap_or_default();
        eprintln!("Publish failed: {text}");
    }
    Ok(())
}
