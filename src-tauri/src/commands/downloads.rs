//! Download command functions behind the Tauri IPC wrappers.
//!
//! These synchronous functions take [`AppState`] so the command surface can be
//! exercised without a Tauri runtime. Every engine failure keeps its stable
//! code and user-safe message, and a missing engine reports `engine_unavailable`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::app_state::AppState;
use crate::engine::service::{AddDownload, EngineService, ProbeRequest};
use crate::error::AppError;
use crate::store::{Segment, Task};

/// A request to create one download task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddDownloadRequest {
    /// Remote HTTP or HTTPS URL.
    pub url: String,
    /// Requested final filename.
    pub filename: String,
    /// Destination directory, or `None` for the configured default.
    pub destination: Option<PathBuf>,
    /// Requested parallel connection count.
    pub connections: u32,
    /// Optional per-task rate limit in bytes per second.
    pub rate_limit: Option<u64>,
    /// Optional expected SHA-256 of the finished file.
    pub expected_sha256: Option<String>,
    /// Accept an HTML response as an ordinary file.
    pub allow_html: bool,
    /// Additional request headers.
    pub extra_headers: Vec<(String, String)>,
}

/// A request to probe one remote URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeDownloadRequest {
    /// Remote HTTP or HTTPS URL.
    pub url: String,
    /// Accept an HTML response instead of reporting an HTML error.
    pub allow_html: bool,
    /// Additional request headers.
    pub extra_headers: Vec<(String, String)>,
}

/// A lightweight task row for the polled download list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadSummary {
    /// Stable task identifier.
    pub id: String,
    /// Stored, redacted remote URL.
    pub url: String,
    /// Final URL after redirects, redacted before storage.
    pub final_url: Option<String>,
    /// Sanitized destination filename.
    pub filename: String,
    /// Persisted destination directory.
    pub destination: Option<String>,
    /// Lowercase lifecycle state.
    pub state: String,
    /// Bytes confirmed written.
    pub downloaded_bytes: u64,
    /// Known total size.
    pub total_bytes: Option<u64>,
    /// Requested parallel connection count.
    pub connections: u32,
    /// Creation time in epoch milliseconds.
    pub created_at_ms: i64,
    /// Last modification time in epoch milliseconds.
    pub updated_at_ms: i64,
    /// Completion time in epoch milliseconds.
    pub completed_at_ms: Option<i64>,
    /// Stable failure code.
    pub error_code: Option<String>,
    /// User-safe failure message.
    pub error_message: Option<String>,
}

/// One segment row for the task details panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SegmentView {
    /// Zero-based segment index.
    pub index: u32,
    /// Inclusive first byte offset.
    pub start: u64,
    /// Exclusive end offset.
    pub end: u64,
    /// Bytes confirmed written for this range.
    pub downloaded_bytes: u64,
    /// Lowercase segment state.
    pub state: String,
}

/// Remote metadata returned by a probe.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    /// Final URL after redirects.
    pub final_url: String,
    /// Total file size when the server reports one.
    pub total_bytes: Option<u64>,
    /// Whether the server supports byte ranges.
    pub range_supported: bool,
    /// Inferred or disposition-supplied filename.
    pub filename: String,
    /// Response media type.
    pub content_type: Option<String>,
    /// Response entity tag.
    pub etag: Option<String>,
    /// Response last-modified value.
    pub last_modified: Option<String>,
}

impl From<Task> for DownloadSummary {
    fn from(task: Task) -> Self {
        Self {
            id: task.id,
            url: task.url,
            final_url: task.final_url,
            filename: task.filename,
            destination: task.destination,
            state: task.state.as_str().to_string(),
            downloaded_bytes: task.downloaded_bytes,
            total_bytes: task.total_bytes,
            connections: task.connections,
            created_at_ms: task.created_at_ms,
            updated_at_ms: task.updated_at_ms,
            completed_at_ms: task.completed_at_ms,
            error_code: task.error_code,
            error_message: task.error_message,
        }
    }
}

