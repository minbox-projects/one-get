//! Non-destructive HTTP probing for remote download metadata.
//!
//! A probe sends a single ranged `GET` and classifies the response headers. It
//! never reads or persists a response body, so an HTML login page or a large
//! file cannot consume memory or disk before the caller decides to download.

use percent_encoding::percent_decode_str;
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT_ENCODING, CONTENT_DISPOSITION, CONTENT_RANGE,
    CONTENT_TYPE, ETAG, LAST_MODIFIED, RANGE,
};
use reqwest::{Response, StatusCode};
use url::Url;

use crate::platform::{sanitize_filename, FilenameTarget};

use super::error::EngineError;
use super::http::{build_client, get_following_redirects};

/// Platform naming rules applied to a probed filename.
#[cfg(windows)]
const FILENAME_TARGET: FilenameTarget = FilenameTarget::Windows;
/// Platform naming rules applied to a probed filename.
#[cfg(not(windows))]
const FILENAME_TARGET: FilenameTarget = FilenameTarget::MacOs;

/// Options that control how a probe classifies a response.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProbeOptions {
    /// Accept an HTML response instead of reporting `HTML_RESPONSE`.
    pub allow_html: bool,
    /// Additional request headers supplied by the caller.
    pub extra_headers: Vec<(String, String)>,
}

/// Metadata discovered without downloading the response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOutcome {
    /// URL of the final response after any redirects.
    pub final_url: String,
    /// Total file size when the server reports it.
    pub total_bytes: Option<u64>,
    /// Whether the probe observed usable byte-range support.
    pub range_supported: bool,
    /// Sanitized single-component download filename.
    pub filename: String,
    /// Response media type.
    pub content_type: Option<String>,
    /// Response entity tag.
    pub etag: Option<String>,
    /// Response last-modified value.
    pub last_modified: Option<String>,
}

/// Probe `url` for download metadata without consuming the body.
pub async fn probe(url: &str, options: &ProbeOptions) -> Result<ProbeOutcome, EngineError> {
    let client = build_client()?;

    let mut headers = HeaderMap::new();
    for (name, value) in &options.extra_headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| EngineError::new("INVALID_URL", "an extra request header is not valid"))?;
        let value = HeaderValue::from_str(value).map_err(|_| {
            EngineError::new("INVALID_URL", "an extra request header value is not valid")
        })?;
        headers.insert(name, value);
    }
    headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    headers.insert(RANGE, HeaderValue::from_static("bytes=0-0"));

    let (response, final_url) = get_following_redirects(&client, url, headers).await?;

    let content_type = header_text(&response, CONTENT_TYPE);
    let etag = header_text(&response, ETAG);
    let last_modified = header_text(&response, LAST_MODIFIED);
    let content_disposition = header_text(&response, CONTENT_DISPOSITION);

    let (total_bytes, range_supported) = classify(&response)?;

    if !options.allow_html && content_type.as_deref().is_some_and(is_html) {
        return Err(EngineError::new(
            "HTML_RESPONSE",
            "the server returned an HTML page instead of a downloadable file",
        ));
    }

    let filename = resolve_filename(content_disposition.as_deref(), &final_url);

    Ok(ProbeOutcome {
        final_url: final_url.as_str().to_string(),
        total_bytes,
        range_supported,
        filename,
        content_type,
        etag,
        last_modified,
    })
}

fn classify(response: &Response) -> Result<(Option<u64>, bool), EngineError> {
    match response.status() {
        StatusCode::PARTIAL_CONTENT => Ok((Some(parse_content_range(response)?), true)),
        StatusCode::OK => Ok((response.content_length(), false)),
        StatusCode::RANGE_NOT_SATISFIABLE => Ok((Some(parse_unsatisfied_range(response)?), false)),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(EngineError::new(
            "AUTH_REQUIRED",
            "the server requires authorization",
        )),
        StatusCode::NOT_FOUND => Err(EngineError::new(
            "NOT_FOUND",
            "the remote file was not found",
        )),
        StatusCode::TOO_MANY_REQUESTS => Err(EngineError::retryable(
            "RATE_LIMITED",
            "the server asked the client to slow down",
        )),
        StatusCode::REQUEST_TIMEOUT => Err(EngineError::retryable(
            "TIMEOUT",
            "the server reported a request timeout",
        )),
        status if status.is_server_error() => Err(EngineError::retryable(
            "SERVER_ERROR",
            "the server failed to serve the file",
        )),
        _ => Err(invalid_response()),
    }
}

