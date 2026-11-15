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

/// Global proxy selection mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProxyMode {
    /// Connect without a proxy; the default.
    #[default]
    Direct,
    /// Use the operating-system proxy configuration.
    System,
    /// Use the configured host and port.
    Manual,
}

/// Protocol used by a manual proxy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProxyType {
    /// Plain HTTP proxy; the default.
    #[default]
    Http,
    /// HTTPS proxy.
    Https,
    /// SOCKS5 proxy.
    Socks5,
}

/// Preferred quality when the video quality dialog is not shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum VideoQualityPreference {
    /// Prefer the highest quality that plays in MP4; the default.
    #[default]
    #[serde(rename = "mp4Compatible")]
    Mp4Compatible,
    /// Prefer the highest available quality.
    #[serde(rename = "highest")]
    Highest,
}

const DEFAULT_CONNECT_TIMEOUT_SECS: u32 = 15;
const DEFAULT_READ_TIMEOUT_SECS: u32 = 30;
const DEFAULT_RETRY_ATTEMPTS: u32 = 5;
const DEFAULT_HF_CONNECTION_CAP: u32 = 8;

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
    /// Global proxy selection mode.
    #[serde(default)]
    pub proxy_mode: ProxyMode,
    /// Protocol used by a manual proxy.
    #[serde(default)]
    pub proxy_type: ProxyType,
    /// Host of a manual proxy.
    #[serde(default)]
    pub proxy_host: Option<String>,
    /// Port of a manual proxy.
    #[serde(default)]
    pub proxy_port: Option<u16>,
    /// Default Netscape cookies file for new tasks.
    #[serde(default)]
    pub default_cookies_file: Option<PathBuf>,
    /// Fallback User-Agent for requests without a per-task value.
    #[serde(default)]
    pub user_agent: Option<String>,
    /// Connect timeout for HTTP and HuggingFace requests, in seconds.
    #[serde(default = "default_connect_timeout_secs")]
    pub connect_timeout_secs: u32,
    /// Read timeout for HTTP and HuggingFace requests, in seconds.
    #[serde(default = "default_read_timeout_secs")]
    pub read_timeout_secs: u32,
    /// Retry attempts for HTTP and HuggingFace requests.
    #[serde(default = "default_retry_attempts")]
    pub retry_attempts: u32,
    /// Maximum connection count applied to a HuggingFace task.
    #[serde(default = "default_hf_connection_cap")]
    pub hf_connection_cap: u32,
    /// Whether parsing a video always opens the quality dialog first.
    #[serde(default = "default_always_ask_quality")]
    pub always_ask_quality: bool,
    /// Preferred quality when the quality dialog is skipped.
    #[serde(default)]
    pub video_quality_preference: VideoQualityPreference,
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
            proxy_mode: ProxyMode::Direct,
            proxy_type: ProxyType::Http,
            proxy_host: None,
            proxy_port: None,
            default_cookies_file: None,
            user_agent: None,
            connect_timeout_secs: DEFAULT_CONNECT_TIMEOUT_SECS,
            read_timeout_secs: DEFAULT_READ_TIMEOUT_SECS,
            retry_attempts: DEFAULT_RETRY_ATTEMPTS,
            hf_connection_cap: DEFAULT_HF_CONNECTION_CAP,
            always_ask_quality: true,
            video_quality_preference: VideoQualityPreference::Mp4Compatible,
        }
    }
}

