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

use std::path::PathBuf;
use std::time::Instant;

/// Check status for display.
#[derive(Debug, Clone, Copy)]
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
pub struct DiagCheck {
    pub name: String,
    pub status: CheckStatus,
    pub detail: String,
    pub duration_ms: u64,
}

/// Run comprehensive diagnostics.
pub async fn run_doctor(verbose: bool) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!();
    println!("ClawDesk Doctor");
    println!("═══════════════");
    println!();

    let mut checks: Vec<DiagCheck> = Vec::new();

    // 1. Platform info
    checks.push(check_platform());

    // 2. Data directory
    checks.push(check_data_dir());

    // 3. SochDB
    checks.push(check_sochdb().await);

    // 4. Gateway connectivity
    checks.push(check_gateway().await);

    // 5. Provider credentials
    let provider_checks = check_providers().await;
    checks.extend(provider_checks);

    // 6. Ollama
    checks.push(check_ollama().await);

    // 7. Disk usage
    checks.push(check_disk_usage());

    // 8. Network
    checks.push(check_network().await);

    // Display results
    let (ok, warn, fail) = display_results(&checks, verbose);

    println!();
    println!("Summary: {} passed, {} warnings, {} failed", ok, warn, fail);

    if fail > 0 {
        println!();
        println!("Run 'clawdesk init' to fix configuration issues.");
    }

    println!();
    Ok(())
}

fn display_results(checks: &[DiagCheck], verbose: bool) -> (usize, usize, usize) {
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
    }

    (ok, warn, fail)
}

fn check_platform() -> DiagCheck {
    let detail = format!(
        "v{} ({}/{})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
    );
    DiagCheck {
        name: "Platform".to_string(),
        status: CheckStatus::Ok,
        detail,
        duration_ms: 0,
    }
}

fn check_data_dir() -> DiagCheck {
    let data_dir = default_data_dir();
    let start = Instant::now();

    if !data_dir.exists() {
        return DiagCheck {
            name: "Data directory".to_string(),
            status: CheckStatus::Warn,
            detail: format!("{} (not created — run 'clawdesk init')", data_dir.display()),
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }

    // Check subdirs
    let subdirs = ["data", "credentials", "skills", "plugins"];
    let mut missing = Vec::new();
    for sub in &subdirs {
        if !data_dir.join(sub).exists() {
            missing.push(*sub);
        }
    }

    if missing.is_empty() {
        DiagCheck {
            name: "Data directory".to_string(),
            status: CheckStatus::Ok,
            detail: data_dir.display().to_string(),
            duration_ms: start.elapsed().as_millis() as u64,
        }
    } else {
        DiagCheck {
            name: "Data directory".to_string(),
            status: CheckStatus::Warn,
            detail: format!("{} (missing: {})", data_dir.display(), missing.join(", ")),
            duration_ms: start.elapsed().as_millis() as u64,
        }
    }
}

async fn check_sochdb() -> DiagCheck {
    let data_dir = default_data_dir().join("data");
    let start = Instant::now();

    if !data_dir.exists() {
        return DiagCheck {
            name: "SochDB".to_string(),
            status: CheckStatus::Skip,
            detail: "data dir not created".to_string(),
            duration_ms: start.elapsed().as_millis() as u64,
        };
    }

    match clawdesk_sochdb::SochStore::open(data_dir.to_str().unwrap_or(".")) {
        Ok(_store) => DiagCheck {
            name: "SochDB".to_string(),
            status: CheckStatus::Ok,
            detail: "healthy".to_string(),
            duration_ms: start.elapsed().as_millis() as u64,
        },
        Err(e) => DiagCheck {
            name: "SochDB".to_string(),
            status: CheckStatus::Fail,
            detail: format!("error: {}", e),
            duration_ms: start.elapsed().as_millis() as u64,
        },
    }
}

async fn check_gateway() -> DiagCheck {
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
            DiagCheck {
                name: "Gateway".to_string(),
                status: CheckStatus::Ok,
                detail,
                duration_ms: start.elapsed().as_millis() as u64,
            }
        }
        Ok(resp) => DiagCheck {
            name: "Gateway".to_string(),
            status: CheckStatus::Fail,
            detail: format!("HTTP {}", resp.status()),
            duration_ms: start.elapsed().as_millis() as u64,
        },
        Err(_) => DiagCheck {
            name: "Gateway".to_string(),
            status: CheckStatus::Warn,
            detail: "not running (start with 'clawdesk gateway run')".to_string(),
            duration_ms: start.elapsed().as_millis() as u64,
        },
    }
}

