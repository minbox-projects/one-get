//! Video command surface behind the Tauri IPC wrappers.
//!
//! A probe resolves the verified component tools before the guarded yt-dlp run,
//! and a start validates the URL, filename, destination, cookies file, and
//! proxy before persisting anything. The full source URL stays in memory; only
//! its minimized form reaches the task store.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::app_state::AppState;
use crate::engine::http::{storage_url, validate_proxy_url, validate_url};
use crate::error::AppError;
use crate::platform::filename::{current_target, sanitize_filename};
use crate::video::quality::QualityChoice;
use crate::video::service::StartVideoRequest;

/// A request to probe one video URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoProbeRequest {
    /// Remote video URL.
    pub url: String,
    /// Optional Netscape cookies-file path passed to yt-dlp.
    pub cookies_file: Option<PathBuf>,
    /// Optional credential-free HTTP proxy URL.
    pub proxy_url: Option<String>,
}

/// A request to create one video download task.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoStartRequest {
    /// Remote video URL.
    pub url: String,
    /// Requested final filename.
    pub filename: String,
    /// Destination directory, or `None` for the configured default.
    pub destination: Option<PathBuf>,
    /// Selected yt-dlp format selector.
    pub format_selector: String,
    /// Whether the download extracts audio only.
    pub audio_only: bool,
    /// Parsed probe metadata stored with the task.
    pub metadata: Value,
    /// Optional Netscape cookies-file path.
    pub cookies_file: Option<PathBuf>,
    /// Optional credential-free HTTP proxy URL.
    pub proxy_url: Option<String>,
    /// Optional requested connection count stored with the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connections: Option<u32>,
}

/// One selectable quality choice returned by a probe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoChoiceView {
    /// yt-dlp format identifier.
    pub id: String,
    /// Human-readable resolution or bitrate label.
    pub label: String,
    /// Estimated output size, or unknown.
    pub size_bytes: Option<u64>,
    /// Whether the container is MP4.
    pub mp4_compatible: bool,
    /// yt-dlp format selector for this choice.
    pub format_selector: String,
}

impl From<&QualityChoice> for VideoChoiceView {
    fn from(choice: &QualityChoice) -> Self {
        Self {
            id: choice.id.clone(),
            label: choice.label.clone(),
            size_bytes: choice.size_bytes,
            mp4_compatible: choice.mp4_compatible,
            format_selector: choice.format_selector.clone(),
        }
    }
}

/// Probe metadata and ordered quality choices for the new-download dialog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoProbeResult {
    /// Media title.
    pub title: String,
    /// Duration in seconds, when reported.
    pub duration_secs: Option<f64>,
    /// Uploader or channel name, when reported.
    pub uploader: Option<String>,
    /// Video choices ordered from highest to lowest quality.
    pub video: Vec<VideoChoiceView>,
    /// Audio-only choices ordered from highest to lowest bitrate.
    pub audio: Vec<VideoChoiceView>,
    /// Index into `video` for the default choice.
    pub default_index: Option<usize>,
}

/// Resolve the verified tools and probe a video URL.
pub fn video_probe(
    state: &AppState,
    request: VideoProbeRequest,
) -> Result<VideoProbeResult, AppError> {
    let url = validate_url(&request.url).map_err(AppError::from)?;
    let cookies_file = validate_cookies_file(request.cookies_file)?;
    let proxy_url = validate_video_proxy(request.proxy_url)?;

    let result = video(state)?
        .probe(&url, cookies_file, proxy_url)
        .map_err(video_error)?;
    Ok(VideoProbeResult {
        title: result.metadata.title,
        duration_secs: result.metadata.duration_secs,
        uploader: result.metadata.uploader,
        video: result
            .quality
            .video
            .iter()
            .map(VideoChoiceView::from)
            .collect(),
        audio: result
            .quality
            .audio
            .iter()
            .map(VideoChoiceView::from)
            .collect(),
        default_index: result.quality.default_index,
    })
}

/// Validate a video request and persist one queued task.
pub fn video_start(state: &AppState, request: VideoStartRequest) -> Result<String, AppError> {
    let url = validate_url(&request.url).map_err(AppError::from)?;
    let minimized_url = storage_url(&url);
    let filename = sanitize_filename(&request.filename, current_target());
    let connections = validate_connections(request.connections)?;

    let destination = request
        .destination
        .clone()
        .unwrap_or_else(|| state.paths().downloads.clone());
    let destination = resolve_destination(&destination)?;

    let cookies_file = validate_cookies_file(request.cookies_file)?;
    let proxy_url = validate_video_proxy(request.proxy_url)?;

    let service = video(state)?;
    service
        .add_with_connections(
            StartVideoRequest {
                url,
                minimized_url,
                filename,
                destination: Some(destination),
                format_selector: request.format_selector,
                audio_only: request.audio_only,
                metadata: request.metadata,
                cookies_file,
                proxy_url,
                full_url_persisted: false,
            },
            connections,
        )
        .map_err(video_error)
}

fn validate_connections(connections: Option<u32>) -> Result<Option<u32>, AppError> {
    match connections {
        Some(value) if !(1..=64).contains(&value) => Err(AppError::new(
            "INVALID_CONNECTIONS",
            "the connection count must be between 1 and 64",
        )),
        value => Ok(value),
    }
}

fn validate_cookies_file(path: Option<PathBuf>) -> Result<Option<PathBuf>, AppError> {
    let Some(path) = path else {
        return Ok(None);
    };
    if !path.is_file() {
        return Err(AppError::new(
            "INVALID_COOKIES_FILE",
            "the cookies file does not exist or is not a file",
        ));
    }
    // Opening checks readability without reading the cookie contents.
    std::fs::File::open(&path)
        .map_err(|_| AppError::new("INVALID_COOKIES_FILE", "the cookies file cannot be read"))?;
    Ok(Some(path))
}

fn validate_video_proxy(proxy: Option<String>) -> Result<Option<String>, AppError> {
    match proxy {
        Some(proxy) => validate_proxy_url(&proxy).map(Some).map_err(AppError::from),
        None => Ok(None),
    }
}

fn resolve_destination(requested: &Path) -> Result<PathBuf, AppError> {
    let canonical = std::fs::canonicalize(requested).map_err(|_| {
        AppError::new(
            "INVALID_DESTINATION",
            "the destination directory does not exist",
        )
    })?;
    let metadata = std::fs::metadata(&canonical).map_err(|_| {
        AppError::new(
            "INVALID_DESTINATION",
            "the destination directory cannot be read",
        )
    })?;
    if !metadata.is_dir() {
        return Err(AppError::new(
            "INVALID_DESTINATION",
            "the destination is not a directory",
        ));
    }
    let probe = canonical.join(format!(".oneget-write-probe-{}", uuid::Uuid::new_v4()));
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(file) => {
            drop(file);
            let _ = std::fs::remove_file(&probe);
        }
        Err(_) => {
            return Err(AppError::new(
                "INVALID_DESTINATION",
                "the destination directory is not writable",
            ));
        }
    }
    Ok(canonical)
}

fn video(state: &AppState) -> Result<&crate::video::service::VideoService, AppError> {
    state.video().ok_or_else(|| {
        AppError::new(
            "video_unavailable",
            "the video download service is not available",
        )
    })
}

fn video_error(error: crate::video::error::VideoError) -> AppError {
    AppError::new(error.code(), error.message())
}
