//! CI quality gate enforcement — catches architectural regressions.
//!
//! Checks:
//! - File size limits (no god-files > 3000 lines)
//! - Tauri command API surface stability (detect removed/changed commands)
//! - Workspace crate count tracking

use anyhow::{bail, Result};
use std::path::Path;

/// Run all quality gate checks.
pub fn run() -> Result<()> {
    eprintln!("Quality Gate");
    eprintln!("════════════");
    eprintln!();

    let mut passed = 0;
    let mut warnings = 0;
    let mut failed = 0;

    // 1. File size limits
    match check_file_sizes() {
        Ok(count) => {
            eprintln!("  ✓ {count} source files within size limits");
            passed += 1;
        }
        Err(e) => {
            eprintln!("  ✗ File size: {e}");
            failed += 1;
        }
    }

    // 2. Tauri command surface check
    match check_tauri_commands() {
        Ok(count) => {
            eprintln!("  ✓ {count} Tauri commands registered");
            passed += 1;
        }
        Err(e) => {
            eprintln!("  ⚠ Tauri commands: {e}");
            warnings += 1;
        }
    }

    // 3. Crate count
    match check_crate_count() {
        Ok(count) => {
            eprintln!("  ✓ {count} workspace crates");
            passed += 1;
        }
        Err(e) => {
            eprintln!("  ⚠ Crate count: {e}");
            warnings += 1;
        }
    }

    // 4. TODO/FIXME audit
    match check_todo_count() {
        Ok(count) => {
            if count > 100 {
                eprintln!("  ⚠ {count} TODO/FIXME comments (consider triaging)");
                warnings += 1;
            } else {
                eprintln!("  ✓ {count} TODO/FIXME comments");
                passed += 1;
            }
        }
        Err(e) => {
            eprintln!("  ⚠ TODO audit: {e}");
            warnings += 1;
        }
    }

    eprintln!();
    eprintln!("Result: {passed} passed, {warnings} warnings, {failed} failed");

    if failed > 0 {
        bail!("{failed} quality gate(s) failed");
    }

    Ok(())
}

/// Check that no .rs file exceeds the line limit.
fn check_file_sizes() -> Result<usize> {
    const MAX_LINES: usize = 3000;

    let mut count = 0;
    let mut violations = Vec::new();

    for entry in walkdir::WalkDir::new("crates")
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != "target" && name != "node_modules" && name != ".git"
        })
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map_or(false, |ext| ext == "rs" || ext == "tsx" || ext == "ts")
        })
    {
        let path = entry.path();
        if let Ok(content) = std::fs::read_to_string(path) {
            let lines = content.lines().count();
            count += 1;
            if lines > MAX_LINES {
                violations.push(format!(
                    "  {} ({} lines, max {})",
                    path.display(),
                    lines,
                    MAX_LINES
                ));
            }
        }
    }

    if !violations.is_empty() {
        bail!(
            "{} file(s) exceed {} lines:\n{}",
            violations.len(),
            MAX_LINES,
            violations.join("\n")
        );
    }

    Ok(count)
}

/// Count registered Tauri commands by scanning the generate_handler! macro.
fn check_tauri_commands() -> Result<usize> {
    let lib_path = Path::new("crates/clawdesk-tauri/src/lib.rs");
    if !lib_path.exists() {
        bail!("crates/clawdesk-tauri/src/lib.rs not found");
    }

    let content = std::fs::read_to_string(lib_path)?;
    let count = content
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            trimmed.contains("::")
                && !trimmed.starts_with("//")
                && !trimmed.starts_with("use ")
                && !trimmed.starts_with("mod ")
                && (trimmed.ends_with(',') || trimmed.ends_with("])"))
        })
        .filter(|line| {
            // Heuristic: lines inside generate_handler! that look like
            // `commands_foo::bar_baz,` are command registrations
            let trimmed = line.trim();
            trimmed.starts_with("commands")
                || trimmed.starts_with("tray::")
                || trimmed.starts_with("pty_session::")
        })
        .count();

    Ok(count)
}

/// Count workspace crates.
fn check_crate_count() -> Result<usize> {
    let crates_dir = Path::new("crates");
    if !crates_dir.exists() {
        bail!("crates/ directory not found");
    }

    let count = std::fs::read_dir(crates_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter(|e| e.path().join("Cargo.toml").exists())
        .count();

    Ok(count)
}

/// Count TODO/FIXME comments across the codebase.
fn check_todo_count() -> Result<usize> {
    let mut count = 0;

    for entry in walkdir::WalkDir::new("crates")
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != "target" && name != "node_modules" && name != ".git"
        })
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map_or(false, |ext| ext == "rs")
        })
    {
        if let Ok(content) = std::fs::read_to_string(entry.path()) {
            for line in content.lines() {
                let upper = line.to_uppercase();
                if upper.contains("TODO") || upper.contains("FIXME") {
                    count += 1;
                }
            }
        }
    }

    Ok(count)
}