impl Settings {
    /// Reject values that cannot be applied to the download engine.
    ///
    /// The messages are fixed and never echo a user-supplied value.
    pub fn validate(&self) -> Result<(), AppError> {
        if matches!(self.proxy_mode, ProxyMode::Manual) {
            let has_host = self
                .proxy_host
                .as_deref()
                .is_some_and(|host| !host.is_empty());
            if !has_host {
                return Err(invalid_settings(
                    "manual proxy mode requires a non-empty proxy host",
                ));
            }
            let has_port = self
                .proxy_port
                .is_some_and(|port| (1..=u16::MAX).contains(&port));
            if !has_port {
                return Err(invalid_settings(
                    "manual proxy mode requires a port between 1 and 65535",
                ));
            }
        }
        if !(1..=64).contains(&self.hf_connection_cap) {
            return Err(invalid_settings(
                "the HuggingFace connection cap must be between 1 and 64",
            ));
        }
        if !(5..=120).contains(&self.connect_timeout_secs) {
            return Err(invalid_settings(
                "the connect timeout must be between 5 and 120 seconds",
            ));
        }
        if !(5..=300).contains(&self.read_timeout_secs) {
            return Err(invalid_settings(
                "the read timeout must be between 5 and 300 seconds",
            ));
        }
        if !(1..=10).contains(&self.retry_attempts) {
            return Err(invalid_settings(
                "the retry attempt count must be between 1 and 10",
            ));
        }
        if let Some(agent) = &self.user_agent {
            if agent.is_empty() || agent.contains('\r') || agent.contains('\n') {
                return Err(invalid_settings(
                    "the User-Agent must be non-empty and free of line breaks",
                ));
            }
        }
        Ok(())
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
        let proxy_mode = parse_proxy_mode(object.get("proxyMode"));
        let proxy_type = parse_proxy_type(object.get("proxyType"));
        let proxy_host = parse_optional_string(object.get("proxyHost"));
        let proxy_port = parse_u16(object.get("proxyPort"));
        let default_cookies_file = parse_directory(object.get("defaultCookiesFile"));
        let user_agent = parse_optional_string(object.get("userAgent"));
        let connect_timeout_secs = parse_u32(
            object.get("connectTimeoutSecs"),
            DEFAULT_CONNECT_TIMEOUT_SECS,
        );
        let read_timeout_secs = parse_u32(object.get("readTimeoutSecs"), DEFAULT_READ_TIMEOUT_SECS);
        let retry_attempts = parse_u32(object.get("retryAttempts"), DEFAULT_RETRY_ATTEMPTS);
        let hf_connection_cap = parse_u32(object.get("hfConnectionCap"), DEFAULT_HF_CONNECTION_CAP);
        let always_ask_quality = parse_bool(object.get("alwaysAskQuality"), true);
        let video_quality_preference =
            parse_video_quality_preference(object.get("videoQualityPreference"));

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
            "proxyMode",
            "proxyType",
            "proxyHost",
            "proxyPort",
            "defaultCookiesFile",
            "userAgent",
            "connectTimeoutSecs",
            "readTimeoutSecs",
            "retryAttempts",
            "hfConnectionCap",
            "alwaysAskQuality",
            "videoQualityPreference",
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
                proxy_mode,
                proxy_type,
                proxy_host,
                proxy_port,
                default_cookies_file,
                user_agent,
                connect_timeout_secs,
                read_timeout_secs,
                retry_attempts,
                hf_connection_cap,
                always_ask_quality,
                video_quality_preference,
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

fn parse_optional_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn parse_u32(value: Option<&Value>, fallback: u32) -> u32 {
    value
        .and_then(Value::as_u64)
        .and_then(|raw| u32::try_from(raw).ok())
        .unwrap_or(fallback)
}

fn parse_u16(value: Option<&Value>) -> Option<u16> {
    value
        .and_then(Value::as_u64)
        .and_then(|raw| u16::try_from(raw).ok())
}

fn parse_bool(value: Option<&Value>, fallback: bool) -> bool {
    value.and_then(Value::as_bool).unwrap_or(fallback)
}

fn parse_proxy_mode(value: Option<&Value>) -> ProxyMode {
    match value.and_then(Value::as_str) {
        Some(text) if text.eq_ignore_ascii_case("system") => ProxyMode::System,
        Some(text) if text.eq_ignore_ascii_case("manual") => ProxyMode::Manual,
        _ => ProxyMode::Direct,
    }
}

fn parse_proxy_type(value: Option<&Value>) -> ProxyType {
    match value.and_then(Value::as_str) {
        Some(text) if text.eq_ignore_ascii_case("https") => ProxyType::Https,
        Some(text) if text.eq_ignore_ascii_case("socks5") => ProxyType::Socks5,
        _ => ProxyType::Http,
    }
}

fn parse_video_quality_preference(value: Option<&Value>) -> VideoQualityPreference {
    match value.and_then(Value::as_str) {
        Some(text) if text.eq_ignore_ascii_case("highest") => VideoQualityPreference::Highest,
        _ => VideoQualityPreference::Mp4Compatible,
    }
}

fn default_connect_timeout_secs() -> u32 {
    DEFAULT_CONNECT_TIMEOUT_SECS
}

fn default_read_timeout_secs() -> u32 {
    DEFAULT_READ_TIMEOUT_SECS
}

fn default_retry_attempts() -> u32 {
    DEFAULT_RETRY_ATTEMPTS
}

fn default_hf_connection_cap() -> u32 {
    DEFAULT_HF_CONNECTION_CAP
}

fn default_always_ask_quality() -> bool {
    true
}

fn invalid_settings(message: &str) -> AppError {
    AppError::new("INVALID_SETTINGS", message)
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
