//! Logging module with built-in log rotation using tracing-appender.
//!
//! This module provides a rolling file appender that handles log rotation
//! internally, without requiring external tools like logrotate.
//! This works natively on all platforms including macOS.

use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Rotation period for log files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RotationPeriod {
    /// Rotate log files every minute.
    Minutely,
    /// Rotate log files every hour.
    Hourly,
    /// Rotate log files every day (default).
    #[default]
    Daily,
    /// Never rotate log files.
    Never,
}

impl std::str::FromStr for RotationPeriod {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "minutely" | "minute" => Ok(RotationPeriod::Minutely),
            "hourly" | "hour" => Ok(RotationPeriod::Hourly),
            "daily" | "day" => Ok(RotationPeriod::Daily),
            "never" | "none" => Ok(RotationPeriod::Never),
            _ => Err(format!(
                "Invalid rotation period '{}'. Valid options: minutely, hourly, daily, never",
                s
            )),
        }
    }
}

impl From<RotationPeriod> for Rotation {
    fn from(period: RotationPeriod) -> Self {
        match period {
            RotationPeriod::Minutely => Rotation::MINUTELY,
            RotationPeriod::Hourly => Rotation::HOURLY,
            RotationPeriod::Daily => Rotation::DAILY,
            RotationPeriod::Never => Rotation::NEVER,
        }
    }
}

/// Configuration for file-based logging with rotation.
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// Directory where log files will be stored.
    pub log_dir: String,
    /// Prefix for log file names.
    pub log_prefix: String,
    /// How often to rotate log files.
    pub rotation: RotationPeriod,
    /// Maximum number of log files to keep (0 = unlimited).
    pub max_log_files: usize,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            log_dir: ".".to_string(),
            log_prefix: "comelit-hub".to_string(),
            rotation: RotationPeriod::Daily,
            max_log_files: 7,
        }
    }
}

/// Guard that must be kept alive to ensure logs are flushed.
///
/// When this guard is dropped, any remaining logs will be flushed to the output.
/// Keep this value alive for the duration of your program.
pub struct LogGuard {
    _guards: Vec<WorkerGuard>,
}

/// Sets up console-only logging (stdout/stderr).
///
/// Returns a guard that must be kept alive for the duration of the program.
pub fn setup_console_logging() -> LogGuard {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    LogGuard { _guards: vec![] }
}

/// Sets up file-based logging with automatic rotation.
///
/// Log files will be created in the specified directory with the given prefix.
/// Files are automatically rotated based on the configured rotation period.
///
/// # Arguments
///
/// * `config` - Configuration for the log files
///
/// # Returns
///
/// A guard that must be kept alive for the duration of the program.
/// When the guard is dropped, remaining logs will be flushed.
///
/// # Example
///
/// ```ignore
/// let config = LogConfig {
///     log_dir: "/var/log/comelit".to_string(),
///     log_prefix: "comelit-hub".to_string(),
///     rotation: RotationPeriod::Daily,
///     max_log_files: 7,
/// };
/// let _guard = setup_file_logging(config)?;
/// // ... application runs ...
/// // guard is dropped here, flushing any remaining logs
/// ```
pub fn setup_file_logging(config: LogConfig) -> std::io::Result<LogGuard> {
    let log_dir = Path::new(&config.log_dir);

    // Clean up old log files if max_log_files is set
    if config.max_log_files > 0 {
        cleanup_old_logs(log_dir, &config.log_prefix, config.max_log_files)?;
    }

    // Create the rolling file appender
    let file_appender = RollingFileAppender::builder()
        .rotation(config.rotation.into())
        .filename_prefix(&config.log_prefix)
        .filename_suffix("log")
        .max_log_files(config.max_log_files)
        .build(log_dir)
        .map_err(std::io::Error::other)?;

    // Use non-blocking writer for better performance
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    // Build the subscriber with file output
    let file_layer = Layer::default()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true)
        .with_thread_ids(false)
        .with_file(true)
        .with_line_number(true);

    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(file_layer)
        .init();

    Ok(LogGuard {
        _guards: vec![guard],
    })
}

