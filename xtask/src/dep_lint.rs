//! Compile-time dependency boundary enforcement for the hexagonal architecture.
//!
//! Models the crate graph as a DAG G = (V, E) where V = workspace crates and
//! E = dependency edges. Assigns each crate a layer L(v) ∈ {0..7} and verifies
//! the invariant: ∀(u,v) ∈ E, L(u) > L(v) — strictly downward dependencies only.
//!
//! Complexity: O(V + E) via adjacency list traversal.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::process::Command;

/// Layer assignments for the hexagonal architecture.
/// Layer 0 = foundation (zero internal deps), Layer 7 = leaf binaries.
fn layer_assignments() -> HashMap<&'static str, u8> {
    let mut m = HashMap::new();

    // Layer 0: Foundation — zero internal dependencies
    m.insert("clawdesk-types", 0);

    // Layer 1: Storage traits — depends only on types
    m.insert("clawdesk-storage", 1);

    // Layer 2: Domain + storage implementations
    m.insert("clawdesk-domain", 2);
    m.insert("clawdesk-sochdb", 2);
    m.insert("clawdesk-simd", 2);

    // Layer 3: Adapters — depend on storage traits + domain
    m.insert("clawdesk-providers", 3);
    m.insert("clawdesk-security", 3);
    m.insert("clawdesk-memory", 3);
    m.insert("clawdesk-channel", 3);
    m.insert("clawdesk-browser", 3);
    m.insert("clawdesk-plugin", 3);
    m.insert("clawdesk-media", 3);
    m.insert("clawdesk-threads", 3);

    // Layer 4: Implementations — depend on adapters
    m.insert("clawdesk-channels", 4);
    m.insert("clawdesk-agents", 4);
    m.insert("clawdesk-skills", 4);
    m.insert("clawdesk-sandbox", 4);
    m.insert("clawdesk-mcp", 4);
    m.insert("clawdesk-canvas", 4);
    m.insert("clawdesk-autoreply", 4);
    m.insert("clawdesk-cron", 4);
    m.insert("clawdesk-rag", 4);
    m.insert("clawdesk-local-models", 4);

    // Layer 5: Orchestration — depend on implementations
    m.insert("clawdesk-gateway", 5);
    m.insert("clawdesk-runtime", 5);
    m.insert("clawdesk-acp", 5);
    m.insert("clawdesk-bus", 5);
    m.insert("clawdesk-planner", 5);
    m.insert("clawdesk-consensus", 5);
    m.insert("clawdesk-adapters", 5);
    m.insert("clawdesk-core", 5); // Orchestration center — depends on gateway+agents+providers

    // Layer 4.5: Cognitive subsystems — depend on agents/memory
    // These are aspirational modules; some may not be wired into main execution.
    m.insert("clawdesk-metacognition", 4);
    m.insert("clawdesk-curiosity", 4);
    m.insert("clawdesk-worldmodel", 4);
    m.insert("clawdesk-user-model", 4);
    m.insert("clawdesk-user-predict", 4);
    m.insert("clawdesk-selfdiag", 4);
    m.insert("clawdesk-procedural", 4);
    m.insert("clawdesk-voice", 4);
    m.insert("clawdesk-polls", 4);
    m.insert("clawdesk-agent-config", 4);
    m.insert("clawdesk-channel-plugins", 4);
    m.insert("clawdesk-wizard", 6);

    // Layer 6: Infrastructure — depend on orchestration
    m.insert("clawdesk-daemon", 6);
    m.insert("clawdesk-infra", 6);
    m.insert("clawdesk-extensions", 6);
    m.insert("clawdesk-discovery", 6);
    m.insert("clawdesk-tunnel", 6);
    m.insert("clawdesk-observability", 6);
    m.insert("clawdesk-telemetry", 6);
    m.insert("clawdesk-migrate", 6);

    // Layer 7: Leaf binaries — can depend on anything below
    m.insert("clawdesk-cli", 7);
    m.insert("clawdesk-tauri", 7);
    m.insert("clawdesk-tui", 7);
    m.insert("clawdesk-bench", 7);
    m.insert("clawdesk-test", 7);

    m
}

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<Package>,
    workspace_members: Vec<String>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    id: String,
    dependencies: Vec<Dependency>,
}

#[derive(Deserialize)]
struct Dependency {
    name: String,
    path: Option<String>,
}

pub fn run() -> Result<()> {
    eprintln!("Checking dependency boundaries...");

    let output = Command::new("cargo")
        .args(["metadata", "--format-version=1", "--no-deps"])
        .output()
        .context("Failed to run cargo metadata")?;

    if !output.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let metadata: CargoMetadata =
        serde_json::from_slice(&output.stdout).context("Failed to parse cargo metadata")?;

    let layers = layer_assignments();

    // Build set of workspace member names
    let workspace_names: std::collections::HashSet<String> = metadata
        .packages
        .iter()
        .filter(|p| metadata.workspace_members.iter().any(|wm| wm.starts_with(&format!("{} ", p.name))))
        .map(|p| p.name.clone())
        .collect();

    let mut violations = Vec::new();
    let mut checked = 0u32;

    for pkg in &metadata.packages {
        if !workspace_names.contains(&pkg.name) {
            continue;
        }

        let Some(&pkg_layer) = layers.get(pkg.name.as_str()) else {
            eprintln!("  WARNING: crate '{}' has no layer assignment, skipping", pkg.name);
            continue;
        };

        for dep in &pkg.dependencies {
            // Only check internal workspace dependencies (those with a path)
            if dep.path.is_none() || !workspace_names.contains(&dep.name) {
                continue;
            }

            let Some(&dep_layer) = layers.get(dep.name.as_str()) else {
                continue;
            };

            checked += 1;

            // Invariant: dependency must be on a strictly lower layer
            if dep_layer >= pkg_layer {
                violations.push(format!(
                    "  VIOLATION: {} (layer {}) -> {} (layer {}): dependency must point downward",
                    pkg.name, pkg_layer, dep.name, dep_layer
                ));
            }
        }
    }

    eprintln!("  Checked {} internal dependency edges", checked);

    if violations.is_empty() {
        eprintln!("  All dependency boundaries OK.");
        Ok(())
    } else {
        eprintln!("\nDependency boundary violations found:\n");
        for v in &violations {
            eprintln!("{}", v);
        }
        bail!("{} dependency boundary violation(s) found", violations.len());
    }
}
