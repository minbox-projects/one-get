//! Admission guards and typed JSON responses for the loopback API.
//!
//! Guards run in a fixed order: header budget, exact loopback Host, route
//! Origin, JSON content type, and bearer authentication. Every rejection is a
//! typed JSON body that never reflects request values.

use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

use super::error::{self, ApiError};
use super::protocol::is_chromium_extension_origin;

/// Maximum accepted request-header count.
pub const MAX_HEADER_COUNT: usize = 64;

/// Maximum accepted combined request-header name and value bytes.
pub const MAX_HEADER_BYTES: usize = 16 * 1024;

/// Reject an oversized header set.
pub(crate) fn check_header_limits(headers: &HeaderMap) -> Result<(), ApiError> {
    if headers.len() > MAX_HEADER_COUNT {
        return Err(error::bad_request(
            "HEADER_LIMIT",
            "request has too many headers",
        ));
    }
    let mut total = 0usize;
    for (name, value) in headers.iter() {
        total = total
            .saturating_add(name.as_str().len())
            .saturating_add(value.as_bytes().len());
        if total > MAX_HEADER_BYTES {
            return Err(error::bad_request(
                "HEADER_LIMIT",
                "request headers are too large",
            ));
        }
    }
    Ok(())
}

/// Require the exact loopback authority.
pub(crate) fn check_host(headers: &HeaderMap, authority: &str) -> Result<(), ApiError> {
    match header_text(headers, header::HOST) {
        Some(host) if host == authority => Ok(()),
        _ => Err(error::forbidden("INVALID_HOST")),
    }
}

/// Allow an absent Origin but reject a present non-extension Origin.
pub(crate) fn check_health_origin(headers: &HeaderMap) -> Result<(), ApiError> {
    match header_text(headers, header::ORIGIN) {
        None => Ok(()),
        Some(origin) if is_chromium_extension_origin(&origin) => Ok(()),
        Some(_) => Err(error::forbidden("INVALID_ORIGIN")),
    }
}

/// Require a Chromium extension Origin and return it.
pub(crate) fn check_write_origin(headers: &HeaderMap) -> Result<String, ApiError> {
    match header_text(headers, header::ORIGIN) {
        Some(origin) if is_chromium_extension_origin(&origin) => Ok(origin),
        _ => Err(error::forbidden("INVALID_ORIGIN")),
    }
}

/// Require a declared JSON content type.
pub(crate) fn check_json_content_type(headers: &HeaderMap) -> Result<(), ApiError> {
    match header_text(headers, header::CONTENT_TYPE) {
        Some(value)
            if value
                .split(';')
                .next()
                .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json")) =>
        {
            Ok(())
        }
        _ => Err(error::unsupported_media_type()),
    }
}

/// Read the bearer token from the Authorization header only.
pub(crate) fn bearer_token(headers: &HeaderMap) -> Result<String, ApiError> {
    let value = header_text(headers, header::AUTHORIZATION)
        .ok_or_else(|| error::unauthorized("UNAUTHORIZED"))?;
    let mut parts = value.splitn(2, ' ');
    match (parts.next(), parts.next()) {
        (Some(scheme), Some(token))
            if scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() =>
        {
            Ok(token.to_string())
        }
        _ => Err(error::unauthorized("UNAUTHORIZED")),
    }
}

/// Build a typed JSON error response with an optional `Retry-After` header.
pub(crate) fn error_response(error: ApiError, retry_after_secs: Option<u64>) -> Response {
    let status = StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let body = Json(json!({ "code": error.code, "message": error.message }));
    let mut response = (status, body).into_response();
    if let Some(seconds) = retry_after_secs {
        if let Ok(value) = HeaderValue::from_str(&seconds.max(1).to_string()) {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
    }
    response
}

/// Build a rate-limited response carrying `Retry-After`.
pub(crate) fn rate_limited(retry_after_secs: u64) -> Response {
    error_response(
        error::too_many_requests("RATE_LIMITED"),
        Some(retry_after_secs),
    )
}

/// Build a successful typed JSON response.
pub(crate) fn json_ok(value: Value) -> Response {
    (StatusCode::OK, Json(value)).into_response()
}

fn header_text(headers: &HeaderMap, name: header::HeaderName) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}
