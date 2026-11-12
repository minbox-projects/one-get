//! Persistent application settings.
//!
//! Settings live in a single JSON document. Loading is tolerant: a missing
//! file is created with defaults, corrupt JSON is backed up to a deterministic
//! `.corrupt` sibling before defaults are restored, and a document written by a
//! newer schema is never overwritten. Normal saves preserve unknown top-level
//! fields and replace the file atomically through a same-directory temporary
//! file and rename.

use std::borrow::Borrow;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::error::AppError;
use crate::logging::LogLevel;

/// The settings schema understood by this build.
pub const CURRENT_SCHEMA_VERSION: u32 = 1;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Color theme preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    /// Follow the operating system appearance; the default.
    #[default]
    System,
    /// Always use the light appearance.
    Light,
    /// Always use the dark appearance.
    Dark,
}

/// Persisted settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    /// Settings schema version.
    pub schema_version: u32,
    /// Color theme preference.
    pub theme: Theme,
    /// Minimum log severity.
    pub log_level: LogLevel,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            theme: Theme::System,
            log_level: LogLevel::Info,
        }
    }
}

/// An in-memory settings document backed by a file.
pub struct SettingsStore {
    path: PathBuf,
    settings: Settings,
    extras: Map<String, Value>,
    read_only: bool,
}

impl SettingsStore {
    /// Load settings, creating defaults or recovering from a corrupt file.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, AppError> {
        let path = path.as_ref().to_path_buf();
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Self::create_defaults(path);
            }
            Err(error) => {
                return Err(AppError::io_error(
                    format!("read settings {}", path.display()),
                    error,
                ));
            }
        };

        let parsed = serde_json::from_slice::<Value>(&bytes);
        let mut object = match parsed {
            Ok(Value::Object(object)) => object,
            _ => return Self::recover_corrupt(path, &bytes),
        };

        let schema_version = object
            .get("schemaVersion")
            .and_then(Value::as_u64)
            .unwrap_or(u64::from(CURRENT_SCHEMA_VERSION));

        if schema_version > u64::from(CURRENT_SCHEMA_VERSION) {
            return Ok(Self {
                path,
                settings: Settings::default(),
                extras: Map::new(),
                read_only: true,
            });
        }

        let theme = parse_theme(object.get("theme"));
        let log_level = parse_log_level(object.get("logLevel"));
        let schema_version = schema_version.max(1) as u32;

        for key in ["schemaVersion", "theme", "logLevel"] {
            object.remove(key);
        }

        Ok(Self {
            path,
            settings: Settings {
                schema_version,
                theme,
                log_level,
            },
            extras: object,
            read_only: false,
        })
    }

    /// Current in-memory settings.
    pub fn settings(&self) -> Settings {
        self.settings.clone()
    }

    /// Whether the file came from a newer schema and must not be overwritten.
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Persist `settings`, preserving unknown top-level fields.
    pub fn save<S>(&mut self, settings: S) -> Result<(), AppError>
    where
        S: Borrow<Settings>,
    {
        if self.read_only {
            return Err(AppError::settings_read_only(
                "settings were written by a newer schema and are read-only",
            ));
        }
        let settings = settings.borrow();

        let mut document = self.extras.clone();
        let known = serde_json::to_value(settings)
            .map_err(|error| AppError::new("settings_serialize", error.to_string()))?;
        if let Value::Object(known) = known {
            for (key, value) in known {
                document.insert(key, value);
            }
        }
        let bytes = serde_json::to_vec_pretty(&Value::Object(document))
            .map_err(|error| AppError::new("settings_serialize", error.to_string()))?;

        write_atomic(&self.path, &bytes)?;
        self.settings = settings.clone();
        Ok(())
    }

    fn create_defaults(path: PathBuf) -> Result<Self, AppError> {
        let settings = Settings::default();
        let bytes = serde_json::to_vec_pretty(&settings)
            .map_err(|error| AppError::new("settings_serialize", error.to_string()))?;
        write_atomic(&path, &bytes)?;
        Ok(Self {
            path,
            settings,
            extras: Map::new(),
            read_only: false,
        })
    }

    fn recover_corrupt(path: PathBuf, original: &[u8]) -> Result<Self, AppError> {
        let backup = corrupt_backup_path(&path);
        fs::write(&backup, original).map_err(|error| {
            AppError::io_error(
                format!("back up corrupt settings to {}", backup.display()),
                error,
            )
        })?;
        Self::create_defaults(path)
    }
}

fn parse_theme(value: Option<&Value>) -> Theme {
    match value.and_then(Value::as_str) {
        Some(text) if text.eq_ignore_ascii_case("light") => Theme::Light,
        Some(text) if text.eq_ignore_ascii_case("dark") => Theme::Dark,
        _ => Theme::System,
    }
}

fn parse_log_level(value: Option<&Value>) -> LogLevel {
    match value.and_then(Value::as_str) {
        Some(text) if text.eq_ignore_ascii_case("error") => LogLevel::Error,
        Some(text) if text.eq_ignore_ascii_case("warn") => LogLevel::Warn,
        Some(text) if text.eq_ignore_ascii_case("debug") => LogLevel::Debug,
        Some(text) if text.eq_ignore_ascii_case("trace") => LogLevel::Trace,
        _ => LogLevel::Info,
    }
}

fn corrupt_backup_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings.json");
    path.with_file_name(format!("{file_name}.corrupt"))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    fs::create_dir_all(directory).map_err(|error| {
        AppError::io_error(
            format!("create settings directory {}", directory.display()),
            error,
        )
    })?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("settings.json");
    let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temporary = directory.join(format!(
        ".{file_name}.tmp-{}-{sequence}",
        std::process::id()
    ));

    let write_result = write_temporary(&temporary, bytes).and_then(|()| {
        fs::rename(&temporary, path).map_err(|error| {
            AppError::io_error(format!("replace settings {}", path.display()), error)
        })
    });

    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

fn write_temporary(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .map_err(|error| {
            AppError::io_error(format!("create settings temp {}", path.display()), error)
        })?;
    file.write_all(bytes).map_err(|error| {
        AppError::io_error(format!("write settings temp {}", path.display()), error)
    })?;
    sync_file(&file, path)
}

fn sync_file(file: &File, path: &Path) -> Result<(), AppError> {
    file.sync_all().map_err(|error| {
        AppError::io_error(format!("sync settings temp {}", path.display()), error)
    })
}
