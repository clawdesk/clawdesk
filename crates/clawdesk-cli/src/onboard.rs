//! Interactive onboarding wizard for first-time ClawDesk setup.
//!
//! Guides users through provider API key configuration, default model selection,
//! channel setup, and data directory initialization.
//!
//! ## Flow
//! 1. Welcome banner + platform detection
//! 2. Provider API key setup (Anthropic, OpenAI, Gemini, Ollama)
//! 3. Default model selection
//! 4. Channel configuration (Telegram, Discord, Slack)
//! 5. Data directory initialization
//! 6. Auto-run `clawdesk doctor` to verify

use std::io::{self, Write};
use std::path::PathBuf;

/// Run the interactive onboarding wizard.
pub async fn run_onboarding() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    println!();
    println!("╔══════════════════════════════════════════════════════╗");
    println!("║           Welcome to ClawDesk! 🤖                   ║");
    println!("║   Multi-Channel AI Agent Gateway                    ║");
    println!("╚══════════════════════════════════════════════════════╝");
    println!();

    // Platform info
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    println!("  Platform: {} / {}", os, arch);
    println!("  Version:  {}", env!("CARGO_PKG_VERSION"));
    println!();

    // Step 1: Data directory
    let data_dir = setup_data_dir()?;
    println!();

    // Step 2: Provider configuration
    let providers = setup_providers(&data_dir).await?;
    println!();

    // Step 3: Default model
    let model = select_default_model(&providers)?;
    println!();

    // Step 4: Channel configuration
    let channels = setup_channels(&data_dir)?;
    println!();

    // Summary
    println!("  ─── Setup Complete ───");
    println!();
    println!("  Providers: {}", if providers.is_empty() { "none".to_string() } else { providers.join(", ") });
    if let Some(ref m) = model {
        println!("  Default model: {}", m);
    }
    println!("  Channels: {}", if channels.is_empty() { "none".to_string() } else { channels.join(", ") });
    println!("  Data dir: {}", data_dir.display());
    println!();
    println!("  Next steps:");
    println!("    clawdesk doctor           — verify configuration");
    println!("    clawdesk gateway run      — start the gateway");
    println!("    clawdesk agent msg \"hi\"   — test the agent");
    println!();

    Ok(())
}

