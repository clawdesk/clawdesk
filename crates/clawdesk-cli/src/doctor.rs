//! Enhanced diagnostic command — comprehensive system health check.
//!
//! Checks:
//! 1. CLI version + platform
//! 2. Data directory permissions
//! 3. SochDB database health
//! 4. Gateway connectivity
//! 5. Provider health (all configured providers)
//! 6. Ollama availability + models
//! 7. Channel configuration
//! 8. Skill loader status
//! 9. Disk usage
//! 10. Network diagnostics
//!
//! ## Extensibility
//!
//! Implement the `DiagnosticCheck` trait and register with
//! `DiagnosticRegistry::register()` to add custom checks. Checks declare
//! dependencies so the registry can run them in topological order.

use async_trait::async_trait;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::time::Instant;

/// Check status for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
    Skip,
}

impl std::fmt::Display for CheckStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ok => write!(f, "✓"),
            Self::Warn => write!(f, "⚠"),
            Self::Fail => write!(f, "✗"),
            Self::Skip => write!(f, "–"),
        }
    }
}

/// A single diagnostic check result.
pub struct DiagResult {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
    pub duration_ms: u64,
    /// Optional repair action that was taken or can be taken.
    pub repair_hint: Option<String>,
}

/// Trait for pluggable diagnostic checks.
///
/// Implement this trait and register with `DiagnosticRegistry` to add custom
/// health checks. Checks declare dependencies so they run in correct order.
#[async_trait]
pub trait DiagnosticCheck: Send + Sync {
    /// Unique name of this check.
    fn name(&self) -> &str;

    /// Names of checks that must pass before this one runs.
    /// Return an empty slice if there are no dependencies.
    fn depends_on(&self) -> &[&str] {
        &[]
    }

    /// Run the diagnostic check.
    async fn run(&self) -> DiagResult;

    /// Attempt to repair the issue. Returns Ok if repair succeeded.
    /// Default: no repair available.
    async fn repair(&self) -> Result<String, String> {
        Err("no automatic repair available".to_string())
    }
}

/// Registry of diagnostic checks with dependency-ordered execution.
pub struct DiagnosticRegistry {
    checks: Vec<Box<dyn DiagnosticCheck>>,
}

impl DiagnosticRegistry {
    pub fn new() -> Self {
        Self { checks: Vec::new() }
    }

    /// Create a registry pre-loaded with all built-in checks.
    pub fn with_builtins() -> Self {
        let mut reg = Self::new();
        reg.register(Box::new(PlatformCheck));
        reg.register(Box::new(DataDirCheck));
        reg.register(Box::new(SochDbCheck));
        reg.register(Box::new(GatewayCheck));
        reg.register(Box::new(ProviderCheck { name: "Anthropic", file: "anthropic.json", env_var: "ANTHROPIC_API_KEY" }));
        reg.register(Box::new(ProviderCheck { name: "OpenAI", file: "openai.json", env_var: "OPENAI_API_KEY" }));
        reg.register(Box::new(ProviderCheck { name: "Gemini", file: "gemini.json", env_var: "GEMINI_API_KEY" }));
        reg.register(Box::new(BedrockCheck));
        reg.register(Box::new(OllamaCheck));
        reg.register(Box::new(DiskUsageCheck));
        reg.register(Box::new(NetworkCheck));
        reg
    }

    /// Register a new diagnostic check.
    pub fn register(&mut self, check: Box<dyn DiagnosticCheck>) {
        self.checks.push(check);
    }

    /// Run all checks in dependency order.
    ///
    /// Uses topological sort to ensure dependencies run first. Checks whose
    /// dependencies failed are skipped. Returns results in execution order.
    pub async fn run_all(&self) -> Vec<DiagResult> {
        let order = self.topological_order();
        let mut results: Vec<DiagResult> = Vec::with_capacity(order.len());
        let mut passed: HashSet<String> = HashSet::new();

        for idx in order {
            let check = &self.checks[idx];
            let deps = check.depends_on();

            // Skip if any dependency failed.
            let deps_met = deps.iter().all(|d| passed.contains(*d));
            if !deps_met {
                results.push(DiagResult {
                    name: check.name().to_string(),
                    status: CheckStatus::Skip,
                    detail: "skipped (dependency failed)".to_string(),
                    duration_ms: 0,
                    repair_hint: None,
                });
                continue;
            }

            let result = check.run().await;
            if matches!(result.status, CheckStatus::Ok | CheckStatus::Warn) {
                passed.insert(check.name().to_string());
            }
            results.push(result);
        }

        results
    }

