//! Platform-specific capabilities: application paths, filename safety,
//! capability-scoped filesystem access, guarded child processes, and secret
//! storage.
//!
//! Target-specific branches are confined to this module. Shared application
//! code depends only on the interfaces re-exported here.

pub mod autostart;
pub mod deeplink;
pub mod filename;
pub mod files;
pub mod notifications;
pub mod paths;
pub mod process;
pub mod secrets;
pub mod tray;

pub use filename::{sanitize_filename, FilenameTarget};
pub use files::{CapDir, FileMetadata};
pub use paths::AppPaths;
pub use process::ChildGuard;
pub use secrets::{KeyringStore, MemorySecretStore, SecretError, SecretStore, SecretValue};

use std::fmt;

/// A typed platform failure with a stable machine-readable code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformError {
    /// Stable error code, for example `invalid_path` or `link_escape`.
    pub code: &'static str,
    /// Human-readable, user-safe description.
    pub message: String,
}

impl PlatformError {
    /// Create an error with an explicit stable code.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// A path was malformed or unsafe to use.
    pub fn invalid_path(message: impl Into<String>) -> Self {
        Self::new("invalid_path", message)
    }

    /// A path or link resolved outside the capability root.
    pub fn link_escape(message: impl Into<String>) -> Self {
        Self::new("link_escape", message)
    }

    /// A required file or directory does not exist.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("not_found", message)
    }

    /// An operating-system operation failed.
    pub fn io_error(context: impl fmt::Display, error: std::io::Error) -> Self {
        Self::new("io_error", format!("{context}: {error}"))
    }

    /// The host is missing a required directory or environment location.
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new("unavailable", message)
    }
}

impl fmt::Display for PlatformError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PlatformError {}