/// Setup the data directory.
fn setup_data_dir() -> Result<PathBuf, Box<dyn std::error::Error + Send + Sync>> {
    let data_dir = clawdesk_types::dirs::data();
    let dot_dir = clawdesk_types::dirs::dot_clawdesk();
    let sochdb_dir = clawdesk_types::dirs::sochdb();
    let threads_dir = clawdesk_types::dirs::threads();
    let agents_dir = clawdesk_types::dirs::agents();
    let skills_dir = clawdesk_types::dirs::skills();

    println!("  [1/4] Data Directory");
    println!("  Platform data: {}", data_dir.display());
    println!("  Config/state:  {}", dot_dir.display());
    print!("  Use these locations? [Y/n]: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();

    if !input.is_empty() && !input.eq_ignore_ascii_case("y") && !input.eq_ignore_ascii_case("yes") {
        println!("  Using default locations.");
    }

    // Create platform data dir and its subdirs
    std::fs::create_dir_all(&data_dir)?;
    std::fs::create_dir_all(data_dir.join("credentials"))?;
    std::fs::create_dir_all(&skills_dir)?;
    std::fs::create_dir_all(data_dir.join("plugins"))?;

    // Create dot-clawdesk dir and subdirs used by Tauri desktop
    std::fs::create_dir_all(&dot_dir)?;
    std::fs::create_dir_all(&sochdb_dir)?;
    std::fs::create_dir_all(&threads_dir)?;
    std::fs::create_dir_all(&agents_dir)?;
    std::fs::create_dir_all(dot_dir.join("logs"))?;
    std::fs::create_dir_all(dot_dir.join("workspace"))?;

    println!("  ✓ Created {}", data_dir.display());
    println!("  ✓ Created {}", dot_dir.display());

    Ok(data_dir)
}

/// Setup LLM provider API keys.
async fn setup_providers(data_dir: &PathBuf) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    println!("  [2/4] Provider API Keys");
    println!("  Configure API keys for LLM providers.");
    println!("  (Press Enter to skip any provider)");
    println!();

    let creds_dir = data_dir.join("credentials");
    std::fs::create_dir_all(&creds_dir)?;

    let mut configured = Vec::new();
    let mut env_lines: Vec<String> = Vec::new();

    // Anthropic
    if let Some(key) = prompt_api_key("Anthropic", "ANTHROPIC_API_KEY", "sk-ant-")? {
        save_credential(&creds_dir, "anthropic", &key)?;
        env_lines.push(format!("ANTHROPIC_API_KEY={}", key));
        configured.push("anthropic".to_string());
    }

    // OpenAI
    if let Some(key) = prompt_api_key("OpenAI", "OPENAI_API_KEY", "sk-")? {
        save_credential(&creds_dir, "openai", &key)?;
        env_lines.push(format!("OPENAI_API_KEY={}", key));
        configured.push("openai".to_string());
    }

    // Gemini
    if let Some(key) = prompt_api_key("Google Gemini", "GEMINI_API_KEY", "AI")? {
        save_credential(&creds_dir, "gemini", &key)?;
        env_lines.push(format!("GEMINI_API_KEY={}", key));
        configured.push("gemini".to_string());
    }

    // Ollama — check connectivity instead of API key
    print!("  Ollama (local): checking localhost:11434... ");
    io::stdout().flush()?;
    let ollama_ok = check_ollama().await;
    if ollama_ok {
        println!("✓ running");
        configured.push("ollama".to_string());
    } else {
        println!("not found");
        println!("    Install: https://ollama.com/download");
    }

    // Write ~/.clawdesk/.env so the desktop app also picks up the keys
    if !env_lines.is_empty() {
        let dot_env_path = clawdesk_types::dirs::dot_clawdesk().join(".env");
        let mut contents = String::new();
        // Preserve existing .env entries
        if dot_env_path.exists() {
            if let Ok(existing) = std::fs::read_to_string(&dot_env_path) {
                for line in existing.lines() {
                    let key = line.split_once('=').map(|(k, _)| k.trim());
                    // Skip lines we're about to overwrite
                    let dominated = key.map(|k| env_lines.iter().any(|e| e.starts_with(&format!("{}=", k)))).unwrap_or(false);
                    if !dominated {
                        contents.push_str(line);
                        contents.push('\n');
                    }
                }
            }
        }
        for line in &env_lines {
            contents.push_str(line);
            contents.push('\n');
        }
        std::fs::write(&dot_env_path, contents)?;
        println!("  ✓ Wrote {} (shared with desktop app)", dot_env_path.display());
    }

    if configured.is_empty() {
        println!();
        println!("  ⚠ No providers configured. You'll need at least one to use the agent.");
    } else {
        println!("  ✓ {} provider(s) configured", configured.len());
    }

    Ok(configured)
}

/// Prompt for an API key, checking env var first.
fn prompt_api_key(
    name: &str,
    env_var: &str,
    prefix: &str,
) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    // Check environment variable first
    if let Ok(key) = std::env::var(env_var) {
        if !key.is_empty() {
            println!("  {} API key: found in ${}", name, env_var);
            return Ok(Some(key));
        }
    }

    print!("  {} API key ({}...): ", name, prefix);
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let key = input.trim().to_string();

    if key.is_empty() {
        println!("    skipped");
        Ok(None)
    } else {
        Ok(Some(key))
    }
}

/// Save an API key to the credentials directory.
fn save_credential(creds_dir: &PathBuf, provider: &str, key: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let data = serde_json::json!({ "api_key": key });
    let path = creds_dir.join(format!("{}.json", provider));
    std::fs::write(&path, serde_json::to_string_pretty(&data)?)?;
    println!("    ✓ saved to {}", path.display());
    Ok(())
}

