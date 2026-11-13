//! Typed failures for the loopback browser-integration API.
//!
//! Every failure carries an HTTP status, a stable machine-readable code, and a
//! user-safe message. Messages never carry submitted codes, tokens, headers, or
//! request bodies.

use std::fmt;

/// A typed API failure with a status, stable code, and safe message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    /// HTTP status code the service should return.
    pub status: u16,
    /// Stable SCREAMING_SNAKE machine-readable code.
    pub code: String,
    /// User-safe description that never contains a secret value.
    pub message: String,
}

impl ApiError {
    /// Create an error with an explicit status, stable code, and safe message.
    pub fn new(status: u16, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            status,
            code: code.into(),
            message: message.into(),
        }
    }
}

/// A malformed request or invalid input.
pub fn bad_request(code: impl Into<String>, message: impl Into<String>) -> ApiError {
    ApiError::new(400, code, message)
}

/// Missing or invalid credentials.
pub fn unauthorized(code: impl Into<String>) -> ApiError {
    ApiError::new(401, code, "request is not authorized")
}

/// A request that is understood but refused.
pub fn forbidden(code: impl Into<String>) -> ApiError {
    ApiError::new(403, code, "request is not permitted")
}

/// A request body whose content type is not supported.
pub fn unsupported_media_type() -> ApiError {
    ApiError::new(
        415,
        "UNSUPPORTED_MEDIA_TYPE",
        "request content type must be application/json",
    )
}

/// A request body that exceeds the configured limit.
pub fn payload_too_large() -> ApiError {
    ApiError::new(413, "PAYLOAD_TOO_LARGE", "request body is too large")
}

/// A request that conflicts with the current protocol or state.
pub fn conflict(code: impl Into<String>) -> ApiError {
    ApiError::new(409, code, "request conflicts with the current state")
}

/// A caller that exceeded its request budget.
pub fn too_many_requests(code: impl Into<String>) -> ApiError {
    ApiError::new(429, code, "request limit exceeded")
}

/// A dependency that is unavailable or not usable.
pub fn service_unavailable(code: impl Into<String>) -> ApiError {
    ApiError::new(503, code, "service is temporarily unavailable")
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}
