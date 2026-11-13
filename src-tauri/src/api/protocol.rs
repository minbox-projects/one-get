//! Capture protocol types, constants, and admission validation.
//!
//! The protocol is deliberately small and defensive: it accepts only the safe
//! metadata a browser can forward without credentials, and every per-item
//! problem is reported with a stable code instead of failing the whole batch.

use serde::{Deserialize, Serialize};
use url::Url;

use super::error::{self, ApiError};

/// Capture protocol version understood by this build.
pub const API_PROTOCOL_VERSION: u32 = 1;

/// Loopback host the service binds to.
pub const API_HOST: &str = "127.0.0.1";

/// Default loopback port.
pub const API_PORT: u16 = 37_652;

/// Maximum accepted request-body size.
pub const MAX_BODY_BYTES: usize = 65_536;

/// Maximum accepted capture items in one request.
pub const MAX_CAPTURE_ITEMS: usize = 50;

/// Maximum accepted URL length.
pub const MAX_URL_BYTES: usize = 8_192;

/// Maximum accepted capture header count.
pub const MAX_CAPTURE_HEADERS: usize = 10;

/// Maximum accepted capture header-name length.
pub const MAX_HEADER_NAME_BYTES: usize = 128;

/// Maximum accepted capture header-value length.
pub const MAX_HEADER_VALUE_BYTES: usize = 2_048;

/// Maximum accepted capture filename length.
pub const MAX_FILENAME_BYTES: usize = 255;

/// Header names a capture may carry, normalized to lowercase.
pub const SAFE_CAPTURE_HEADERS: [&str; 4] = ["referer", "user-agent", "accept", "accept-language"];

const CHROMIUM_EXTENSION_ORIGIN_PREFIX: &str = "chrome-extension://";
const CHROMIUM_EXTENSION_ID_LEN: usize = 32;
const FIREFOX_EXTENSION_ORIGIN_PREFIX: &str = "moz-extension://";
const FIREFOX_UUID_LEN: usize = 36;

/// Why a capture request intends to use the captured items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureIntent {
    /// Replace a browser download with a desktop task.
    Takeover,
    /// Prefill a download dialog without creating a task.
    Prefill,
}

/// The kind of resource being captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureKind {
    /// An ordinary downloadable file.
    File,
    /// An HLS manifest, unsupported for direct download.
    Hls,
    /// A DASH manifest, unsupported for direct download.
    Dash,
    /// A platform-managed video resource, unsupported in this build.
    VideoPlatform,
}

/// One captured resource and its safe metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureItem {
    /// Absolute HTTP or HTTPS target URL.
    pub url: String,
    /// Suggested filename, when the browser exposed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// Page URL that initiated the resource.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub referer: Option<String>,
    /// Response MIME type, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
    /// Known resource size in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    /// Resource kind; absence means an ordinary file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<CaptureKind>,
    /// Safe request headers to replay with the download.
    #[serde(default)]
    pub headers: Vec<CaptureHeader>,
}

/// A safe request header forwarded with a capture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureHeader {
    /// Header name.
    pub name: String,
    /// Header value.
    pub value: String,
}

/// A complete capture request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureRequest {
    /// Capture protocol version.
    pub protocol_version: u32,
    /// Intended use of the captured items.
    pub intent: CaptureIntent,
    /// Browser-stable identifier for duplicate suppression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    /// Captured items.
    #[serde(default)]
    pub items: Vec<CaptureItem>,
}

/// A per-item validation rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureItemError {
    /// Zero-based index of the rejected item.
    pub index: usize,
    /// Stable per-item rejection code.
    pub code: &'static str,
}

/// The accepted items and per-item rejections for a capture request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CaptureValidation {
    /// Items that passed validation.
    pub accepted: Vec<CaptureItem>,
    /// Items that failed validation, in request order.
    pub rejected: Vec<CaptureItemError>,
}

/// Validate a capture request.
///
/// Request-level problems (protocol, emptiness, item count, takeover arity)
/// return an [`ApiError`]; problems with individual items are reported in
/// [`CaptureValidation`] so the remaining items can still be used.
pub fn validate_capture_request(request: &CaptureRequest) -> Result<CaptureValidation, ApiError> {
    if request.protocol_version != API_PROTOCOL_VERSION {
        return Err(error::conflict("PROTOCOL_MISMATCH"));
    }
    if request.items.is_empty() {
        return Err(error::bad_request(
            "EMPTY_CAPTURE",
            "capture contains no items",
        ));
    }
    if request.items.len() > MAX_CAPTURE_ITEMS {
        return Err(error::bad_request(
            "TOO_MANY_ITEMS",
            "capture contains too many items",
        ));
    }
    if matches!(request.intent, CaptureIntent::Takeover) && request.items.len() != 1 {
        return Err(error::bad_request(
            "TAKEOVER_ITEM_COUNT",
            "a takeover capture must contain exactly one item",
        ));
    }

    let mut validation = CaptureValidation::default();
    for (index, item) in request.items.iter().enumerate() {
        match validate_item(item) {
            Ok(()) => validation.accepted.push(item.clone()),
            Err(code) => validation.rejected.push(CaptureItemError { index, code }),
        }
    }
    Ok(validation)
}

