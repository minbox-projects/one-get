//! Typed engine failures with stable machine-readable codes.
//!
//! The engine and the durable store share this error type so a caller can
//! branch on a stable code without parsing a human-readable message. Messages
//! are user-safe and never contain internal filesystem paths.

use std::fmt;
use std::time::Duration;

use crate::error::AppError;

/// A typed engine or store failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineError {
    code: String,
    message: String,
    retryable: bool,
    retry_after: Option<Duration>,
}

impl EngineError {
    /// A failure that must not be retried automatically.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
            retry_after: None,
        }
    }

    /// A transient failure that a bounded retry policy may attempt again.
    pub fn retryable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: true,
            retry_after: None,
        }
    }

    /// Attach the delay advertised by an integer-seconds `Retry-After` header.
    pub fn with_retry_after(mut self, delay: Duration) -> Self {
        self.retry_after = Some(delay);
        self
    }

    /// Stable machine-readable code.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Whether a bounded retry policy may try the operation again.
    pub fn is_retryable(&self) -> bool {
        self.retryable
    }

    /// Delay a retry should honor before the next attempt, when advertised.
    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    /// The destination device ran out of free space during a write.
    pub fn disk_full() -> Self {
        Self::new(
            "DISK_FULL",
            "the destination device does not have enough free space",
        )
    }

    /// A task or segment transition was not permitted by the state machine.
    pub fn invalid_state(message: impl Into<String>) -> Self {
        Self::new("INVALID_STATE", message)
    }

    /// A segment range was empty, reversed, or otherwise unusable.
    pub fn invalid_segment_range(message: impl Into<String>) -> Self {
        Self::new("INVALID_SEGMENT_RANGE", message)
    }

    /// The durable store could not complete an operation.
    pub fn database(message: impl Into<String>) -> Self {
        Self::new("DATABASE_ERROR", message)
    }

    /// The database was written by a schema newer than this build supports.
    pub fn schema_too_new(version: i64) -> Self {
        Self::new(
            "SCHEMA_TOO_NEW",
            format!("the task database schema version {version} is newer than supported"),
        )
    }
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for EngineError {}

impl From<rusqlite::Error> for EngineError {
    fn from(_: rusqlite::Error) -> Self {
        Self::database("the task store could not complete the operation")
    }
}

impl From<std::io::Error> for EngineError {
    fn from(_: std::io::Error) -> Self {
        Self::new("IO_ERROR", "a required task-store file operation failed")
    }
}

impl From<EngineError> for AppError {
    fn from(error: EngineError) -> Self {
        AppError::new(error.code, error.message)
    }
}

impl From<AppError> for EngineError {
    fn from(error: AppError) -> Self {
        Self::new(error.code, error.message)
    }
}
