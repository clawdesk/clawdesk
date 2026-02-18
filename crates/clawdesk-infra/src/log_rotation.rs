//! Log file rotation — daily rolling log files with configurable retention.
//!
//! Provides `RotatingFileWriter`, an `io::Write` implementor that:
//! - Creates a new log file each day (YYYY-MM-DD.log)
//! - Cleans up log files older than `max_days` on rotation
//! - Is suitable as a `MakeWriter` for `tracing_subscriber::fmt::Layer`
//!
//! No external dependency on `tracing-appender` — uses standard file I/O.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use tracing::debug;

/// Configuration for log file rotation.
#[derive(Debug, Clone)]
pub struct LogRotationConfig {
    /// Directory where log files are stored.
    pub log_dir: PathBuf,
    /// Prefix for log file names (e.g., "clawdesk" → "clawdesk.2025-01-15.log").
    pub file_prefix: String,
    /// Maximum number of days to retain log files.
    pub max_days: u32,
}

impl Default for LogRotationConfig {
    fn default() -> Self {
        Self {
            log_dir: PathBuf::from("logs"),
            file_prefix: "clawdesk".to_string(),
            max_days: 7,
        }
    }
}

/// A thread-safe rotating file writer.
///
/// Opens a new file each day and cleans up old files on rotation.
pub struct RotatingFileWriter {
    config: LogRotationConfig,
    inner: Mutex<WriterState>,
}

struct WriterState {
    current_date: String,
    file: Option<File>,
}

impl RotatingFileWriter {
    /// Create a new rotating file writer. Creates the log directory if needed.
    pub fn new(config: LogRotationConfig) -> io::Result<Self> {
        fs::create_dir_all(&config.log_dir)?;

        let today = today_string();
        let file = open_log_file(&config.log_dir, &config.file_prefix, &today)?;

        Ok(Self {
            config,
            inner: Mutex::new(WriterState {
                current_date: today,
                file: Some(file),
            }),
        })
    }

    /// Write bytes, rotating the file if the date has changed.
    pub fn write_bytes(&self, buf: &[u8]) -> io::Result<usize> {
        let mut state = self.inner.lock().map_err(|_| {
            io::Error::new(io::ErrorKind::Other, "lock poisoned")
        })?;

        let today = today_string();
        if today != state.current_date {
            // Rotate: close old file, open new one, clean up old files.
            state.file = None;
            state.file = Some(open_log_file(
                &self.config.log_dir,
                &self.config.file_prefix,
                &today,
            )?);
            state.current_date = today;
            self.cleanup_old_files();
        }

        if let Some(ref mut file) = state.file {
            file.write(buf)
        } else {
            Err(io::Error::new(io::ErrorKind::Other, "no log file"))
        }
    }

    /// Remove log files older than max_days.
    fn cleanup_old_files(&self) {
        let prefix = &self.config.file_prefix;
        let max_days = self.config.max_days;

        let cutoff = chrono::Utc::now()
            .checked_sub_signed(chrono::Duration::days(max_days as i64))
            .map(|d| d.format("%Y-%m-%d").to_string());

        let Some(cutoff_str) = cutoff else { return };

        let Ok(entries) = fs::read_dir(&self.config.log_dir) else {
            return;
        };

        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(prefix) || !name.ends_with(".log") {
                continue;
            }
            // Extract date from "prefix.YYYY-MM-DD.log"
            let date_part = name
                .strip_prefix(prefix)
                .and_then(|s| s.strip_prefix('.'))
                .and_then(|s| s.strip_suffix(".log"));
            if let Some(date) = date_part {
                if date < cutoff_str.as_str() {
                    if let Err(e) = fs::remove_file(entry.path()) {
                        debug!(file = %name, error = %e, "Failed to remove old log");
                    } else {
                        debug!(file = %name, "Removed old log file");
                    }
                }
            }
        }
    }

    /// Flush the current log file.
    pub fn flush_inner(&self) -> io::Result<()> {
        let mut state = self.inner.lock().map_err(|_| {
            io::Error::new(io::ErrorKind::Other, "lock poisoned")
        })?;
        if let Some(ref mut file) = state.file {
            file.flush()
        } else {
            Ok(())
        }
    }

    /// Get the path to the current log file.
    pub fn current_path(&self) -> PathBuf {
        let state = self.inner.lock().unwrap();
        log_file_path(
            &self.config.log_dir,
            &self.config.file_prefix,
            &state.current_date,
        )
    }
}

impl Write for RotatingFileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_bytes(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_inner()
    }
}

/// Generate today's date string (YYYY-MM-DD).
fn today_string() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Build the log file path.
fn log_file_path(dir: &Path, prefix: &str, date: &str) -> PathBuf {
    dir.join(format!("{prefix}.{date}.log"))
}

/// Open (or create) a log file in append mode.
fn open_log_file(dir: &Path, prefix: &str, date: &str) -> io::Result<File> {
    let path = log_file_path(dir, prefix, date);
    OpenOptions::new().create(true).append(true).open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_log_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("clawdesk_log_test_{name}_{}", std::process::id()))
    }

    #[test]
    fn test_rotating_writer() {
        let dir = temp_log_dir("rotate");
        let _ = fs::remove_dir_all(&dir);

        let config = LogRotationConfig {
            log_dir: dir.clone(),
            file_prefix: "test".to_string(),
            max_days: 7,
        };
        let writer = RotatingFileWriter::new(config).unwrap();

        // Write something.
        let bytes_written = writer.write_bytes(b"hello log\n").unwrap();
        assert!(bytes_written > 0);

        // Verify file exists.
        let path = writer.current_path();
        assert!(path.exists());

        // Read back contents.
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("hello log"));

        // Cleanup.
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_log_file_path() {
        let p = log_file_path(Path::new("/tmp/logs"), "clawdesk", "2025-01-15");
        assert_eq!(p.to_str().unwrap(), "/tmp/logs/clawdesk.2025-01-15.log");
    }

    #[test]
    fn test_cleanup_old_files() {
        let dir = temp_log_dir("cleanup");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        // Create a fake old log file (date far in the past).
        let old_path = dir.join("app.2020-01-01.log");
        fs::write(&old_path, "old").unwrap();

        // Create a recent log file.
        let today = today_string();
        let recent_path = dir.join(format!("app.{today}.log"));
        fs::write(&recent_path, "recent").unwrap();

        let config = LogRotationConfig {
            log_dir: dir.clone(),
            file_prefix: "app".to_string(),
            max_days: 7,
        };
        let writer = RotatingFileWriter::new(config).unwrap();
        writer.cleanup_old_files();

        // Old file should be removed.
        assert!(!old_path.exists(), "old log file should be deleted");
        // Recent file should remain.
        assert!(recent_path.exists(), "recent log file should remain");

        let _ = fs::remove_dir_all(&dir);
    }
}