/// Whether `origin` is an exact Chromium extension origin.
pub fn is_chromium_extension_origin(origin: &str) -> bool {
    match origin.strip_prefix(CHROMIUM_EXTENSION_ORIGIN_PREFIX) {
        Some(id) => {
            id.len() == CHROMIUM_EXTENSION_ID_LEN
                && id.bytes().all(|byte| (b'a'..=b'p').contains(&byte))
        }
        None => false,
    }
}

/// Whether `origin` is an exact Chromium or Firefox extension origin.
///
/// A Chromium origin is the scheme plus 32 `a`-`p` characters. A Firefox
/// origin is the scheme plus a lowercase UUID in canonical 8-4-4-4-12 form
/// with no path, query, fragment, or uppercase digits.
pub fn is_browser_extension_origin(origin: &str) -> bool {
    if is_chromium_extension_origin(origin) {
        return true;
    }
    match origin.strip_prefix(FIREFOX_EXTENSION_ORIGIN_PREFIX) {
        Some(id) => is_lowercase_uuid(id),
        None => false,
    }
}

fn is_lowercase_uuid(value: &str) -> bool {
    if value.len() != FIREFOX_UUID_LEN {
        return false;
    }
    value.bytes().enumerate().all(|(index, byte)| match index {
        8 | 13 | 18 | 23 => byte == b'-',
        _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
    })
}

/// The exact loopback authority for a port.
pub fn loopback_authority(port: u16) -> String {
    format!("{API_HOST}:{port}")
}

enum UrlProblem {
    Invalid,
    Credentialed,
}

fn validate_item(item: &CaptureItem) -> Result<(), &'static str> {
    match http_url_problem(&item.url) {
        None => {}
        Some(UrlProblem::Credentialed) => return Err("CREDENTIALED_URL"),
        Some(UrlProblem::Invalid) => return Err("INVALID_URL"),
    }
    if let Some(filename) = &item.filename {
        validate_filename(filename)?;
    }
    if let Some(referer) = &item.referer {
        if http_url_problem(referer).is_some() {
            return Err("INVALID_REFERER");
        }
    }
    validate_headers(&item.headers)?;
    match item.kind {
        None | Some(CaptureKind::File) => Ok(()),
        Some(CaptureKind::Hls | CaptureKind::Dash | CaptureKind::VideoPlatform) => {
            Err("UNSUPPORTED_KIND")
        }
    }
}

fn http_url_problem(value: &str) -> Option<UrlProblem> {
    if value.len() > MAX_URL_BYTES || value.chars().any(char::is_control) {
        return Some(UrlProblem::Invalid);
    }
    let parsed = match Url::parse(value) {
        Ok(parsed) => parsed,
        Err(_) => return Some(UrlProblem::Invalid),
    };
    if !matches!(parsed.scheme(), "http" | "https") || !parsed.has_host() {
        return Some(UrlProblem::Invalid);
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Some(UrlProblem::Credentialed);
    }
    None
}

fn validate_filename(filename: &str) -> Result<(), &'static str> {
    if filename.is_empty()
        || filename == "."
        || filename == ".."
        || filename.len() > MAX_FILENAME_BYTES
        || filename.contains('/')
        || filename.contains('\\')
        || filename.chars().any(char::is_control)
    {
        return Err("UNSAFE_FILENAME");
    }
    Ok(())
}

fn validate_headers(headers: &[CaptureHeader]) -> Result<(), &'static str> {
    if headers.len() > MAX_CAPTURE_HEADERS {
        return Err("HEADER_LIMIT");
    }

    let mut seen: Vec<String> = Vec::with_capacity(headers.len());
    for header in headers {
        if header.name.len() > MAX_HEADER_NAME_BYTES || header.value.len() > MAX_HEADER_VALUE_BYTES
        {
            return Err("HEADER_LIMIT");
        }
        if header.name.chars().any(char::is_control) || header.value.chars().any(char::is_control) {
            return Err("FORBIDDEN_HEADER");
        }
        let normalized = header.name.to_ascii_lowercase();
        if !SAFE_CAPTURE_HEADERS.contains(&normalized.as_str()) {
            return Err("FORBIDDEN_HEADER");
        }
        if seen.iter().any(|existing| existing == &normalized) {
            return Err("DUPLICATE_HEADER");
        }
        seen.push(normalized);
    }
    Ok(())
}