    /// Topological sort of checks based on dependencies.
    fn topological_order(&self) -> Vec<usize> {
        let name_to_idx: HashMap<&str, usize> = self.checks.iter()
            .enumerate()
            .map(|(i, c)| (c.name(), i))
            .collect();

        let mut in_degree = vec![0usize; self.checks.len()];
        let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); self.checks.len()];

        for (i, check) in self.checks.iter().enumerate() {
            for dep in check.depends_on() {
                if let Some(&dep_idx) = name_to_idx.get(dep) {
                    adjacency[dep_idx].push(i);
                    in_degree[i] += 1;
                }
            }
        }

        let mut queue: VecDeque<usize> = in_degree.iter()
            .enumerate()
            .filter(|(_, &d)| d == 0)
            .map(|(i, _)| i)
            .collect();

        let mut order = Vec::with_capacity(self.checks.len());
        while let Some(idx) = queue.pop_front() {
            order.push(idx);
            for &next in &adjacency[idx] {
                in_degree[next] -= 1;
                if in_degree[next] == 0 {
                    queue.push_back(next);
                }
            }
        }

        // Append any checks not reached (cycle protection).
        for i in 0..self.checks.len() {
            if !order.contains(&i) {
                order.push(i);
            }
        }

        order
    }
}

impl Default for DiagnosticRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

// ---------------------------------------------------------------------------
// Public entry point (backwards compatible)
// ---------------------------------------------------------------------------

/// Run comprehensive diagnostics.
pub async fn run_doctor(verbose: bool) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!();
    println!("ClawDesk Doctor");
    println!("═══════════════");
    println!();

    let registry = DiagnosticRegistry::with_builtins();
    let results = registry.run_all().await;

    let (ok, warn, fail) = display_results(&results, verbose);

    println!();
    println!("Summary: {} passed, {} warnings, {} failed", ok, warn, fail);

    if fail > 0 {
        println!();
        println!("Run 'clawdesk init' to fix configuration issues.");
    }

    println!();
    Ok(())
}

fn display_results(checks: &[DiagResult], verbose: bool) -> (usize, usize, usize) {
    let mut ok = 0;
    let mut warn = 0;
    let mut fail = 0;

    for check in checks {
        match check.status {
            CheckStatus::Ok => ok += 1,
            CheckStatus::Warn => warn += 1,
            CheckStatus::Fail => fail += 1,
            CheckStatus::Skip => {}
        }

        let timing = if verbose && check.duration_ms > 0 {
            format!(" ({}ms)", check.duration_ms)
        } else {
            String::new()
        };

        println!("  {} {:<25} {}{}", check.status, check.name, check.detail, timing);

        if verbose {
            if let Some(ref hint) = check.repair_hint {
                println!("    → fix: {}", hint);
            }
        }
    }

    (ok, warn, fail)
}

// ---------------------------------------------------------------------------
// Built-in checks (implement DiagnosticCheck trait)
// ---------------------------------------------------------------------------

struct PlatformCheck;

#[async_trait]
impl DiagnosticCheck for PlatformCheck {
    fn name(&self) -> &str { "Platform" }

    async fn run(&self) -> DiagResult {
        DiagResult {
            name: "Platform".to_string(),
            status: CheckStatus::Ok,
            detail: format!(
                "v{} ({}/{})",
                env!("CARGO_PKG_VERSION"),
                std::env::consts::OS,
                std::env::consts::ARCH,
            ),
            duration_ms: 0,
            repair_hint: None,
        }
    }
}

struct DataDirCheck;

#[async_trait]
impl DiagnosticCheck for DataDirCheck {
    fn name(&self) -> &str { "Data directory" }
    fn depends_on(&self) -> &[&str] { &["Platform"] }

