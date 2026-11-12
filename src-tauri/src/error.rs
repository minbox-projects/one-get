//! Typed application errors with stable machine-readable codes.
//!
//! Every fallible boundary returns [`AppError`] so the Tauri command layer can
//! serialize a single, user-safe `{ code, message }` shape without leaking
//! internal paths, stack traces, or secret values.

use std::fmt;

use serde::Serialize;

/// A typed application failure.
///
/// The `code` is a stable identifier consumed by the frontend and by tests;
/// the `message` is user-safe and never contains a secret value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppError {
    /// Stable machine-readable error code, for example `settings_read_only`.
    pub code: String,
    /// Human-readable, user-safe description.
    pub message: String,
}

impl AppError {
    /// Create an error with an explicit stable code and message.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// Settings were loaded from a newer schema and must not be overwritten.
    pub fn settings_read_only(message: impl Into<String>) -> Self {
        Self::new("settings_read_only", message)
    }

    /// An operating-system operation failed.
    pub fn io_error(context: impl fmt::Display, error: std::io::Error) -> Self {
        Self::new("io_error", format!("{context}: {error}"))
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for AppError {}
