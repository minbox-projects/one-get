//! Typed failures shared by component catalog verification and caching.
//!
//! `code` is a stable identifier safe for callers, logs, and the interface;
//! `message` is a human-readable description. Neither carries key material or
//! artifact data.

use std::fmt;

/// A stable, user-safe component kernel failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KernelError {
    code: String,
    message: String,
}

impl KernelError {
    /// Create an error from a stable code and a human-readable message.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// The stable error code.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// The human-readable error message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for KernelError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for KernelError {}
