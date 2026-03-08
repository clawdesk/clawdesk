fn main() {
    // Load .env files: first ~/.clawdesk/.env, then CWD/.env.
    // Later files override earlier ones. This makes it easy to configure
    // DISCORD_TOKEN, DISCORD_APP_ID, etc. without the Settings UI.
    load_dotenv();

    // Initialize tracing subscriber so info!/warn!/error! logs are visible
    // on stderr AND written to ~/.clawdesk/logs/desktop.log for post-mortem
    // debugging (e.g. investigating why deleted agents reappear).
    use tracing_subscriber::prelude::*;

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    // Stderr layer — same as before.
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_names(true);

    // File layer — write to ~/.clawdesk/logs/desktop.log (rotated on startup).
    let log_dir = clawdesk_types::dirs::dot_clawdesk().join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("desktop.log");
    // Rotate: keep the previous run's log as desktop.log.prev
    if log_path.exists() {
        let prev = log_dir.join("desktop.log.prev");
        let _ = std::fs::rename(&log_path, &prev);
    }
    let file_layer = match std::fs::File::create(&log_path) {
        Ok(file) => {
            eprintln!("[clawdesk] Logging to {}", log_path.display());
            Some(
                tracing_subscriber::fmt::layer()
                    .with_ansi(false)
                    .with_target(true)
                    .with_thread_names(true)
                    .with_writer(std::sync::Mutex::new(file)),
            )
        }
        Err(e) => {
            eprintln!("[clawdesk] WARNING: could not open log file {}: {}", log_path.display(), e);
            None
        }
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(stderr_layer)
        .with(file_layer)
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
