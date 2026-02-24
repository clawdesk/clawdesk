fn main() {
    // Load .env files: first ~/.clawdesk/.env, then CWD/.env.
    // Later files override earlier ones. This makes it easy to configure
    // DISCORD_TOKEN, DISCORD_APP_ID, etc. without the Settings UI.
    load_dotenv();

    // Initialize tracing subscriber so info!/warn!/error! logs are visible
    // on stderr. Without this, all tracing output is silently dropped.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(true)
        .with_thread_names(true)
        .init();

    clawdesk_tauri_lib::run();
}

/// Load environment variables from .env files.
/// Reads `~/.clawdesk/.env` and `$CWD/.env` (if they exist).
/// Only sets vars that are NOT already present in the environment,
/// so explicit exports always win.
fn load_dotenv() {
    let paths: Vec<std::path::PathBuf> = [
        std::env::var("HOME")
            .ok()
            .map(|h| std::path::PathBuf::from(h).join(".clawdesk").join(".env")),
        Some(std::path::PathBuf::from(".env")),
    ]
    .into_iter()
    .flatten()
    .collect();

    for path in paths {
        if let Ok(contents) = std::fs::read_to_string(&path) {
            for line in contents.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    let key = key.trim();
                    let value = value.trim().trim_matches('"').trim_matches('\'');
                    // Only set if not already present — explicit env wins
                    if std::env::var(key).is_err() {
                        std::env::set_var(key, value);
                    }
                }
            }
            eprintln!("[clawdesk] Loaded env from {}", path.display());
        }
    }
}