impl From<Segment> for SegmentView {
    fn from(segment: Segment) -> Self {
        Self {
            index: segment.index,
            start: segment.start,
            end: segment.end,
            downloaded_bytes: segment.downloaded_bytes,
            state: segment.state.as_str().to_string(),
        }
    }
}

/// List every stored task as a lightweight summary.
pub fn downloads_list(state: &AppState) -> Result<Vec<DownloadSummary>, AppError> {
    let tasks = engine(state)?.list().map_err(AppError::from)?;
    Ok(tasks.into_iter().map(DownloadSummary::from).collect())
}

/// Load one task's segment plan.
pub fn downloads_segments(state: &AppState, id: &str) -> Result<Vec<SegmentView>, AppError> {
    let segments = engine(state)?.segments(id).map_err(AppError::from)?;
    Ok(segments.into_iter().map(SegmentView::from).collect())
}

/// Probe a remote URL for download metadata.
pub fn downloads_probe(
    state: &AppState,
    request: ProbeDownloadRequest,
) -> Result<ProbeResult, AppError> {
    let outcome = engine(state)?
        .probe(ProbeRequest {
            url: request.url,
            allow_html: request.allow_html,
            extra_headers: request.extra_headers,
        })
        .map_err(AppError::from)?;

    Ok(ProbeResult {
        final_url: outcome.final_url,
        total_bytes: outcome.total_bytes,
        range_supported: outcome.range_supported,
        filename: outcome.filename,
        content_type: outcome.content_type,
        etag: outcome.etag,
        last_modified: outcome.last_modified,
    })
}

/// Create a queued download task.
pub fn downloads_add(state: &AppState, request: AddDownloadRequest) -> Result<String, AppError> {
    engine(state)?
        .add(AddDownload {
            url: request.url,
            filename: request.filename,
            destination: request.destination,
            connections: request.connections,
            rate_limit: request.rate_limit,
            expected_sha256: request.expected_sha256,
            allow_html: request.allow_html,
            extra_headers: request.extra_headers,
        })
        .map_err(AppError::from)
}

/// Pause a task.
pub fn downloads_pause(state: &AppState, id: &str) -> Result<(), AppError> {
    engine(state)?.pause(id).map_err(AppError::from)
}

/// Resume a task.
pub fn downloads_resume(state: &AppState, id: &str) -> Result<(), AppError> {
    engine(state)?.resume(id).map_err(AppError::from)
}

/// Cancel a task.
pub fn downloads_cancel(state: &AppState, id: &str) -> Result<(), AppError> {
    engine(state)?.cancel(id).map_err(AppError::from)
}

/// Retry a failed task.
pub fn downloads_retry(state: &AppState, id: &str) -> Result<(), AppError> {
    engine(state)?.retry(id).map_err(AppError::from)
}

/// Delete a terminal task record.
pub fn downloads_delete_record(state: &AppState, id: &str) -> Result<(), AppError> {
    engine(state)?.delete_record(id).map_err(AppError::from)
}

/// Delete a completed task's final file and record.
pub fn downloads_delete_file(state: &AppState, id: &str) -> Result<(), AppError> {
    engine(state)?.delete_file(id).map_err(AppError::from)
}

/// Open a completed task's final file.
pub fn downloads_open_file(state: &AppState, id: &str) -> Result<(), AppError> {
    map_open(engine(state)?.open_file(id))
}

/// Open a task's destination folder.
pub fn downloads_open_folder(state: &AppState, id: &str) -> Result<(), AppError> {
    map_open(engine(state)?.open_folder(id))
}

fn engine(state: &AppState) -> Result<&EngineService, AppError> {
    state.engine().ok_or_else(|| {
        AppError::new(
            "engine_unavailable",
            "the download engine service is not available",
        )
    })
}

fn map_open(result: Result<(), crate::engine::error::EngineError>) -> Result<(), AppError> {
    match result {
        Ok(()) => Ok(()),
        Err(error) if matches!(error.code(), "FILE_MISSING" | "not_found") => Err(AppError::new(
            "FILE_MISSING",
            "the requested file or folder is missing",
        )),
        Err(error) => Err(AppError::from(error)),
    }
}
