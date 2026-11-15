//! Download command functions behind the Tauri IPC wrappers.
//!
//! These synchronous functions take [`AppState`] so the command surface can be
//! exercised without a Tauri runtime. Every engine failure keeps its stable
//! code and user-safe message, and a missing engine reports `engine_unavailable`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::app_state::AppState;
use crate::engine::service::{AddDownload, EngineService, ProbeRequest};
use crate::error::AppError;
use crate::store::hf::HfMetadata;
use crate::store::{Segment, Task, TaskKind};
use crate::video::service::VideoService;

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

/// Persisted video metadata exposed alongside a video task summary.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoSummary {
    /// Media title.
    pub title: Option<String>,
    /// Duration in seconds.
    pub duration_secs: Option<f64>,
    /// Uploader or channel name.
    pub uploader: Option<String>,
    /// Selected quality label or format selector.
    pub quality: Option<String>,
    /// Current video stage, for example `merge`.
    pub stage: Option<String>,
    /// Whether the task extracts audio only.
    pub audio_only: bool,
}

/// Persisted Hugging Face metadata exposed alongside an HTTP task summary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HuggingFaceSummary {
    /// Repository identifier without the family prefix.
    pub repo_id: String,
    /// File path within the repository.
    pub file_path: String,
    /// Revision, branch, or tag.
    pub revision: String,
    /// Connections actually used by the task.
    pub effective_connections: u32,
    /// Metadata refresh count.
    pub refresh_count: u32,
    /// Whether the transfer fell back to a downgraded connection count.
    pub downgraded: bool,
}

/// A lightweight task row for the polled download list.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadSummary {
    /// Stable task identifier.
    pub id: String,
    /// Pipeline that owns the task: `http` or `video`.
    pub kind: String,
    /// Structured video metadata for a video task.
    pub video: Option<VideoSummary>,
    /// Structured Hugging Face metadata for a resolver-host task.
    pub huggingface: Option<HuggingFaceSummary>,
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
        let kind = task.kind.as_str().to_string();
        let video = if task.kind == TaskKind::Video {
            video_summary(task.video.as_ref())
        } else {
            None
        };
        Self {
            id: task.id,
            kind,
            video,
            huggingface: None,
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

fn video_summary(record: Option<&Value>) -> Option<VideoSummary> {
    let record = record?;
    let metadata = record.get("metadata");
    Some(VideoSummary {
        title: read_string(metadata, record, "title"),
        duration_secs: read_f64(metadata, record, "durationSecs")
            .or_else(|| read_f64(metadata, record, "duration")),
        uploader: read_string(metadata, record, "uploader"),
        quality: read_string(metadata, record, "quality").or_else(|| {
            record
                .get("formatSelector")
                .and_then(Value::as_str)
                .map(str::to_string)
        }),
        stage: read_string(metadata, record, "mergeStage")
            .or_else(|| read_string(metadata, record, "stage")),
        audio_only: record
            .get("audioOnly")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

fn read_string(metadata: Option<&Value>, record: &Value, key: &str) -> Option<String> {
    metadata
        .and_then(|value| value.get(key))
        .and_then(Value::as_str)
        .or_else(|| record.get(key).and_then(Value::as_str))
        .map(str::to_string)
}

fn read_f64(metadata: Option<&Value>, record: &Value, key: &str) -> Option<f64> {
    metadata
        .and_then(|value| value.get(key))
        .and_then(Value::as_f64)
        .or_else(|| record.get(key).and_then(Value::as_f64))
}

/// List every stored task as a lightweight summary.
pub fn downloads_list(state: &AppState) -> Result<Vec<DownloadSummary>, AppError> {
    let engine = engine(state)?;
    let tasks = engine.list().map_err(AppError::from)?;
    let metadata = state
        .hf_metadata()
        .or_else(|| engine.huggingface_metadata());
    Ok(tasks
        .into_iter()
        .map(|task| {
            let mut summary = DownloadSummary::from(task);
            summary.huggingface = metadata
                .as_ref()
                .and_then(|store| store.get(&summary.id).ok().flatten())
                .as_ref()
                .map(huggingface_summary);
            summary
        })
        .collect())
}

fn huggingface_summary(metadata: &HfMetadata) -> HuggingFaceSummary {
    HuggingFaceSummary {
        repo_id: metadata.repo_id.clone(),
        file_path: metadata.file_path.clone(),
        revision: metadata.revision.clone(),
        effective_connections: metadata.effective_connections,
        refresh_count: metadata.refresh_count,
        downgraded: metadata.downgraded,
    }
}

/// Load one task's segment plan; a video task has no byte segments.
pub fn downloads_segments(state: &AppState, id: &str) -> Result<Vec<SegmentView>, AppError> {
    match task_kind(state, id)? {
        TaskKind::Video => Ok(Vec::new()),
        TaskKind::Http => {
            let segments = engine(state)?.segments(id).map_err(AppError::from)?;
            Ok(segments.into_iter().map(SegmentView::from).collect())
        }
    }
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
    let engine = engine(state)?;
    if matches!(crate::engine::hf::parse_hf_url(&request.url), Ok(Some(_))) {
        state.ensure_hf_metadata_attached();
    }
    engine
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

/// Pause a task through the pipeline that owns it.
pub fn downloads_pause(state: &AppState, id: &str) -> Result<(), AppError> {
    match task_kind(state, id)? {
        TaskKind::Http => engine(state)?.pause(id).map_err(AppError::from),
        TaskKind::Video => video(state)?.pause(id).map_err(video_error),
    }
}

/// Resume a paused task through the pipeline that owns it.
pub fn downloads_resume(state: &AppState, id: &str) -> Result<(), AppError> {
    match task_kind(state, id)? {
        TaskKind::Http => engine(state)?.resume(id).map_err(AppError::from),
        TaskKind::Video => video(state)?.resume(id).map_err(video_error),
    }
}

/// Cancel a task through the pipeline that owns it.
pub fn downloads_cancel(state: &AppState, id: &str) -> Result<(), AppError> {
    match task_kind(state, id)? {
        TaskKind::Http => engine(state)?.cancel(id).map_err(AppError::from),
        TaskKind::Video => video(state)?.cancel(id).map_err(video_error),
    }
}

/// Retry a failed task through the pipeline that owns it.
pub fn downloads_retry(state: &AppState, id: &str) -> Result<(), AppError> {
    match task_kind(state, id)? {
        TaskKind::Http => engine(state)?.retry(id).map_err(AppError::from),
        TaskKind::Video => video(state)?.retry(id).map_err(video_error),
    }
}

fn task_kind(state: &AppState, id: &str) -> Result<TaskKind, AppError> {
    let tasks = engine(state)?.list().map_err(AppError::from)?;
    tasks
        .into_iter()
        .find(|task| task.id == id)
        .map(|task| task.kind)
        .ok_or_else(|| AppError::new("TASK_NOT_FOUND", "the requested task does not exist"))
}

fn video(state: &AppState) -> Result<&VideoService, AppError> {
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
