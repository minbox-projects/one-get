//! yt-dlp metadata probe and result parsing.
//!
//! The probe validates the URL before any spawn, runs yt-dlp through the guarded
//! process runner with a fixed metadata argument set, and turns the JSON result
//! into [`VideoMetadata`]. Cookies, proxy, and runtime paths are forwarded as
//! arguments only and never logged.

use std::path::PathBuf;
use std::time::Duration;

use serde_json::Value;
use url::Url;

use super::error::VideoError;
use super::metadata::{VideoFormat, VideoMetadata};
use super::process::{run_probe, ProcessSpec, TimeoutPolicy};

/// Maximum accepted source URL length in bytes.
const MAX_URL_BYTES: usize = 8 * 1024;
const URL_UNSUPPORTED: &str = "VIDEO_URL_UNSUPPORTED";
const PROBE_INVALID: &str = "VIDEO_PROBE_INVALID";
const LIVE_UNSUPPORTED: &str = "VIDEO_LIVE_UNSUPPORTED";
const DRM_UNSUPPORTED: &str = "VIDEO_DRM_UNSUPPORTED";

/// Inputs for one yt-dlp metadata probe.
#[derive(Debug, Clone)]
pub struct ProbeOptions {
    /// The resolved yt-dlp executable.
    pub yt_dlp: PathBuf,
    /// Optional active Deno runtime.
    pub deno: Option<PathBuf>,
    /// Optional Netscape cookies file passed to yt-dlp as a path only.
    pub cookies_file: Option<PathBuf>,
    /// Optional credential-free HTTP proxy URL passed to yt-dlp.
    pub proxy_url: Option<String>,
    /// Bound for the whole probe.
    pub timeout: Duration,
}

/// Probe a video URL and return its parsed metadata.
pub async fn probe_video(url: &str, options: &ProbeOptions) -> Result<VideoMetadata, VideoError> {
    let parsed = validate_url(url)?;
    let spec = ProcessSpec {
        program: options.yt_dlp.clone(),
        args: probe_args(parsed.as_str(), options),
        cwd: None,
        env: Vec::new(),
    };
    let policy = TimeoutPolicy {
        probe: options.timeout,
        ..TimeoutPolicy::default()
    };
    let output = match run_probe(&spec, &policy).await {
        Ok(output) => output,
        Err(error) if error.is_missing_js_runtime() => {
            return Err(VideoError::js_runtime_missing());
        }
        Err(error) => return Err(error),
    };
    parse_metadata(&output.stdout)
}

fn validate_url(url: &str) -> Result<Url, VideoError> {
    if url.is_empty() || url.len() > MAX_URL_BYTES || url.chars().any(char::is_control) {
        return Err(url_unsupported());
    }
    let parsed = Url::parse(url).map_err(|_| url_unsupported())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(url_unsupported());
    }
    if parsed.host_str().map(str::is_empty).unwrap_or(true) {
        return Err(url_unsupported());
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(url_unsupported());
    }
    Ok(parsed)
}

fn probe_args(url: &str, options: &ProbeOptions) -> Vec<String> {
    let mut args = vec![
        "--dump-single-json".to_string(),
        "--no-playlist".to_string(),
        "--no-warnings".to_string(),
        "--skip-download".to_string(),
        "--ignore-config".to_string(),
    ];

    if let Some(deno) = &options.deno {
        args.push("--js-runtimes".to_string());
        args.push(format!("deno:{}", deno.display()));
    }
    if let Some(cookies_file) = &options.cookies_file {
        args.push("--cookies".to_string());
        args.push(cookies_file.to_string_lossy().into_owned());
    }
    if let Some(proxy_url) = &options.proxy_url {
        args.push("--proxy".to_string());
        args.push(proxy_url.clone());
    }

    args.push(url.to_string());
    args
}

fn parse_metadata(bytes: &[u8]) -> Result<VideoMetadata, VideoError> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| invalid("the probe result was not valid JSON"))?;
    let object = value
        .as_object()
        .ok_or_else(|| invalid("the probe result was not a JSON object"))?;

    let title = object
        .get("title")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("the probe result is missing a title"))?
        .to_string();
    let duration_secs = optional_f64(object, "duration")?;
    let uploader = optional_string(object, "uploader")?;
    let is_live = optional_bool(object, "is_live")?.unwrap_or(false);
    let live_status = object
        .get("live_status")
        .and_then(Value::as_str)
        .unwrap_or_default();

    if is_live || matches!(live_status, "is_live" | "is_upcoming" | "post_live") {
        return Err(VideoError::new(
            LIVE_UNSUPPORTED,
            "live and upcoming streams are not supported",
        ));
    }

    if optional_bool(object, "has_drm")?.unwrap_or(false) {
        return Err(drm_unsupported());
    }

    let formats = object
        .get("formats")
        .ok_or_else(|| invalid("the probe result is missing formats"))?
        .as_array()
        .ok_or_else(|| invalid("the probe result formats were not a list"))?;
    if formats.is_empty() {
        return Err(invalid("the probe result contained no formats"));
    }

    let formats = formats
        .iter()
        .map(parse_format)
        .collect::<Result<Vec<_>, _>>()?;
    if formats.iter().all(|format| format.has_drm) {
        return Err(drm_unsupported());
    }

    Ok(VideoMetadata {
        title,
        duration_secs,
        uploader,
        is_live,
        formats,
    })
}

fn parse_format(value: &Value) -> Result<VideoFormat, VideoError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid("a probe format entry was not an object"))?;
    let format_id = object
        .get("format_id")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("a probe format entry is missing its identifier"))?
        .to_string();

    let height = match optional_u64(object, "height")? {
        Some(height) => Some(
            u32::try_from(height).map_err(|_| invalid("a probe format height was too large"))?,
        ),
        None => None,
    };

    Ok(VideoFormat {
        format_id,
        height,
        fps: optional_f64(object, "fps")?,
        bitrate: optional_u64(object, "bitrate")?,
        filesize: optional_u64(object, "filesize")?,
        vcodec: optional_string(object, "vcodec")?,
        acodec: optional_string(object, "acodec")?,
        ext: optional_string(object, "ext")?,
        has_drm: optional_bool(object, "has_drm")?.unwrap_or(false),
    })
}

fn optional_string(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<String>, VideoError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(invalid(format!(
            "a probe field `{key}` had an invalid type"
        ))),
    }
}

fn optional_bool(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<bool>, VideoError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(invalid(format!(
            "a probe field `{key}` had an invalid type"
        ))),
    }
}

fn optional_u64(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<u64>, VideoError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| invalid(format!("a probe field `{key}` had an invalid type"))),
    }
}

fn optional_f64(
    object: &serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<f64>, VideoError> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .map(Some)
            .ok_or_else(|| invalid(format!("a probe field `{key}` had an invalid type"))),
    }
}

fn url_unsupported() -> VideoError {
    VideoError::new(
        URL_UNSUPPORTED,
        "only http and https video URLs are supported",
    )
}

fn drm_unsupported() -> VideoError {
    VideoError::new(DRM_UNSUPPORTED, "DRM-protected video is not supported")
}

fn invalid(message: impl Into<String>) -> VideoError {
    VideoError::new(PROBE_INVALID, message)
}
