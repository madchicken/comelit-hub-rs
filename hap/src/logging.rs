//! Logging module with support for reopening log files on SIGHUP.
//!
//! This module provides a `ReopenableFile` writer that can be used with
//! `tracing-subscriber` and supports reopening the underlying file when
//! a SIGHUP signal is received (for logrotate compatibility).

use parking_lot::RwLock;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::info;

/// A file writer that can be reopened without dropping the writer.
///
/// This is useful for log rotation scenarios where an external tool
/// (like logrotate or newsyslog) moves the log file and sends SIGHUP
/// to signal the application to reopen the log file.
#[derive(Debug)]
pub struct ReopenableFile {
    path: PathBuf,
    file: RwLock<File>,
    reopen_flag: AtomicBool,
}

impl ReopenableFile {
    /// Creates a new `ReopenableFile` that writes to the specified path.
    ///
    /// The file is opened in append mode, creating it if it doesn't exist.
    pub fn new(path: impl Into<PathBuf>) -> io::Result<Self> {
        let path = path.into();
        let file = Self::open_file(&path)?;
        Ok(Self {
            path,
            file: RwLock::new(file),
            reopen_flag: AtomicBool::new(false),
        })
    }

    fn open_file(path: &PathBuf) -> io::Result<File> {
        OpenOptions::new().create(true).append(true).open(path)
    }

    /// Signals that the file should be reopened on the next write.
    ///
    /// This is typically called from a signal handler.
    pub fn signal_reopen(&self) {
        self.reopen_flag.store(true, Ordering::SeqCst);
    }

    /// Reopens the underlying file.
    ///
    /// This should be called after log rotation to ensure new log entries
    /// go to the new file instead of the rotated one.
    pub fn reopen(&self) -> io::Result<()> {
        let new_file = Self::open_file(&self.path)?;
        let mut file_guard = self.file.write();
        *file_guard = new_file;
        Ok(())
    }

    /// Checks if a reopen was signaled and performs it if necessary.
    fn check_and_reopen(&self) -> io::Result<()> {
        if self.reopen_flag.swap(false, Ordering::SeqCst) {
            self.reopen()?;
        }
        Ok(())
    }
}

impl Write for &ReopenableFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Check if we need to reopen before writing
        self.check_and_reopen()?;
        self.file.write().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.write().flush()
    }
}

/// A handle to a `ReopenableFile` that can be cloned and shared.
///
/// This is the type you pass to `tracing_subscriber::fmt().with_writer()`.
#[derive(Clone, Debug)]
pub struct ReopenableFileHandle {
    inner: Arc<ReopenableFile>,
}

impl ReopenableFileHandle {
    /// Creates a new handle wrapping a `ReopenableFile`.
    pub fn new(file: ReopenableFile) -> Self {
        Self {
            inner: Arc::new(file),
        }
    }

    /// Signals that the file should be reopened on the next write.
    #[allow(dead_code)]
    pub fn signal_reopen(&self) {
        self.inner.signal_reopen();
    }

    /// Immediately reopens the underlying file.
    pub fn reopen(&self) -> io::Result<()> {
        self.inner.reopen()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ReopenableFileHandle {
    type Writer = &'a ReopenableFile;

    fn make_writer(&'a self) -> Self::Writer {
        &self.inner
    }
}

/// Sets up the SIGHUP signal handler to reopen log files.
///
/// Returns the signal handler thread's join handle.
///
/// # Arguments
///
/// * `log_handle` - Optional handle to the main log file
/// * `err_handle` - Optional handle to the error log file
pub fn setup_sighup_handler(
    log_handle: Option<ReopenableFileHandle>,
    err_handle: Option<ReopenableFileHandle>,
) -> std::thread::JoinHandle<()> {
    use signal_hook::{consts::SIGHUP, iterator::Signals};

    let mut signals = Signals::new([SIGHUP]).expect("Failed to register SIGHUP handler");

    std::thread::spawn(move || {
        for _ in signals.forever() {
            info!("Received SIGHUP, reopening log files...");

            if let Some(ref handle) = log_handle
                && let Err(e) = handle.reopen()
            {
                eprintln!("Failed to reopen log file: {}", e);
            }

            if let Some(ref handle) = err_handle
                && let Err(e) = handle.reopen()
            {
                eprintln!("Failed to reopen error log file: {}", e);
            }

            info!("Log files reopened successfully");
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn test_reopenable_file_write() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join(format!("test_log_{}.log", std::process::id()));

        let reopenable = ReopenableFile::new(&path).unwrap();
        let mut writer: &ReopenableFile = &reopenable;
        writer.write_all(b"test line\n").unwrap();
        writer.flush().unwrap();

        let mut contents = String::new();
        File::open(&path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "test line\n");

        // Cleanup
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn test_reopenable_file_reopen() {
        let temp_dir = std::env::temp_dir();
        let path = temp_dir.join(format!("test_log_reopen_{}.log", std::process::id()));

        let reopenable = ReopenableFile::new(&path).unwrap();
        let mut writer: &ReopenableFile = &reopenable;
        writer.write_all(b"before reopen\n").unwrap();
        writer.flush().unwrap();

        // Simulate log rotation by renaming the file
        let rotated_path = path.with_extension("rotated");
        std::fs::rename(&path, &rotated_path).unwrap();

        // Reopen should create a new file at the original path
        reopenable.reopen().unwrap();
        writer.write_all(b"after reopen\n").unwrap();
        writer.flush().unwrap();

        // Check that the new file has only the new content
        let mut contents = String::new();
        File::open(&path)
            .unwrap()
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "after reopen\n");

        // Clean up
        std::fs::remove_file(&path).ok();
        std::fs::remove_file(rotated_path).ok();
    }
}