fn parse_content_range(response: &Response) -> Result<u64, EngineError> {
    let value = content_range(response)?;
    let (unit, rest) = value.split_once(' ').ok_or_else(invalid_response)?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return Err(invalid_response());
    }

    let (range, total) = rest.trim().split_once('/').ok_or_else(invalid_response)?;
    let (start, end) = range.split_once('-').ok_or_else(invalid_response)?;
    let start: u64 = start.trim().parse().map_err(|_| invalid_response())?;
    let end: u64 = end.trim().parse().map_err(|_| invalid_response())?;
    let total: u64 = total.trim().parse().map_err(|_| invalid_response())?;

    if start != 0 || end < start || total == 0 || end >= total {
        return Err(invalid_response());
    }

    Ok(total)
}

fn parse_unsatisfied_range(response: &Response) -> Result<u64, EngineError> {
    let value = content_range(response)?;
    let (unit, rest) = value.split_once(' ').ok_or_else(invalid_response)?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return Err(invalid_response());
    }

    rest.trim()
        .strip_prefix("*/")
        .ok_or_else(invalid_response)?
        .trim()
        .parse()
        .map_err(|_| invalid_response())
}

fn content_range(response: &Response) -> Result<&str, EngineError> {
    response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(invalid_response)
}

fn header_text(response: &Response, name: HeaderName) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn is_html(content_type: &str) -> bool {
    content_type
        .split(';')
        .next()
        .map(|value| value.trim().eq_ignore_ascii_case("text/html"))
        .unwrap_or(false)
}

fn resolve_filename(content_disposition: Option<&str>, final_url: &Url) -> String {
    let candidate = content_disposition
        .and_then(disposition_filename)
        .unwrap_or_else(|| url_filename(final_url));
    let component = single_component(&candidate);
    let mut sanitized = sanitize_filename(&component, FILENAME_TARGET);

    if sanitized.starts_with("..") {
        sanitized = sanitized.trim_start_matches('.').to_string();
        if sanitized.is_empty() {
            sanitized = String::from("download");
        }
    }

    sanitized
}

fn disposition_filename(value: &str) -> Option<String> {
    let mut plain = None;
    let mut extended = None;

    for parameter in split_parameters(value) {
        let Some((name, raw)) = parameter.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let raw = raw.trim();

        if name.eq_ignore_ascii_case("filename*") {
            if let Some(decoded) = decode_extended_value(raw) {
                extended = Some(decoded);
            }
        } else if name.eq_ignore_ascii_case("filename") && plain.is_none() {
            plain = Some(unquote(raw));
        }
    }

    extended.or(plain)
}

fn split_parameters(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    let mut escaped = false;

    for (index, character) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ';' if !quoted => {
                parts.push(&value[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&value[start..]);
    parts
}

fn decode_extended_value(raw: &str) -> Option<String> {
    let value = strip_quotes(raw);
    let (charset, remainder) = value.split_once('\'')?;
    if !charset.eq_ignore_ascii_case("utf-8") && !charset.eq_ignore_ascii_case("utf8") {
        return None;
    }
    let (_, encoded) = remainder.split_once('\'')?;
    percent_decode_str(encoded)
        .decode_utf8()
        .ok()
        .map(|decoded| decoded.into_owned())
}

fn strip_quotes(value: &str) -> &str {
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        &value[1..value.len() - 1]
    } else {
        value
    }
}

fn unquote(value: &str) -> String {
    let value = strip_quotes(value);
    let mut output = String::with_capacity(value.len());
    let mut characters = value.chars();

    while let Some(character) = characters.next() {
        if character == '\\' {
            if let Some(escaped) = characters.next() {
                output.push(escaped);
            }
        } else {
            output.push(character);
        }
    }

    output
}

fn url_filename(url: &Url) -> String {
    url.path_segments()
        .and_then(|mut segments| segments.rfind(|segment| !segment.is_empty()))
        .map(|segment| percent_decode_str(segment).decode_utf8_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| String::from("download"))
}

fn single_component(value: &str) -> String {
    value
        .rsplit(['/', '\\'])
        .find(|component| !component.is_empty() && *component != "." && *component != "..")
        .unwrap_or("download")
        .to_string()
}

fn invalid_response() -> EngineError {
    EngineError::new("INVALID_RESPONSE", "the server response was malformed")
}