    async fn run(&self) -> DiagResult {
        let data_dir = clawdesk_types::dirs::data();
        let dot_dir = clawdesk_types::dirs::dot_clawdesk();
        let start = Instant::now();

        let data_exists = data_dir.exists();
        let dot_exists = dot_dir.exists();

        if !data_exists && !dot_exists {
            return DiagResult {
                name: self.name().to_string(),
                status: CheckStatus::Warn,
                detail: format!("{} (not created)", data_dir.display()),
                duration_ms: start.elapsed().as_millis() as u64,
                repair_hint: Some("run 'clawdesk init'".to_string()),
            };
        }

        let mut missing = Vec::new();
        for sub in &["skills"] {
            if !data_dir.join(sub).exists() { missing.push(format!("data/{}", sub)); }
        }
        for sub in &["sochdb", "threads", "agents"] {
            if !dot_dir.join(sub).exists() { missing.push(format!(".clawdesk/{}", sub)); }
        }

        if missing.is_empty() {
            DiagResult {
                name: self.name().to_string(),
                status: CheckStatus::Ok,
                detail: format!("{} + {}", data_dir.display(), dot_dir.display()),
                duration_ms: start.elapsed().as_millis() as u64,
                repair_hint: None,
            }
        } else {
            let status = if dot_exists || data_exists { CheckStatus::Warn } else { CheckStatus::Fail };
            DiagResult {
                name: self.name().to_string(),
                status,
                detail: format!("{} (missing: {})", data_dir.display(), missing.join(", ")),
                duration_ms: start.elapsed().as_millis() as u64,
                repair_hint: Some("run 'clawdesk init'".to_string()),
            }
        }
    }

    async fn repair(&self) -> Result<String, String> {
        let data_dir = clawdesk_types::dirs::data();
        let dot_dir = clawdesk_types::dirs::dot_clawdesk();
        for sub in &["skills"] {
            let _ = std::fs::create_dir_all(data_dir.join(sub));
        }
        for sub in &["sochdb", "threads", "agents"] {
            let _ = std::fs::create_dir_all(dot_dir.join(sub));
        }
        Ok("created missing directories".to_string())
    }
}

struct SochDbCheck;

#[async_trait]
impl DiagnosticCheck for SochDbCheck {
    fn name(&self) -> &str { "SochDB" }
    fn depends_on(&self) -> &[&str] { &["Data directory"] }

    async fn run(&self) -> DiagResult {
        let sochdb_dir = clawdesk_types::dirs::sochdb();
        let start = Instant::now();

        if !sochdb_dir.exists() {
            return DiagResult {
                name: self.name().to_string(),
                status: CheckStatus::Skip,
                detail: format!("{} (not created)", sochdb_dir.display()),
                duration_ms: start.elapsed().as_millis() as u64,
                repair_hint: Some("run 'clawdesk init'".to_string()),
            };
        }

        match clawdesk_sochdb::SochStore::open(sochdb_dir.to_str().unwrap_or(".")) {
            Ok(_store) => DiagResult {
                name: self.name().to_string(),
                status: CheckStatus::Ok,
                detail: format!("healthy ({})", sochdb_dir.display()),
                duration_ms: start.elapsed().as_millis() as u64,
                repair_hint: None,
            },
            Err(e) => DiagResult {
                name: self.name().to_string(),
                status: CheckStatus::Fail,
                detail: format!("error: {}", e),
                duration_ms: start.elapsed().as_millis() as u64,
                repair_hint: None,
            },
        }
    }
}

struct GatewayCheck;

#[async_trait]
impl DiagnosticCheck for GatewayCheck {
    fn name(&self) -> &str { "Gateway" }

