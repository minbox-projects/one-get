//! Per-user application directories.

use std::path::{Path, PathBuf};

use super::PlatformError;

/// Tauri application identifier used for the macOS directory roots.
pub const APP_IDENTIFIER: &str = "com.oneget.app";

/// Resolved application directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    /// Durable application data.
    pub data: PathBuf,
    /// Disposable cache data.
    pub cache: PathBuf,
    /// Log files.
    pub logs: PathBuf,
    /// Default download destination.
    pub downloads: PathBuf,
    /// Backup files kept beside application data.
    pub backups: PathBuf,
}

impl AppPaths {
    /// Build paths beneath an explicit root and create every directory.
    ///
    /// `root` may be relative; the returned paths are absolute and
    /// canonicalized.
    pub fn with_root(root: impl AsRef<Path>) -> Result<Self, PlatformError> {
        let root = root.as_ref();
        std::fs::create_dir_all(root).map_err(|error| {
            PlatformError::io_error(format!("create app root {}", root.display()), error)
        })?;
        let root = std::fs::canonicalize(root).map_err(|error| {
            PlatformError::io_error(format!("canonicalize app root {}", root.display()), error)
        })?;

        let paths = Self {
            data: root.join("data"),
            cache: root.join("cache"),
            logs: root.join("logs"),
            downloads: root.join("downloads"),
            backups: root.join("backups"),
        };
        paths.ensure()?;
        Ok(paths)
    }

    /// Resolve the platform's per-user application directories.
    pub fn for_user() -> Result<Self, PlatformError> {
        user_paths()
    }

    /// Create all directories, tolerating ones that already exist.
    pub fn ensure(&self) -> Result<(), PlatformError> {
        for (label, dir) in [
            ("data", &self.data),
            ("cache", &self.cache),
            ("logs", &self.logs),
            ("downloads", &self.downloads),
            ("backups", &self.backups),
        ] {
            std::fs::create_dir_all(dir).map_err(|error| {
                PlatformError::io_error(
                    format!("create {label} directory {}", dir.display()),
                    error,
                )
            })?;
        }
        Ok(())
    }
}

fn env_path(name: &str) -> Result<PathBuf, PlatformError> {
    match std::env::var_os(name) {
        Some(value) if !value.is_empty() => Ok(PathBuf::from(value)),
        _ => Err(PlatformError::unavailable(format!(
            "required environment variable {name} is not set"
        ))),
    }
}

#[cfg(target_os = "macos")]
fn user_paths() -> Result<AppPaths, PlatformError> {
    let home = env_path("HOME")?;
    let data = home
        .join("Library")
        .join("Application Support")
        .join(APP_IDENTIFIER);
    let cache = home.join("Library").join("Caches").join(APP_IDENTIFIER);
    let logs = home.join("Library").join("Logs").join(APP_IDENTIFIER);
    let downloads = home.join("Downloads");
    let backups = data.join("backups");
    Ok(AppPaths {
        data,
        cache,
        logs,
        downloads,
        backups,
    })
}

#[cfg(target_os = "windows")]
fn user_paths() -> Result<AppPaths, PlatformError> {
    let appdata = env_path("APPDATA")?;
    let local_appdata = env_path("LOCALAPPDATA")?;
    let home = env_path("USERPROFILE")?;

    let data = appdata.join("OneGet");
    let cache = local_appdata.join("OneGet").join("Cache");
    let logs = local_appdata.join("OneGet").join("Logs");
    let downloads = home.join("Downloads");
    let backups = data.join("backups");
    Ok(AppPaths {
        data,
        cache,
        logs,
        downloads,
        backups,
    })
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn user_paths() -> Result<AppPaths, PlatformError> {
    let home = env_path("HOME")?;
    let data_home = match std::env::var_os("XDG_DATA_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => home.join(".local").join("share"),
    };
    let cache_home = match std::env::var_os("XDG_CACHE_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value),
        _ => home.join(".cache"),
    };

    let data = data_home.join("oneget");
    let cache = cache_home.join("oneget");
    let logs = data.join("logs");
    let downloads = home.join("Downloads");
    let backups = data.join("backups");
    Ok(AppPaths {
        data,
        cache,
        logs,
        downloads,
        backups,
    })
}
