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
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

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

/// Interface language preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Language {
    /// Follow the operating-system language; the default.
    #[default]
    #[serde(rename = "system")]
    System,
    /// Simplified Chinese.
    #[serde(rename = "zh-CN")]
    ZhCn,
    /// American English.
    #[serde(rename = "en-US")]
    EnUs,
}

/// What the main window's close button does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum CloseBehavior {
    /// Hide the window and keep running in the tray; the default.
    #[default]
    Tray,
    /// Quit the application.
    Quit,
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
    /// Interface language preference.
    pub language: Language,
    /// Main-window close behavior.
    pub close_behavior: CloseBehavior,
    /// Default directory for new downloads.
    pub default_directory: Option<PathBuf>,
    /// Default parallel connection count for new downloads.
    pub default_connections: u32,
    /// Maximum number of concurrently running tasks.
    pub max_concurrent_tasks: u32,
    /// Optional global download rate limit in bytes per second.
    pub global_rate_limit: Option<u64>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            theme: Theme::System,
            log_level: LogLevel::Info,
            language: Language::System,
            close_behavior: CloseBehavior::Tray,
            default_directory: None,
            default_connections: 8,
            max_concurrent_tasks: 3,
            global_rate_limit: None,
        }
    }
}

/// An in-memory settings document backed by a file.
pub struct SettingsStore {
    path: PathBuf,
    settings: Settings,
    extras: Map<String, Value>,
    read_only: bool,
    recovered: bool,
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
                return Err(AppError::io_error("read settings file", error));
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
                recovered: false,
            });
        }

        let theme = parse_theme(object.get("theme"));
        let log_level = parse_log_level(object.get("logLevel"));
        let language = parse_language(object.get("language"));
        let close_behavior = parse_close_behavior(object.get("closeBehavior"));
        let default_directory = parse_directory(object.get("defaultDirectory"));
        let default_connections = parse_u32(object.get("defaultConnections"), 8);
        let max_concurrent_tasks = parse_u32(object.get("maxConcurrentTasks"), 3);
        let global_rate_limit = object.get("globalRateLimit").and_then(Value::as_u64);

        // Older documents migrate in memory to the current schema. The file is
        // only rewritten when the caller saves, which preserves unknown extras.
        let schema_version = CURRENT_SCHEMA_VERSION;

        for key in [
            "schemaVersion",
            "theme",
            "logLevel",
            "language",
            "closeBehavior",
            "defaultDirectory",
            "defaultConnections",
            "maxConcurrentTasks",
            "globalRateLimit",
        ] {
            object.remove(key);
        }

        Ok(Self {
            path,
            settings: Settings {
                schema_version,
                theme,
                log_level,
                language,
                close_behavior,
                default_directory,
                default_connections,
                max_concurrent_tasks,
                global_rate_limit,
            },
            extras: object,
            read_only: false,
            recovered: false,
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

    /// Whether corrupt input was backed up and defaults were loaded instead.
    pub fn is_recovered(&self) -> bool {
        self.recovered
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
            recovered: false,
        })
    }

    fn recover_corrupt(path: PathBuf, original: &[u8]) -> Result<Self, AppError> {
        let backup = corrupt_backup_path(&path);
        fs::write(&backup, original)
            .map_err(|error| AppError::io_error("back up corrupt settings", error))?;
        let mut store = Self::create_defaults(path)?;
        store.recovered = true;
        Ok(store)
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

fn parse_language(value: Option<&Value>) -> Language {
    match value.and_then(Value::as_str) {
        Some(text) if text.eq_ignore_ascii_case("zh-CN") => Language::ZhCn,
        Some(text) if text.eq_ignore_ascii_case("en-US") => Language::EnUs,
        _ => Language::System,
    }
}

fn parse_close_behavior(value: Option<&Value>) -> CloseBehavior {
    match value.and_then(Value::as_str) {
        Some(text) if text.eq_ignore_ascii_case("quit") => CloseBehavior::Quit,
        _ => CloseBehavior::Tray,
    }
}

fn parse_directory(value: Option<&Value>) -> Option<PathBuf> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(PathBuf::from)
}

fn parse_u32(value: Option<&Value>, fallback: u32) -> u32 {
    value
        .and_then(Value::as_u64)
        .and_then(|raw| u32::try_from(raw).ok())
        .unwrap_or(fallback)
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
    fs::create_dir_all(directory)
        .map_err(|error| AppError::io_error("create settings directory", error))?;

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
        fs::rename(&temporary, path)
            .map_err(|error| AppError::io_error("replace settings file", error))
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
        .map_err(|error| AppError::io_error("create settings temporary file", error))?;
    file.write_all(bytes)
        .map_err(|error| AppError::io_error("write settings temporary file", error))?;
    sync_file(&file)
}

fn sync_file(file: &File) -> Result<(), AppError> {
    file.sync_all()
        .map_err(|error| AppError::io_error("sync settings temporary file", error))
}