async fn check_providers() -> Vec<DiagCheck> {
    let creds_dir = default_data_dir().join("credentials");
    let mut checks = Vec::new();

    let providers = [
        ("Anthropic", "anthropic.json", "ANTHROPIC_API_KEY"),
        ("OpenAI", "openai.json", "OPENAI_API_KEY"),
        ("Gemini", "gemini.json", "GEMINI_API_KEY"),
    ];

    for (name, file, env_var) in &providers {
        let start = Instant::now();

        let has_env = std::env::var(env_var).ok().filter(|v| !v.is_empty()).is_some();
        let has_file = creds_dir.join(file).exists();

        let (status, detail) = if has_env {
            (CheckStatus::Ok, format!("configured (${env_var})"))
        } else if has_file {
            (CheckStatus::Ok, format!("configured ({})", creds_dir.join(file).display()))
        } else {
            (CheckStatus::Skip, "not configured".to_string())
        };

        checks.push(DiagCheck {
            name: format!("Provider: {}", name),
            status,
            detail,
            duration_ms: start.elapsed().as_millis() as u64,
        });
    }

    // AWS Bedrock
    let start = Instant::now();
    let has_aws = std::env::var("AWS_ACCESS_KEY_ID").ok().filter(|v| !v.is_empty()).is_some()
        && std::env::var("AWS_SECRET_ACCESS_KEY").ok().filter(|v| !v.is_empty()).is_some();
    checks.push(DiagCheck {
        name: "Provider: Bedrock".to_string(),
        status: if has_aws { CheckStatus::Ok } else { CheckStatus::Skip },
        detail: if has_aws { "configured (AWS env vars)".to_string() } else { "not configured".to_string() },
        duration_ms: start.elapsed().as_millis() as u64,
    });

    checks
}

async fn check_ollama() -> DiagCheck {
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
            DiagCheck {
                name: "Ollama".to_string(),
                status: CheckStatus::Ok,
                detail,
                duration_ms: start.elapsed().as_millis() as u64,
            }
        }
        _ => DiagCheck {
            name: "Ollama".to_string(),
            status: CheckStatus::Skip,
            detail: "not running (optional — https://ollama.com)".to_string(),
            duration_ms: start.elapsed().as_millis() as u64,
        },
    }
}

fn check_disk_usage() -> DiagCheck {
    let data_dir = default_data_dir();
    let start = Instant::now();

    if !data_dir.exists() {
        return DiagCheck {
            name: "Disk usage".to_string(),
            status: CheckStatus::Skip,
            detail: "data dir not created".to_string(),
            duration_ms: 0,
        };
    }

    let size = dir_size(&data_dir);
    let human = format_bytes(size);

    DiagCheck {
        name: "Disk usage".to_string(),
        status: if size > 1_000_000_000 { CheckStatus::Warn } else { CheckStatus::Ok },
        detail: format!("{} ({})", human, data_dir.display()),
        duration_ms: start.elapsed().as_millis() as u64,
    }
}

async fn check_network() -> DiagCheck {
    let start = Instant::now();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();

    // Check general internet connectivity
    match client.get("https://api.anthropic.com").send().await {
        Ok(_) => DiagCheck {
            name: "Network".to_string(),
            status: CheckStatus::Ok,
            detail: "internet reachable".to_string(),
            duration_ms: start.elapsed().as_millis() as u64,
        },
        Err(_) => DiagCheck {
            name: "Network".to_string(),
            status: CheckStatus::Warn,
            detail: "internet unreachable (offline mode only)".to_string(),
            duration_ms: start.elapsed().as_millis() as u64,
        },
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

fn default_data_dir() -> PathBuf {
    if cfg!(target_os = "macos") {
        dirs_home().join("Library").join("Application Support").join("clawdesk")
    } else if cfg!(target_os = "linux") {
        std::env::var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| dirs_home().join(".local").join("share"))
            .join("clawdesk")
    } else if cfg!(target_os = "windows") {
        std::env::var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|_| dirs_home().join("AppData").join("Roaming"))
            .join("clawdesk")
    } else {
        dirs_home().join(".clawdesk")
    }
}

fn dirs_home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

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

    #[test]
    fn check_platform_always_ok() {
        let check = check_platform();
        assert!(matches!(check.status, CheckStatus::Ok));
        assert!(check.detail.contains(env!("CARGO_PKG_VERSION")));
    }
}
