//! Typed failures for video probing and subprocess execution.
//!
//! `code` is a stable identifier safe for callers, logs, and the interface;
//! `message` is a human-readable description. Neither carries credential
//! material or unbounded child output.

use std::fmt;

/// A stable, user-safe video failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoError {
    code: String,
    message: String,
}

impl VideoError {
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

    /// The process exceeded its probe, startup, or inactivity timeout and its
    /// whole tree was terminated.
    pub fn timeout() -> Self {
        Self::new(
            "VIDEO_TIMEOUT",
            "the video process did not finish within its allowed time",
        )
    }

    /// The process produced more output than the combined capture allows.
    pub fn output_limit() -> Self {
        Self::new(
            "VIDEO_OUTPUT_LIMIT",
            "the video process produced more output than allowed",
        )
    }

    /// The downloader reported that no supported JavaScript runtime is
    /// available.
    pub fn js_runtime_missing() -> Self {
        Self::new(
            "VIDEO_JS_RUNTIME_MISSING",
            "a supported JavaScript runtime is required but none is available",
        )
    }

    /// Whether this is a generic process failure whose bounded output tail
    /// reports that no supported JavaScript runtime is available.
    pub fn is_missing_js_runtime(&self) -> bool {
        self.code == "VIDEO_PROCESS_FAILED"
            && self
                .message
                .to_ascii_lowercase()
                .contains("javascript runtime")
    }

    /// The process could not be started at all.
    pub fn spawn_failed() -> Self {
        Self::new(
            "VIDEO_SPAWN_FAILED",
            "the video process could not be started",
        )
    }

    /// The process exited unsuccessfully or its status could not be observed.
    ///
    /// `detail` is an already bounded, user-safe tail of the child output.
    pub fn process_failed(detail: impl Into<String>) -> Self {
        let detail = detail.into();
        if detail.trim().is_empty() {
            Self::new(
                "VIDEO_PROCESS_FAILED",
                "the video process exited unsuccessfully",
            )
        } else {
            Self::new(
                "VIDEO_PROCESS_FAILED",
                format!("the video process exited unsuccessfully: {detail}"),
            )
        }
    }
}

impl fmt::Display for VideoError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for VideoError {}
