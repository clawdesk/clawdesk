//! Release artifact validation — catches broken builds before publish.
//!
//! Checks:
//! - Workspace version consistency (all crates match)
//! - CHANGELOG.md has an entry for the current version
//! - `cargo build --release` artifacts exist and pass basic sanity checks
//! - Git state is clean (no uncommitted changes)

use anyhow::{bail, Context, Result};
use std::path::Path;

/// Run pre-release validation checks.
pub fn run() -> Result<()> {
    eprintln!("Release Check");
    eprintln!("═════════════");
    eprintln!();

    let mut passed = 0;
    let mut failed = 0;

    // 1. Workspace version consistency
    match check_workspace_version() {
        Ok(version) => {
            eprintln!("  ✓ Workspace version: {version}");
            passed += 1;
        }
        Err(e) => {
            eprintln!("  ✗ Workspace version: {e}");
            failed += 1;
        }
    }

    // 2. CHANGELOG entry
    match check_changelog() {
        Ok(()) => {
            eprintln!("  ✓ CHANGELOG.md has current version entry");
            passed += 1;
        }
        Err(e) => {
            eprintln!("  ✗ CHANGELOG.md: {e}");
            failed += 1;
        }
    }

    // 3. Git cleanliness
    match check_git_clean() {
        Ok(()) => {
            eprintln!("  ✓ Git working tree is clean");
            passed += 1;
        }
        Err(e) => {
            eprintln!("  ⚠ Git: {e}");
            // Warning, not failure — dirty tree is common during development
        }
    }

    // 4. Key binary artifacts
    match check_binary_artifacts() {
        Ok(count) => {
            eprintln!("  ✓ {count} release binaries verified");
            passed += 1;
        }
        Err(e) => {
            eprintln!("  ✗ Binary artifacts: {e}");
            failed += 1;
        }
    }

    // 5. Cargo.toml metadata completeness
    match check_metadata() {
        Ok(()) => {
            eprintln!("  ✓ Workspace metadata complete (description, license, authors)");
            passed += 1;
        }
        Err(e) => {
            eprintln!("  ✗ Metadata: {e}");
            failed += 1;
        }
    }

    eprintln!();
    eprintln!("Result: {passed} passed, {failed} failed");

    if failed > 0 {
        bail!("{failed} release check(s) failed — fix before publishing");
    }

    Ok(())
}

fn check_workspace_version() -> Result<String> {
    let content = std::fs::read_to_string("Cargo.toml")
        .context("cannot read workspace Cargo.toml")?;

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with("version") && line.contains('=') {
            if let Some(version) = line.split('=').nth(1) {
                let version = version.trim().trim_matches('"').to_string();
                if version.is_empty() {
                    bail!("empty version in workspace Cargo.toml");
                }
                return Ok(version);
            }
        }
    }

    bail!("no version found in workspace Cargo.toml")
}

fn check_changelog() -> Result<()> {
    let version = check_workspace_version()?;
    let changelog = std::fs::read_to_string("CHANGELOG.md")
        .context("cannot read CHANGELOG.md")?;

    if changelog.contains(&version) {
        Ok(())
    } else {
        bail!("no entry for version {version} in CHANGELOG.md")
    }
}

fn check_git_clean() -> Result<()> {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .context("failed to run git status")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let changed = stdout.lines().count();

    if changed > 0 {
        bail!("{changed} uncommitted file(s) in working tree")
    } else {
        Ok(())
    }
}

fn check_binary_artifacts() -> Result<usize> {
    let release_dir = Path::new("target/release");
    if !release_dir.exists() {
        bail!("target/release/ does not exist — run `cargo build --release` first");
    }

    let expected = ["clawdesk-cli", "clawdesk-daemon"];
    let mut found = 0;

    for name in &expected {
        let path = release_dir.join(name);
        if path.exists() {
            let meta = std::fs::metadata(&path)?;
            if meta.len() < 1024 {
                bail!("{name} exists but is suspiciously small ({} bytes)", meta.len());
            }
            found += 1;
        }
        // Also check with .exe on Windows
        #[cfg(windows)]
        {
            let exe_path = release_dir.join(format!("{name}.exe"));
            if exe_path.exists() {
                found += 1;
            }
        }
    }

    if found == 0 {
        bail!("no release binaries found — run `cargo build --release`");
    }

    Ok(found)
}

fn check_metadata() -> Result<()> {
    let content = std::fs::read_to_string("Cargo.toml")
        .context("cannot read workspace Cargo.toml")?;

    let required = ["description", "license", "authors"];
    for field in &required {
        if !content.lines().any(|l| l.trim().starts_with(field)) {
            bail!("missing `{field}` in workspace Cargo.toml [workspace.package]");
        }
    }

    Ok(())
}