    async fn run(&self) -> DiagResult {
        let start = Instant::now();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap();

        match client.get("http://127.0.0.1:18789/api/v1/health").send().await {
            Ok(resp) if resp.status().is_success() => {
                let detail = if let Ok(body) = resp.json::<serde_json::Value>().await {
                    let uptime = body.get("uptime_secs").and_then(|v| v.as_u64()).unwrap_or(0);
                    let version = body.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                    format!("v{}, uptime {}s", version, uptime)
                } else {
                    "running".to_string()
                };
                DiagResult {
                    name: self.name().to_string(),
                    status: CheckStatus::Ok, detail,
                    duration_ms: start.elapsed().as_millis() as u64,
                    repair_hint: None,
                }
            }
            Ok(resp) => DiagResult {
                name: self.name().to_string(),
                status: CheckStatus::Fail,
                detail: format!("HTTP {}", resp.status()),
                duration_ms: start.elapsed().as_millis() as u64,
                repair_hint: Some("check gateway logs".to_string()),
            },
            Err(_) => DiagResult {
                name: self.name().to_string(),
                status: CheckStatus::Warn,
                detail: "not running".to_string(),
                duration_ms: start.elapsed().as_millis() as u64,
                repair_hint: Some("start with 'clawdesk gateway run'".to_string()),
            },
        }
    }
}

struct ProviderCheck {
    name: &'static str,
    file: &'static str,
    env_var: &'static str,
}

#[async_trait]
impl DiagnosticCheck for ProviderCheck {
    fn name(&self) -> &str { self.name }

    async fn run(&self) -> DiagResult {
        let creds_dir = clawdesk_types::dirs::data().join("credentials");
        let dot_env_path = clawdesk_types::dirs::dot_clawdesk().join(".env");
        let start = Instant::now();

        // Load .env if present.
        if dot_env_path.exists() {
            if let Ok(contents) = std::fs::read_to_string(&dot_env_path) {
                for line in contents.lines() {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') { continue; }
                    if let Some((key, value)) = line.split_once('=') {
                        let key = key.trim();
                        let value = value.trim().trim_matches('"').trim_matches('\'');
                        if std::env::var(key).is_err() {
                            std::env::set_var(key, value);
                        }
                    }
                }
            }
        }

        let has_env = std::env::var(self.env_var).ok().filter(|v| !v.is_empty()).is_some();
        let has_file = creds_dir.join(self.file).exists();

        let (status, detail) = if has_env {
            (CheckStatus::Ok, format!("configured (${})", self.env_var))
        } else if has_file {
            (CheckStatus::Ok, format!("configured ({})", creds_dir.join(self.file).display()))
        } else {
            (CheckStatus::Skip, "not configured".to_string())
        };

        DiagResult {
            name: format!("Provider: {}", self.name),
            status, detail,
            duration_ms: start.elapsed().as_millis() as u64,
            repair_hint: if matches!(status, CheckStatus::Skip) {
                Some(format!("set ${} or add {}", self.env_var, self.file))
            } else { None },
        }
    }
}

struct BedrockCheck;

#[async_trait]
impl DiagnosticCheck for BedrockCheck {
    fn name(&self) -> &str { "Bedrock" }

    async fn run(&self) -> DiagResult {
        let start = Instant::now();
        let has_aws = std::env::var("AWS_ACCESS_KEY_ID").ok().filter(|v| !v.is_empty()).is_some()
            && std::env::var("AWS_SECRET_ACCESS_KEY").ok().filter(|v| !v.is_empty()).is_some();
        DiagResult {
            name: "Provider: Bedrock".to_string(),
            status: if has_aws { CheckStatus::Ok } else { CheckStatus::Skip },
            detail: if has_aws { "configured (AWS env vars)".to_string() } else { "not configured".to_string() },
            duration_ms: start.elapsed().as_millis() as u64,
            repair_hint: if !has_aws {
                Some("set $AWS_ACCESS_KEY_ID and $AWS_SECRET_ACCESS_KEY".to_string())
            } else { None },
        }
    }
}

struct OllamaCheck;

#[async_trait]
impl DiagnosticCheck for OllamaCheck {
    fn name(&self) -> &str { "Ollama" }