/// Sets up logging with both file and console output.
///
/// Logs at INFO level and above go to the file, while console output
/// respects the RUST_LOG environment variable.
///
/// # Arguments
///
/// * `config` - Configuration for the log files
///
/// # Returns
///
/// A guard that must be kept alive for the duration of the program.
pub fn setup_dual_logging(config: LogConfig) -> std::io::Result<LogGuard> {
    let log_dir = Path::new(&config.log_dir);

    // Clean up old log files if max_log_files is set
    if config.max_log_files > 0 {
        cleanup_old_logs(log_dir, &config.log_prefix, config.max_log_files)?;
    }

    // Create the rolling file appender
    let file_appender = RollingFileAppender::builder()
        .rotation(config.rotation.into())
        .filename_prefix(&config.log_prefix)
        .filename_suffix("log")
        .max_log_files(config.max_log_files)
        .build(log_dir)
        .map_err(std::io::Error::other)?;

    // Use non-blocking writer for better performance
    let (non_blocking_file, file_guard) = tracing_appender::non_blocking(file_appender);

    // File layer - no ANSI codes
    let file_layer = Layer::default()
        .with_writer(non_blocking_file)
        .with_ansi(false)
        .with_target(true)
        .with_file(true)
        .with_line_number(true);

    // Console layer - with ANSI colors
    let console_layer = Layer::default()
        .with_writer(std::io::stdout)
        .with_ansi(true)
        .with_target(true)
        .with_level(true);

    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(file_layer)
        .with(console_layer)
        .init();

    Ok(LogGuard {
        _guards: vec![file_guard],
    })
}

/// Cleans up old log files, keeping only the most recent ones.
///
/// This is called automatically when `max_log_files` is set and > 0.
fn cleanup_old_logs(log_dir: &Path, prefix: &str, max_files: usize) -> std::io::Result<()> {
    if !log_dir.exists() {
        return Ok(());
    }

    let mut log_files: Vec<_> = std::fs::read_dir(log_dir)?
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .map(|name| name.starts_with(prefix) && name.ends_with(".log"))
                .unwrap_or(false)
        })
        .filter_map(|entry| {
            entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .map(|mtime| (entry.path(), mtime))
        })
        .collect();

    // Sort by modification time, newest first
    log_files.sort_by(|a, b| b.1.cmp(&a.1));

    // Remove files beyond the limit
    for (path, _) in log_files.into_iter().skip(max_files) {
        if let Err(e) = std::fs::remove_file(&path) {
            eprintln!("Warning: failed to remove old log file {:?}: {}", path, e);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_rotation_period_from_str() {
        assert_eq!(
            "daily".parse::<RotationPeriod>().unwrap(),
            RotationPeriod::Daily
        );
        assert_eq!(
            "hourly".parse::<RotationPeriod>().unwrap(),
            RotationPeriod::Hourly
        );
        assert_eq!(
            "minutely".parse::<RotationPeriod>().unwrap(),
            RotationPeriod::Minutely
        );
        assert_eq!(
            "never".parse::<RotationPeriod>().unwrap(),
            RotationPeriod::Never
        );
        assert!("invalid".parse::<RotationPeriod>().is_err());
    }

    #[test]
    fn test_rotation_period_case_insensitive() {
        assert_eq!(
            "DAILY".parse::<RotationPeriod>().unwrap(),
            RotationPeriod::Daily
        );
        assert_eq!(
            "Daily".parse::<RotationPeriod>().unwrap(),
            RotationPeriod::Daily
        );
    }

    #[test]
    fn test_cleanup_old_logs() {
        let temp_dir = TempDir::new().unwrap();
        let log_dir = temp_dir.path();

        // Create some fake log files
        for i in 0..5 {
            let path = log_dir.join(format!("test-{}.log", i));
            std::fs::write(&path, format!("log content {}", i)).unwrap();
            // Add a small delay to ensure different modification times
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        // Keep only 2 files
        cleanup_old_logs(log_dir, "test-", 2).unwrap();

        let remaining: Vec<_> = std::fs::read_dir(log_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| n.starts_with("test-") && n.ends_with(".log"))
                    .unwrap_or(false)
            })
            .collect();

        assert_eq!(remaining.len(), 2);
    }

    #[test]
    fn test_log_config_default() {
        let config = LogConfig::default();
        assert_eq!(config.log_dir, ".");
        assert_eq!(config.log_prefix, "comelit-hub");
        assert_eq!(config.rotation, RotationPeriod::Daily);
        assert_eq!(config.max_log_files, 7);
    }
}
