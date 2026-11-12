//! Structured JSON-line logging with size rotation and secret redaction.
//!
//! The logger writes one complete JSON object per line with stable fields:
//! epoch-millisecond `timestamp`, lowercase `level`, redacted `event`, and
//! recursively redacted `fields`. Rotated files are capped at a configurable
//! total count, and debug builds mirror the redacted line to stderr.

pub mod redact;
pub mod rotate;

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AppError;

use rotate::{RotatingWriter, LOG_FILE_NAME};

/// Default maximum size of a single log file: 10 MiB.
pub const DEFAULT_MAX_BYTES: u64 = 10 * 1024 * 1024;

/// Default maximum number of log files, including the current file.
pub const DEFAULT_MAX_FILES: usize = 5;

/// Severity of a log record, serialized in lowercase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// An operation failed and needs attention.
    Error,
    /// A recoverable problem or degraded path.
    Warn,
    /// A normal lifecycle event; the default level.
    #[default]
    Info,
    /// Diagnostic detail for development.
    Debug,
    /// High-volume tracing detail.
    Trace,
}

impl LogLevel {
    fn verbosity(self) -> u8 {
        match self {
            LogLevel::Error => 1,
            LogLevel::Warn => 2,
            LogLevel::Info => 3,
            LogLevel::Debug => 4,
            LogLevel::Trace => 5,
        }
    }
}

/// Configuration for a [`Logger`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogConfig {
    /// Directory that holds the log files.
    pub directory: PathBuf,
    /// Maximum size of a log file before rotation, in bytes.
    pub max_bytes: u64,
    /// Maximum number of files, including the current file.
    pub max_files: usize,
    /// Minimum severity that is written.
    pub level: LogLevel,
}

impl LogConfig {
    /// Create a configuration with an explicit directory and rotation bounds.
    pub fn new(directory: PathBuf, max_bytes: u64, max_files: usize) -> Self {
        Self {
            directory,
            max_bytes,
            max_files,
            level: LogLevel::Info,
        }
    }

    /// Create a configuration with the default 10 MiB / five-file rotation.
    pub fn with_defaults(directory: PathBuf) -> Self {
        Self::new(directory, DEFAULT_MAX_BYTES, DEFAULT_MAX_FILES)
    }

    /// Set the minimum written severity.
    pub fn with_level(mut self, level: LogLevel) -> Self {
        self.level = level;
        self
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self::with_defaults(std::env::temp_dir().join("oneget-logs"))
    }
}

/// A thread-safe JSON-line logger.
pub struct Logger {
    writer: Mutex<RotatingWriter>,
    level: LogLevel,
}

impl Logger {
    /// Create a logger, creating its directory and current file.
    pub fn new(config: LogConfig) -> Result<Self, AppError> {
        let writer = RotatingWriter::open(
            config.directory,
            LOG_FILE_NAME,
            config.max_bytes,
            config.max_files,
        )?;
        Ok(Self {
            writer: Mutex::new(writer),
            level: config.level,
        })
    }

    /// Append a structured record at `level`.
    ///
    /// Records more verbose than the configured level are ignored. The event
    /// and every string in `fields` are redacted before serialization.
    pub fn log(&self, level: LogLevel, event: &str, mut fields: Value) -> Result<(), AppError> {
        if level.verbosity() > self.level.verbosity() {
            return Ok(());
        }

        let event = redact::redact_text(event);
        redact::redact_json(&mut fields);
        let record = serde_json::json!({
            "timestamp": now_millis(),
            "level": level,
            "event": event,
            "fields": fields,
        });
        let line = serde_json::to_string(&record).map_err(|error| {
            AppError::new("log_serialize", format!("serialize log record: {error}"))
        })?;

        #[cfg(debug_assertions)]
        eprintln!("{line}");

        let mut writer = self.lock_writer()?;
        writer.write_line(&line)
    }

    /// Flush all buffered writes.
    pub fn flush(&self) -> Result<(), AppError> {
        let mut writer = self.lock_writer()?;
        writer.flush()
    }

    /// Path of the current log file.
    pub fn current_path(&self) -> PathBuf {
        match self.writer.lock() {
            Ok(writer) => writer.current_path(),
            Err(poisoned) => poisoned.into_inner().current_path(),
        }
    }

    /// All managed log files, current file first.
    pub fn files(&self) -> Result<Vec<PathBuf>, AppError> {
        let writer = self.lock_writer()?;
        writer.files()
    }

    fn lock_writer(&self) -> Result<MutexGuard<'_, RotatingWriter>, AppError> {
        self.writer
            .lock()
            .map_err(|_| AppError::new("log_lock", "log writer lock is poisoned"))
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}
