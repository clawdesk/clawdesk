mod dep_lint;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", about = "ClawDesk build automation")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Verify crate dependency boundaries match the hexagonal layer ordering
    DepLint,
    /// Run the full CI pre-flight suite locally
    Ci,
    /// Run benchmarks and compare against baseline
    Bench,
    /// Build and deploy documentation
    Docs,
    /// Manage fuzz testing targets
    Fuzz,
    /// Prepare a release (version bump + changelog)
    Release,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::DepLint => dep_lint::run(),
        Command::Ci => run_ci(),
        Command::Bench => run_bench(),
        Command::Docs => run_docs(),
        Command::Fuzz => run_fuzz(),
        Command::Release => run_release(),
    }
}

fn run_ci() -> Result<()> {
    eprintln!("Running CI pre-flight...");

    // 1. Dependency boundary lint
    dep_lint::run()?;

    // 2. Format check
    run_cmd("cargo", &["fmt", "--all", "--check"])?;

    // 3. Clippy
    run_cmd("cargo", &["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"])?;

    // 4. Tests
    run_cmd("cargo", &["test", "--workspace"])?;

    // 5. Doc tests
    run_cmd("cargo", &["test", "--workspace", "--doc"])?;

    eprintln!("CI pre-flight passed.");
    Ok(())
}

fn run_bench() -> Result<()> {
    run_cmd("cargo", &["bench", "--package", "clawdesk-bench"])
}

fn run_docs() -> Result<()> {
    run_cmd("cargo", &["doc", "--workspace", "--no-deps", "--document-private-items"])
}

fn run_fuzz() -> Result<()> {
    eprintln!("Fuzz targets:");
    eprintln!("  cargo +nightly fuzz run fuzz_websocket");
    eprintln!("  cargo +nightly fuzz run fuzz_sse_stream");
    eprintln!("  cargo +nightly fuzz run fuzz_jsonrpc");
    eprintln!("  cargo +nightly fuzz run fuzz_a2a_message");
    Ok(())
}

fn run_release() -> Result<()> {
    eprintln!("Release preparation not yet automated. Steps:");
    eprintln!("  1. Update version in workspace Cargo.toml");
    eprintln!("  2. Update CHANGELOG.md");
    eprintln!("  3. git tag v<version>");
    eprintln!("  4. git push --tags");
    Ok(())
}

fn run_cmd(cmd: &str, args: &[&str]) -> Result<()> {
    let status = std::process::Command::new(cmd)
        .args(args)
        .status()?;
    if !status.success() {
        anyhow::bail!("{} {} failed with {}", cmd, args.join(" "), status);
    }
    Ok(())
}
