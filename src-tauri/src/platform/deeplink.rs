//! `oneget://` deep-link parsing and runtime scheme registration.
//!
//! Only `oneget://download?url=<http-or-https-url>` is accepted. The outer
//! scheme and host are validated, exactly one `url` query parameter is
//! required, the raw query is checked for malformed percent escapes, and the
//! inner URL must use `http` or `https` without embedded credentials.

use serde::Serialize;

use crate::error::AppError;

/// Maximum accepted deep-link length in bytes.
pub const MAX_DEEP_LINK_BYTES: usize = 8192;

/// The action requested by a deep link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeepLinkIntent {
    /// Prefill a new download from the validated URL.
    Download,
}

/// Result of handling a deep link, shared with the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeepLinkEvent {
    /// Whether the link was accepted.
    pub accepted: bool,
    /// The validated inner URL when accepted.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub url: String,
    /// A typed error when rejected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<AppError>,
}

impl DeepLinkEvent {
    /// Build an accepted event around a validated URL.
    pub fn accepted(url: String) -> Self {
        Self {
            accepted: true,
            url,
            error: None,
        }
    }

    /// Build a rejected event with a stable error code and message.
    pub fn rejected(code: String, message: String) -> Self {
        Self {
            accepted: false,
            url: String::new(),
            error: Some(AppError::new(code, message)),
        }
    }

    /// The requested intent, or `None` for a rejected event.
    pub fn intent(&self) -> Option<DeepLinkIntent> {
        self.accepted.then_some(DeepLinkIntent::Download)
    }
}

/// Parse and validate a deep link.
pub fn parse_deep_link(raw: &str) -> Result<DeepLinkEvent, AppError> {
    if raw.len() > MAX_DEEP_LINK_BYTES {
        return Err(AppError::new(
            "deep_link_too_long",
            "deep link exceeds the maximum supported length",
        ));
    }

    let (scheme, rest) = raw.split_once("://").ok_or_else(malformed_link)?;
    if !scheme.eq_ignore_ascii_case("oneget") {
        return Err(AppError::new(
            "unsupported_deep_link_scheme",
            "only oneget:// links are supported",
        ));
    }

    let rest = rest.split('#').next().unwrap_or(rest);
    let (authority, query) = match rest.split_once('?') {
        Some((authority, query)) => (authority, Some(query)),
        None => (rest, None),
    };

    if !authority.eq_ignore_ascii_case("download") {
        return Err(AppError::new(
            "unsupported_deep_link_host",
            "deep link host must be download",
        ));
    }

    let query = query.ok_or_else(|| {
        AppError::new(
            "missing_url_parameter",
            "deep link is missing the url parameter",
        )
    })?;

    validate_percent_encoding(query)?;

    let mut inner_url = None;
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key_raw, value_raw) = pair.split_once('=').unwrap_or((pair, ""));
        let key = percent_decode(key_raw)?;
        if key != "url" {
            continue;
        }
        if inner_url.is_some() {
            return Err(AppError::new(
                "duplicate_url_parameter",
                "deep link must contain exactly one url parameter",
            ));
        }
        let value = percent_decode(value_raw)?;
        if value.is_empty() {
            return Err(AppError::new(
                "empty_url_parameter",
                "deep link url parameter must not be empty",
            ));
        }
        inner_url = Some(value);
    }

    let inner_url = inner_url.ok_or_else(|| {
        AppError::new(
            "missing_url_parameter",
            "deep link is missing the url parameter",
        )
    })?;

    let parsed = url::Url::parse(&inner_url).map_err(|_| {
        AppError::new(
            "invalid_url_parameter",
            "deep link url parameter is not a valid URL",
        )
    })?;

    match parsed.scheme() {
        "http" | "https" => {}
        _ => {
            return Err(AppError::new(
                "unsupported_url_scheme",
                "deep link url parameter must use http or https",
            ))
        }
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(AppError::new(
            "embedded_credentials",
            "deep link url parameter must not contain credentials",
        ));
    }

    Ok(DeepLinkEvent::accepted(inner_url))
}

/// Register every configured URL scheme at runtime on Windows.
#[cfg(target_os = "windows")]
pub fn register_schemes(app: &tauri::AppHandle) -> Result<(), AppError> {
    use tauri_plugin_deep_link::DeepLinkExt;

    app.deep_link().register_all().map_err(|error| {
        AppError::new(
            "deep_link_register_failed",
            format!("register deep link schemes: {error}"),
        )
    })
}

/// Other platforms register schemes from the bundle configuration.
#[cfg(not(target_os = "windows"))]
pub fn register_schemes(_app: &tauri::AppHandle) -> Result<(), AppError> {
    Ok(())
}

fn malformed_link() -> AppError {
    AppError::new("invalid_deep_link", "deep link is not a valid oneget link")
}

fn validate_percent_encoding(input: &str) -> Result<(), AppError> {
    let bytes = input.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len()
                || !bytes[index + 1].is_ascii_hexdigit()
                || !bytes[index + 2].is_ascii_hexdigit()
            {
                return Err(invalid_encoding());
            }
            index += 3;
        } else {
            index += 1;
        }
    }
    Ok(())
}

fn percent_decode(input: &str) -> Result<String, AppError> {
    let bytes = input.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(invalid_encoding());
            }
            let high = hex_value(bytes[index + 1]).ok_or_else(invalid_encoding)?;
            let low = hex_value(bytes[index + 2]).ok_or_else(invalid_encoding)?;
            decoded.push(high * 16 + low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| invalid_encoding())
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn invalid_encoding() -> AppError {
    AppError::new(
        "invalid_url_encoding",
        "deep link contains malformed percent encoding",
    )
}