/// Check if Ollama is running locally.
async fn check_ollama() -> bool {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap();
    client
        .get("http://localhost:11434/api/tags")
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Select a default model.
fn select_default_model(providers: &[String]) -> Result<Option<String>, Box<dyn std::error::Error + Send + Sync>> {
    println!("  [3/4] Default Model");

    if providers.is_empty() {
        println!("  No providers configured — skipping model selection.");
        return Ok(None);
    }

    let models: Vec<(&str, &str)> = providers
        .iter()
        .flat_map(|p| match p.as_str() {
            "anthropic" => vec![
                ("1", "claude-sonnet-4-20250514"),
                ("2", "claude-3-5-haiku-20241022"),
            ],
            "openai" => vec![
                ("3", "gpt-4o"),
                ("4", "gpt-4o-mini"),
            ],
            "gemini" => vec![
                ("5", "gemini-2.0-flash"),
                ("6", "gemini-2.5-pro"),
            ],
            "ollama" => vec![
                ("7", "llama3.2:latest"),
                ("8", "mistral:latest"),
            ],
            _ => vec![],
        })
        .collect();

    if models.is_empty() {
        return Ok(None);
    }

    for (num, model) in &models {
        println!("    [{}] {}", num, model);
    }

    print!("  Select default model [1]: ");
    io::stdout().flush()?;

    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let input = input.trim();

    let choice = if input.is_empty() { "1" } else { input };

    let model = models
        .iter()
        .find(|(num, _)| *num == choice)
        .map(|(_, model)| model.to_string())
        .or_else(|| models.first().map(|(_, m)| m.to_string()));

    if let Some(ref m) = model {
        println!("  ✓ Default model: {}", m);
    }

    Ok(model)
}

/// Setup basic channel configuration.
fn setup_channels(data_dir: &PathBuf) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    println!("  [4/4] Channels");
    println!("  Configure messaging channels (optional — can be done later).");
    println!();

    let mut configured = Vec::new();

    // Telegram
    print!("  Configure Telegram bot? [y/N]: ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    if input.trim().eq_ignore_ascii_case("y") {
        print!("    Bot token (@BotFather): ");
        io::stdout().flush()?;
        let mut token = String::new();
        io::stdin().read_line(&mut token)?;
        let token = token.trim();
        if !token.is_empty() {
            let config = serde_json::json!({
                "telegram": {
                    "bot_token": token,
                    "enable_groups": false
                }
            });
            let path = data_dir.join("channels.json");
            // Merge with existing if present
            let mut existing: serde_json::Value = if path.exists() {
                let content = std::fs::read_to_string(&path)?;
                serde_json::from_str(&content).unwrap_or_default()
            } else {
                serde_json::json!({})
            };
            if let Some(obj) = existing.as_object_mut() {
                obj.insert("telegram".to_string(), config["telegram"].clone());
            }
            std::fs::write(&path, serde_json::to_string_pretty(&existing)?)?;
            configured.push("telegram".to_string());
            println!("    ✓ Telegram configured");
        }
    }

    // Discord
    print!("  Configure Discord bot? [y/N]: ");
    io::stdout().flush()?;
    input.clear();
    io::stdin().read_line(&mut input)?;
    if input.trim().eq_ignore_ascii_case("y") {
        print!("    Bot token: ");
        io::stdout().flush()?;
        let mut token = String::new();
        io::stdin().read_line(&mut token)?;
        print!("    Application ID: ");
        io::stdout().flush()?;
        let mut app_id = String::new();
        io::stdin().read_line(&mut app_id)?;
        if !token.trim().is_empty() && !app_id.trim().is_empty() {
            let path = data_dir.join("channels.json");
            let mut existing: serde_json::Value = if path.exists() {
                let content = std::fs::read_to_string(&path)?;
                serde_json::from_str(&content).unwrap_or_default()
            } else {
                serde_json::json!({})
            };
            if let Some(obj) = existing.as_object_mut() {
                obj.insert("discord".to_string(), serde_json::json!({
                    "bot_token": token.trim(),
                    "application_id": app_id.trim()
                }));
            }
            std::fs::write(&path, serde_json::to_string_pretty(&existing)?)?;
            configured.push("discord".to_string());
            println!("    ✓ Discord configured");
        }
    }

    if configured.is_empty() {
        println!("  No channels configured. The built-in webchat channel is always available.");
    }

    Ok(configured)
}

// Removed: duplicated default_data_dir() and dirs_home().
// All path resolution now uses clawdesk_types::dirs.

#[cfg(test)]
mod tests {
    #[test]
    fn canonical_dirs_available() {
        let d = clawdesk_types::dirs::data();
        let s = d.to_string_lossy();
        assert!(s.contains("clawdesk"), "data dir should contain 'clawdesk': {}", s);
    }
}