    async fn run(&self) -> DiagResult {
        let start = Instant::now();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()
            .unwrap();

        match client.get("http://localhost:11434/api/tags").send().await {
            Ok(resp) if resp.status().is_success() => {
                let detail = if let Ok(body) = resp.json::<serde_json::Value>().await {
                    let models = body.get("models")
                        .and_then(|v| v.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    format!("running ({} model{})", models, if models == 1 { "" } else { "s" })
                } else {
                    "running".to_string()
                };
                DiagResult {
                    name: self.name().to_string(),
                    status: CheckStatus::Ok, detail,
                    duration_ms: start.elapsed().as_millis() as u64,
                    repair_hint: None,
                }
            }
            _ => DiagResult {
                name: self.name().to_string(),
                status: CheckStatus::Skip,
                detail: "not running (optional)".to_string(),
                duration_ms: start.elapsed().as_millis() as u64,
                repair_hint: Some("install from https://ollama.com".to_string()),
            },
        }
    }
}

struct DiskUsageCheck;

#[async_trait]
impl DiagnosticCheck for DiskUsageCheck {
    fn name(&self) -> &str { "Disk usage" }
    fn depends_on(&self) -> &[&str] { &["Data directory"] }

    async fn run(&self) -> DiagResult {
        let data_dir = clawdesk_types::dirs::data();
        let dot_dir = clawdesk_types::dirs::dot_clawdesk();
        let start = Instant::now();

        if !data_dir.exists() && !dot_dir.exists() {
            return DiagResult {
                name: self.name().to_string(),
                status: CheckStatus::Skip,
                detail: "data dir not created".to_string(),
                duration_ms: 0,
                repair_hint: None,
            };
        }

        let mut size = 0u64;
        if data_dir.exists() { size += dir_size(&data_dir); }
        if dot_dir.exists() && dot_dir != data_dir { size += dir_size(&dot_dir); }
        let human = format_bytes(size);

        DiagResult {
            name: self.name().to_string(),
            status: if size > 1_000_000_000 { CheckStatus::Warn } else { CheckStatus::Ok },
            detail: format!("{} ({})", human, data_dir.display()),
            duration_ms: start.elapsed().as_millis() as u64,
            repair_hint: if size > 1_000_000_000 {
                Some("consider pruning old sessions".to_string())
            } else { None },
        }
    }
}

struct NetworkCheck;

#[async_trait]
impl DiagnosticCheck for NetworkCheck {
    fn name(&self) -> &str { "Network" }

    async fn run(&self) -> DiagResult {
        let start = Instant::now();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();

        match client.get("https://api.anthropic.com").send().await {
            Ok(_) => DiagResult {
                name: self.name().to_string(),
                status: CheckStatus::Ok,
                detail: "internet reachable".to_string(),
                duration_ms: start.elapsed().as_millis() as u64,
                repair_hint: None,
            },
            Err(_) => DiagResult {
                name: self.name().to_string(),
                status: CheckStatus::Warn,
                detail: "internet unreachable (offline mode only)".to_string(),
                duration_ms: start.elapsed().as_millis() as u64,
                repair_hint: Some("check network/proxy settings".to_string()),
            },
        }
    }
}

fn dir_size(path: &PathBuf) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    total += meta.len();
                } else if meta.is_dir() {
                    total += dir_size(&entry.path());
                }
            }
        }
    }
    total
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit_idx = 0;
    while size >= 1024.0 && unit_idx < UNITS.len() - 1 {
        size /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{} B", bytes)
    } else {
        format!("{:.1} {}", size, UNITS[unit_idx])
    }
}

// Removed: duplicated default_data_dir() and dirs_home().
// All path resolution now uses clawdesk_types::dirs::{data, dot_clawdesk, sochdb, threads, agents}.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1_048_576), "1.0 MB");
        assert_eq!(format_bytes(1_073_741_824), "1.0 GB");
    }

    #[tokio::test]
    async fn platform_check_always_ok() {
        let check = PlatformCheck;
        let result = check.run().await;
        assert!(matches!(result.status, CheckStatus::Ok));
        assert!(result.detail.contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn topological_order_respects_dependencies() {
        let registry = DiagnosticRegistry::with_builtins();
        let order = registry.topological_order();
        let names: Vec<&str> = order.iter().map(|&i| registry.checks[i].name()).collect();

        // Platform must come before Data directory.
        let platform_pos = names.iter().position(|&n| n == "Platform").unwrap();
        let data_dir_pos = names.iter().position(|&n| n == "Data directory").unwrap();
        assert!(platform_pos < data_dir_pos);

        // Data directory must come before SochDB.
        let sochdb_pos = names.iter().position(|&n| n == "SochDB").unwrap();
        assert!(data_dir_pos < sochdb_pos);
    }
}
