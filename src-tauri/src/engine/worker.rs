//! Bounded multi-connection download execution and atomic finalization.
//!
//! A task probes the remote file, plans its segments, writes into one part file
//! at absolute offsets, and then finalizes by syncing, hashing, choosing a
//! non-conflicting final name, and atomically renaming the part file.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use futures_util::{stream, StreamExt};
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT_ENCODING, CONTENT_RANGE, CONTENT_TYPE, ETAG,
    LAST_MODIFIED, RANGE,
};
use reqwest::{Client, StatusCode};
use sha2::{Digest, Sha256};

use crate::platform::files::CapDir;
use crate::platform::PlatformError;
use crate::store::types::{SegmentInput, SegmentProgressInput, Task};
use crate::store::Database;

use super::error::EngineError;
use super::http::{build_client, get_following_redirects};
use super::part_file::PartFile;
use super::planner::{plan_segments, SegmentPlan};
use super::probe::{probe, ProbeOptions, ProbeOutcome};
use super::state::{SegmentState, TaskState};

/// Progress is persisted at least once per this many downloaded bytes.
const PROGRESS_INTERVAL: u64 = 1024 * 1024;

/// Each ranged segment is fetched as bounded reads of this size.
const READ_CHUNK_BYTES: u64 = 128 * 1024;

/// Bounded replays for a ranged read that returns 200 or resets before any
/// response body is written. No bytes are committed for a replayed read.
const RANGE_REQUEST_ATTEMPTS: usize = 8;

/// Upper bound for the stored end of an unknown-length segment.
const OPEN_ENDED_END: u64 = i64::MAX as u64;

/// Caller-supplied download configuration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DownloadSpec {
    /// Remote HTTP or HTTPS URL.
    pub url: String,
    /// Sanitized final filename.
    pub filename: String,
    /// Requested number of parallel connections, from 1 to 64.
    pub connections: u32,
    /// Accept an HTML response as an ordinary file.
    pub allow_html: bool,
    /// Expected SHA-256 of the finished file.
    pub expected_sha256: Option<String>,
    /// Additional request headers.
    pub extra_headers: Vec<(String, String)>,
}

/// Result of a completed download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadOutcome {
    /// SHA-256 of the final file.
    pub actual_sha256: String,
    /// Total byte length of the final file.
    pub total_bytes: u64,
    /// Absolute path of the final file.
    pub final_path: PathBuf,
}

/// Result of finalizing an existing finalizing task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinalizeOutcome {
    /// Absolute path of the final file.
    pub final_path: PathBuf,
}

/// How one engine execution of a task finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskRunOutcome {
    /// The part file was finalized into its destination file.
    Completed(DownloadOutcome),
    /// Workers stopped and progress was persisted for a later resume.
    Paused,
    /// Workers stopped and the task's own part file was removed.
    Cancelled,
}

/// Cooperative pause and cancel signals shared with a running task.
#[derive(Debug, Default)]
pub struct TaskControl {
    pause_requested: AtomicBool,
    cancel_requested: AtomicBool,
}

impl TaskControl {
    /// Create a control object that lets a task run to completion.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the task's workers to stop before their next streamed chunk.
    pub fn request_pause(&self) {
        self.pause_requested.store(true, Ordering::SeqCst);
    }

    /// Ask the task's workers to stop and discard the task's part file.
    pub fn request_cancel(&self) {
        self.cancel_requested.store(true, Ordering::SeqCst);
    }

    /// Whether a pause has been requested.
    pub fn pause_requested(&self) -> bool {
        self.pause_requested.load(Ordering::SeqCst)
    }

    /// Whether a cancel has been requested.
    pub fn cancel_requested(&self) -> bool {
        self.cancel_requested.load(Ordering::SeqCst)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkerFlow {
    Complete,
    Paused,
    Cancelled,
}

fn requested_flow(control: &TaskControl) -> Option<WorkerFlow> {
    if control.cancel_requested() {
        Some(WorkerFlow::Cancelled)
    } else if control.pause_requested() {
        Some(WorkerFlow::Paused)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy)]
enum RequestShape {
    Range { start: u64, end: Option<u64> },
    Plain,
}

impl RequestShape {
    fn range_header(self) -> Option<String> {
        match self {
            Self::Range { start, end } => Some(match end {
                Some(end) => format!("bytes={start}-{}", end - 1),
                None => format!("bytes={start}-"),
            }),
            Self::Plain => None,
        }
    }

    fn expects_partial(self) -> bool {
        matches!(self, Self::Range { .. })
    }
}

#[derive(Debug, Clone, Default)]
struct DownloadMetadata {
    total_bytes: Option<u64>,
    range_supported: bool,
    etag: Option<String>,
    last_modified: Option<String>,
}

impl From<ProbeOutcome> for DownloadMetadata {
    fn from(outcome: ProbeOutcome) -> Self {
        Self {
            total_bytes: outcome.total_bytes,
            range_supported: outcome.range_supported,
            etag: outcome.etag,
            last_modified: outcome.last_modified,
        }
    }
}

/// Download one task to completion and finalize its file.
pub async fn download_once(
    store: &Database,
    task_id: &str,
    dest_dir: &Path,
    spec: &DownloadSpec,
) -> Result<DownloadOutcome, EngineError> {
    let options = ProbeOptions {
        allow_html: spec.allow_html,
        extra_headers: spec.extra_headers.clone(),
    };

    let metadata = match probe(&spec.url, &options).await {
        Ok(outcome) => DownloadMetadata::from(outcome),
        Err(error) if error.code() == "INVALID_RESPONSE" => {
            match probe_zero_length(&spec.url, &options).await {
                Ok(metadata) => metadata,
                Err(_) => return Err(mark_failed(store, task_id, error)),
            }
        }
        Err(error) => return Err(mark_failed(store, task_id, error)),
    };

    let plan = plan_segments(
        metadata.total_bytes,
        metadata.range_supported,
        spec.connections,
    )
    .map_err(|error| mark_failed(store, task_id, error))?;
    // A known empty total finalizes without workers regardless of range support.
    let plan = if metadata.total_bytes == Some(0) {
        SegmentPlan::Ranged(Vec::new())
    } else {
        plan
    };

    store.record_discovered_metadata(
        task_id,
        metadata.total_bytes,
        metadata.etag.as_deref(),
        metadata.last_modified.as_deref(),
    )?;

    let stored = load_task(store, task_id)?;
    if stored.state == TaskState::Queued {
        store.transition_task(task_id, TaskState::Queued, TaskState::Connecting)?;
    }
    let stored = load_task(store, task_id)?;
    if stored.state == TaskState::Connecting {
        store.transition_task(task_id, TaskState::Connecting, TaskState::Downloading)?;
    }
    let state = load_task(store, task_id)?.state;
    if state != TaskState::Downloading {
        return Err(EngineError::invalid_state(
            "only a connecting task can start downloading",
        ));
    }

    let cap = CapDir::open(dest_dir).map_err(platform_error)?;
    let part_name = PartFile::part_name(&spec.filename);
    let part = PartFile::create(&cap, &spec.filename, metadata.total_bytes)?;
    store.record_part_path(task_id, &part_name)?;

    let segment_rows = segment_inputs(&plan, metadata.total_bytes);
    store.replace_segments(task_id, &segment_rows)?;
    let progress = Mutex::new(vec![0u64; segment_rows.len()]);

    let client = build_client()?;
    let control = TaskControl::new();
    match &plan {
        SegmentPlan::Ranged(segments) => {
            let concurrency = (spec.connections as usize).max(1);
            let mut results = stream::iter(segments.iter().enumerate().map(|(index, segment)| {
                let request = RequestShape::Range {
                    start: segment.start,
                    end: Some(segment.end),
                };
                run_worker(
                    &client,
                    store,
                    task_id,
                    &part,
                    &progress,
                    spec,
                    &control,
                    request,
                    index as u32,
                    segment.start,
                    Some(segment.end),
                )
            }))
            .buffer_unordered(concurrency);
            while let Some(result) = results.next().await {
                match result {
                    Ok(WorkerFlow::Complete) => {}
                    Ok(_) => {
                        return Err(EngineError::invalid_state(
                            "the download was interrupted without a control request",
                        ))
                    }
                    Err(error) => return Err(mark_failed(store, task_id, error)),
                }
            }
        }
        SegmentPlan::OpenEnded { start } => {
            let request = if metadata.range_supported {
                RequestShape::Range {
                    start: *start,
                    end: None,
                }
            } else {
                RequestShape::Plain
            };
            match run_worker(
                &client,
                store,
                task_id,
                &part,
                &progress,
                spec,
                &control,
                request,
                0,
                *start,
                metadata.total_bytes,
            )
            .await
            .map_err(|error| mark_failed(store, task_id, error))?
            {
                WorkerFlow::Complete => {}
                _ => {
                    return Err(EngineError::invalid_state(
                        "the download was interrupted without a control request",
                    ))
                }
            }
        }
    }

    part.sync_all()?;
    store.transition_task(task_id, TaskState::Downloading, TaskState::Finalizing)?;
    drop(part);

    let finalized = finalize_task(store, task_id, dest_dir, spec.expected_sha256.as_deref())?;
    let task = load_task(store, task_id)?;
    let actual_sha256 = task.actual_sha256.ok_or_else(|| {
        EngineError::new(
            "CHECKSUM_MISSING",
            "the completed task did not store its SHA-256",
        )
    })?;
    let total_bytes = metadata.total_bytes.unwrap_or_else(|| {
        std::fs::metadata(&finalized.final_path)
            .map(|metadata| metadata.len())
            .unwrap_or(0)
    });

    Ok(DownloadOutcome {
        actual_sha256,
        total_bytes,
        final_path: finalized.final_path,
    })
}

/// Execute one claimed task, resuming persisted segments when present.
///
/// A pause or cancel request is cooperative: workers stop at the next streamed
/// chunk, persist their progress, and the task is left paused or cancelled
/// before this function returns. Real download failures mark the task failed
/// and preserve its part file.
pub async fn download_task(
    store: &Database,
    task_id: &str,
    dest_dir: &Path,
    spec: &DownloadSpec,
    control: &TaskControl,
) -> Result<TaskRunOutcome, EngineError> {
    if let Some(flow) = requested_flow(control) {
        return finish_interruption(store, task_id, dest_dir, flow);
    }

    let task = load_task(store, task_id)?;

    let mut segments = store.segments_for_task(task_id)?;
    if segments.is_empty() {
        store.replace_segments(
            task_id,
            &[SegmentInput {
                index: 0,
                start: 0,
                end: OPEN_ENDED_END.max(2),
                downloaded: 0,
                state: SegmentState::Pending,
            }],
        )?;
        segments = store.segments_for_task(task_id)?;
    }

    if task.state == TaskState::Queued {
        store.transition_task(task_id, TaskState::Queued, TaskState::Connecting)?;
    }
    let stored = load_task(store, task_id)?;
    if stored.state == TaskState::Connecting {
        store.transition_task(task_id, TaskState::Connecting, TaskState::Downloading)?;
    } else if stored.state != TaskState::Downloading {
        return Err(EngineError::invalid_state(
            "only a connecting task can start downloading",
        ));
    }

    if let Some(flow) = requested_flow(control) {
        return finish_interruption(store, task_id, dest_dir, flow);
    }

    let cap = CapDir::open(dest_dir).map_err(platform_error)?;
    let part_name = PartFile::part_name(&spec.filename);
    let part = PartFile::create(&cap, &spec.filename, None)?;
    store.record_part_path(task_id, &part_name)?;

    let mut values = vec![0u64; segments.len()];
    for segment in &segments {
        if let Some(slot) = values.get_mut(segment.index as usize) {
            *slot = segment.downloaded_bytes;
        }
    }
    let progress = Mutex::new(values);

    for segment in &segments {
        if segment.state == SegmentState::Done {
            continue;
        }
        if segment.state != SegmentState::Pending {
            let _ = store.transition_segment(
                task_id,
                segment.index,
                segment.state,
                SegmentState::Pending,
            );
        }
    }

    let client = build_client()?;
    let pending: Vec<_> = segments
        .iter()
        .filter(|segment| segment.state != SegmentState::Done)
        .collect();
    let mut interrupted = None;
    if !pending.is_empty() {
        let concurrency = (spec.connections as usize).max(1).min(pending.len());
        let mut results = stream::iter(pending.into_iter().map(|segment| {
            let start = segment.start.saturating_add(segment.downloaded_bytes);
            let (request, limit_end) = if segment.end >= OPEN_ENDED_END {
                (RequestShape::Range { start, end: None }, None)
            } else {
                (
                    RequestShape::Range {
                        start,
                        end: Some(segment.end),
                    },
                    Some(segment.end),
                )
            };
            run_worker(
                &client,
                store,
                task_id,
                &part,
                &progress,
                spec,
                control,
                request,
                segment.index,
                segment.start,
                limit_end,
            )
        }))
        .buffer_unordered(concurrency);

        while let Some(result) = results.next().await {
            match result {
                Ok(WorkerFlow::Complete) => {}
                Ok(flow) => interrupted = Some(flow),
                Err(error) => {
                    if control.cancel_requested() {
                        interrupted = Some(WorkerFlow::Cancelled);
                    } else if control.pause_requested() {
                        interrupted = Some(WorkerFlow::Paused);
                    } else {
                        return Err(mark_failed(store, task_id, error));
                    }
                }
            }
        }
    }

    if let Some(flow) = interrupted {
        drop(part);
        return finish_interruption(store, task_id, dest_dir, flow);
    }
    if let Some(flow) = requested_flow(control) {
        drop(part);
        return finish_interruption(store, task_id, dest_dir, flow);
    }

    part.sync_all()
        .map_err(|error| mark_failed(store, task_id, error))?;
    store.transition_task(task_id, TaskState::Downloading, TaskState::Finalizing)?;
    drop(part);

    let finalized = finalize_task(store, task_id, dest_dir, spec.expected_sha256.as_deref())?;
    let task = load_task(store, task_id)?;
    let actual_sha256 = task.actual_sha256.ok_or_else(|| {
        EngineError::new(
            "CHECKSUM_MISSING",
            "the completed task did not store its SHA-256",
        )
    })?;
    let total_bytes = std::fs::metadata(&finalized.final_path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);

    Ok(TaskRunOutcome::Completed(DownloadOutcome {
        actual_sha256,
        total_bytes,
        final_path: finalized.final_path,
    }))
}

fn finish_interruption(
    store: &Database,
    task_id: &str,
    dest_dir: &Path,
    flow: WorkerFlow,
) -> Result<TaskRunOutcome, EngineError> {
    match flow {
        WorkerFlow::Paused => {
            persist_pause(store, task_id)?;
            Ok(TaskRunOutcome::Paused)
        }
        WorkerFlow::Cancelled => {
            cancel_task(store, task_id, dest_dir)?;
            Ok(TaskRunOutcome::Cancelled)
        }
        WorkerFlow::Complete => Ok(TaskRunOutcome::Paused),
    }
}

pub(crate) fn persist_pause(store: &Database, task_id: &str) -> Result<(), EngineError> {
    let segments = store.segments_for_task(task_id)?;
    let inputs: Vec<SegmentProgressInput> = segments
        .iter()
        .map(|segment| SegmentProgressInput {
            segment_id: segment.index,
            downloaded_bytes: segment.downloaded_bytes,
        })
        .collect();
    let total: u64 = inputs.iter().map(|segment| segment.downloaded_bytes).sum();
    store.persist_paused_progress(task_id, &inputs, total)
}

pub(crate) fn cancel_task(
    store: &Database,
    task_id: &str,
    dest_dir: &Path,
) -> Result<(), EngineError> {
    let task = load_task(store, task_id)?;
    let part_name = PartFile::part_name(&task.filename);
    match std::fs::remove_file(dest_dir.join(&part_name)) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(EngineError::new(
                "IO_ERROR",
                format!("the part file could not be removed: {error}"),
            ))
        }
    }

    let task = load_task(store, task_id)?;
    if task.state != TaskState::Cancelled {
        if !task.state.can_transition_to(TaskState::Cancelled) {
            return Err(EngineError::invalid_state(
                "the task cannot be cancelled in its current state",
            ));
        }
        store.transition_task(task_id, task.state, TaskState::Cancelled)?;
    }
    Ok(())
}

/// Finalize a task that is already in the finalizing state.
///
/// Repeating the call on a completed task returns the same final path without
/// network access. A final-only crash state is recovered by hashing the final
/// file, and a missing part and final fails with `PART_MISSING`.
pub fn finalize_task(
    store: &Database,
    task_id: &str,
    dest_dir: &Path,
    expected_sha256: Option<&str>,
) -> Result<FinalizeOutcome, EngineError> {
    let task = load_task(store, task_id)?;
    let cap = CapDir::open(dest_dir).map_err(platform_error)?;
    let expected = expected_sha256.or(task.expected_sha256.as_deref());

    if task.state == TaskState::Completed {
        let name = find_completed_final(&cap, &task)?;
        return Ok(FinalizeOutcome {
            final_path: dest_dir.join(name),
        });
    }
    if task.state != TaskState::Finalizing {
        return Err(EngineError::invalid_state(
            "only a finalizing task can be finalized",
        ));
    }

    let part_name = PartFile::part_name(&task.filename);
    let part_exists = exists(&cap, &part_name);
    let final_exists = exists(&cap, &task.filename);

    if !part_exists {
        if final_exists {
            let actual = hash_cap_file(&cap, &task.filename)?;
            if let Some(total) = task.total_bytes {
                if file_len(&cap, &task.filename)? != total {
                    return Err(mark_failed(
                        store,
                        task_id,
                        checksum_mismatch("the final file size does not match the recorded total"),
                    ));
                }
            }
            if expected.is_some_and(|expected| !hash_matches(&actual, expected)) {
                return Err(mark_failed(
                    store,
                    task_id,
                    checksum_mismatch("the final file does not match the expected SHA-256"),
                ));
            }
            store.record_actual_hash(task_id, &actual)?;
            store.complete_task(task_id, Some(&actual))?;
            return Ok(FinalizeOutcome {
                final_path: dest_dir.join(&task.filename),
            });
        }
        return Err(mark_failed(
            store,
            task_id,
            EngineError::new(
                "PART_MISSING",
                "the part file and the final file are both missing",
            ),
        ));
    }

    let part = PartFile::open(&cap, &task.filename)?;
    part.sync_all()?;
    let actual = part.try_sha256()?;
    drop(part);

    store.record_actual_hash(task_id, &actual)?;
    if expected.is_some_and(|expected| !hash_matches(&actual, expected)) {
        return Err(mark_failed(
            store,
            task_id,
            checksum_mismatch("the downloaded bytes do not match the expected SHA-256"),
        ));
    }

    let chosen = unique_final_name(&cap, &task.filename);
    cap.replace_blocking(&part_name, &chosen)
        .map_err(platform_error)
        .map_err(|error| mark_failed(store, task_id, error))?;
    cap.sync_dir().map_err(platform_error)?;
    store.complete_task(task_id, Some(&actual))?;

    Ok(FinalizeOutcome {
        final_path: dest_dir.join(chosen),
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_worker(
    client: &Client,
    store: &Database,
    task_id: &str,
    part: &PartFile,
    progress: &Mutex<Vec<u64>>,
    spec: &DownloadSpec,
    control: &TaskControl,
    request: RequestShape,
    segment_index: u32,
    segment_start: u64,
    limit_end: Option<u64>,
) -> Result<WorkerFlow, EngineError> {
    if let Some(flow) = requested_flow(control) {
        return Ok(flow);
    }
    store.transition_segment(
        task_id,
        segment_index,
        SegmentState::Pending,
        SegmentState::Active,
    )?;

    let progress_index = segment_index as usize;
    let result = stream_segment(
        client,
        store,
        task_id,
        part,
        progress,
        spec,
        control,
        request,
        progress_index,
        segment_start,
        limit_end,
    )
    .await;

    match result {
        Ok(WorkerFlow::Complete) => {
            store.transition_segment(
                task_id,
                segment_index,
                SegmentState::Active,
                SegmentState::Done,
            )?;
            Ok(WorkerFlow::Complete)
        }
        Ok(flow) => {
            let _ = store.transition_segment(
                task_id,
                segment_index,
                SegmentState::Active,
                SegmentState::Pending,
            );
            Ok(flow)
        }
        Err(error) => {
            let _ = store.transition_segment(
                task_id,
                segment_index,
                SegmentState::Active,
                SegmentState::Failed,
            );
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn stream_segment(
    client: &Client,
    store: &Database,
    task_id: &str,
    part: &PartFile,
    progress: &Mutex<Vec<u64>>,
    spec: &DownloadSpec,
    control: &TaskControl,
    request: RequestShape,
    progress_index: usize,
    segment_start: u64,
    limit_end: Option<u64>,
) -> Result<WorkerFlow, EngineError> {
    match request {
        RequestShape::Range { start, end } => {
            let mut position = start;
            loop {
                if let Some(flow) = requested_flow(control) {
                    return Ok(flow);
                }
                let chunk_end = match end {
                    Some(end) if position >= end => return Ok(WorkerFlow::Complete),
                    Some(end) => (position + READ_CHUNK_BYTES).min(end),
                    None => position + READ_CHUNK_BYTES,
                };
                let chunk = RequestShape::Range {
                    start: position,
                    end: Some(chunk_end),
                };
                let (written, flow, total) = stream_request(
                    client,
                    store,
                    task_id,
                    part,
                    progress,
                    spec,
                    control,
                    chunk,
                    Some(chunk_end),
                    segment_start,
                    progress_index,
                )
                .await?;
                if flow != WorkerFlow::Complete {
                    return Ok(flow);
                }
                if end.is_some_and(|end| written >= end) {
                    return Ok(WorkerFlow::Complete);
                }
                if total.is_some_and(|total| written >= total) {
                    return Ok(WorkerFlow::Complete);
                }
                if written <= position {
                    return Err(EngineError::retryable(
                        "INCOMPLETE_BODY",
                        "the server closed the response before the range was complete",
                    ));
                }
                position = written;
            }
        }
        request => {
            let (_, flow, _) = stream_request(
                client,
                store,
                task_id,
                part,
                progress,
                spec,
                control,
                request,
                limit_end,
                segment_start,
                progress_index,
            )
            .await?;
            Ok(flow)
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn stream_request(
    client: &Client,
    store: &Database,
    task_id: &str,
    part: &PartFile,
    progress: &Mutex<Vec<u64>>,
    spec: &DownloadSpec,
    control: &TaskControl,
    request: RequestShape,
    limit_end: Option<u64>,
    segment_start: u64,
    progress_index: usize,
) -> Result<(u64, WorkerFlow, Option<u64>), EngineError> {
    let expected_start = match request {
        RequestShape::Range { start, .. } => start,
        RequestShape::Plain => 0,
    };
    let mut range_total = None;
    let mut attempts = 0usize;
    let response = loop {
        if let Some(flow) = requested_flow(control) {
            return Ok((expected_start, flow, range_total));
        }
        attempts += 1;
        let headers = request_headers(spec, request.range_header())?;
        let (response, _final_url) = match get_following_redirects(client, &spec.url, headers).await
        {
            Ok(response) => response,
            Err(error) => {
                if request.expects_partial() && attempts < RANGE_REQUEST_ATTEMPTS {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    continue;
                }
                return Err(error);
            }
        };
        let status = response.status();

        if request.expects_partial() {
            if status == StatusCode::OK {
                if attempts < RANGE_REQUEST_ATTEMPTS {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    continue;
                }
                return Err(EngineError::new(
                    "RANGE_IGNORED",
                    "the server ignored the requested byte range",
                ));
            }
            if status != StatusCode::PARTIAL_CONTENT {
                return Err(status_error(status));
            }
            let (start, total) = parse_content_range(&response)?;
            if start != expected_start {
                return Err(EngineError::new(
                    "RANGE_MISMATCH",
                    "the server returned a different range than requested",
                ));
            }
            range_total = total;
        } else if status != StatusCode::OK {
            return Err(status_error(status));
        } else {
            range_total = response.content_length();
        }
        break response;
    };

    let write_start = match request {
        RequestShape::Range { start, .. } => start,
        RequestShape::Plain => 0,
    };
    let mut position = write_start;
    let mut last_persisted = 0u64;
    let mut body = response.bytes_stream();

    loop {
        if let Some(flow) = requested_flow(control) {
            let downloaded = position - segment_start;
            persist_progress(store, task_id, progress, progress_index, downloaded)?;
            return Ok((position, flow, range_total));
        }
        let Some(chunk) = body.next().await else {
            break;
        };
        let chunk = chunk.map_err(|_| {
            EngineError::retryable("NETWORK_INTERRUPTED", "the response stream was interrupted")
        })?;
        let mut bytes: &[u8] = &chunk;
        if let Some(end) = limit_end {
            let remaining = end.saturating_sub(position);
            if remaining == 0 {
                break;
            }
            if bytes.len() as u64 > remaining {
                bytes = &bytes[..remaining as usize];
            }
        }
        if !bytes.is_empty() {
            part.write_at(position, bytes)?;
            position += bytes.len() as u64;
        }
        let downloaded = position - segment_start;
        if downloaded >= last_persisted + PROGRESS_INTERVAL {
            persist_progress(store, task_id, progress, progress_index, downloaded)?;
            last_persisted = downloaded;
        }
        if limit_end.is_some_and(|end| position >= end) {
            break;
        }
    }

    if let Some(end) = limit_end {
        let reached_total = range_total.is_some_and(|total| position >= total);
        if position < end && !reached_total {
            return Err(EngineError::retryable(
                "INCOMPLETE_BODY",
                "the server closed the response before the range was complete",
            ));
        }
    }
    let downloaded = position - segment_start;
    persist_progress(store, task_id, progress, progress_index, downloaded)?;
    Ok((position, WorkerFlow::Complete, range_total))
}

fn request_headers(spec: &DownloadSpec, range: Option<String>) -> Result<HeaderMap, EngineError> {
    let mut headers = HeaderMap::new();
    for (name, value) in &spec.extra_headers {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|_| {
            EngineError::new("INVALID_HEADER", "an extra request header is not valid")
        })?;
        let value = HeaderValue::from_str(value).map_err(|_| {
            EngineError::new(
                "INVALID_HEADER",
                "an extra request header value is not valid",
            )
        })?;
        headers.insert(name, value);
    }
    headers.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    if let Some(range) = range {
        let value = HeaderValue::from_str(&range)
            .map_err(|_| EngineError::new("INVALID_HEADER", "the range header is not valid"))?;
        headers.insert(RANGE, value);
    }
    Ok(headers)
}

fn parse_content_range(response: &reqwest::Response) -> Result<(u64, Option<u64>), EngineError> {
    let value = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(invalid_response)?;
    let (unit, rest) = value.split_once(' ').ok_or_else(invalid_response)?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return Err(invalid_response());
    }
    let (range, total) = rest.trim().split_once('/').ok_or_else(invalid_response)?;
    let (start, end) = range.split_once('-').ok_or_else(invalid_response)?;
    let start: u64 = start.trim().parse().map_err(|_| invalid_response())?;
    let end: u64 = end.trim().parse().map_err(|_| invalid_response())?;
    if end < start {
        return Err(invalid_response());
    }
    let total = if total.trim() == "*" {
        None
    } else {
        Some(total.trim().parse().map_err(|_| invalid_response())?)
    };
    Ok((start, total))
}

fn persist_progress(
    store: &Database,
    task_id: &str,
    progress: &Mutex<Vec<u64>>,
    index: usize,
    downloaded: u64,
) -> Result<(), EngineError> {
    let snapshot = {
        let mut values = progress.lock().map_err(|_| {
            EngineError::new("INTERNAL_ERROR", "the segment progress lock was poisoned")
        })?;
        values[index] = downloaded;
        values.clone()
    };
    let inputs: Vec<SegmentProgressInput> = snapshot
        .iter()
        .enumerate()
        .map(|(position, downloaded_bytes)| SegmentProgressInput {
            segment_id: position as u32,
            downloaded_bytes: *downloaded_bytes,
        })
        .collect();
    let total: u64 = snapshot.iter().sum();
    store.persist_progress(task_id, &inputs, total)
}

fn segment_inputs(plan: &SegmentPlan, total: Option<u64>) -> Vec<SegmentInput> {
    match plan {
        SegmentPlan::Ranged(segments) => segments
            .iter()
            .map(|segment| SegmentInput {
                index: segment.index,
                start: segment.start,
                end: segment.end,
                downloaded: 0,
                state: SegmentState::Pending,
            })
            .collect(),
        SegmentPlan::OpenEnded { start } => {
            let end = total.unwrap_or(OPEN_ENDED_END).max(start + 1);
            vec![SegmentInput {
                index: 0,
                start: *start,
                end,
                downloaded: 0,
                state: SegmentState::Pending,
            }]
        }
    }
}

async fn probe_zero_length(
    url: &str,
    options: &ProbeOptions,
) -> Result<DownloadMetadata, EngineError> {
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

    let (response, _final_url) = get_following_redirects(&client, url, headers).await?;
    if response.status() != StatusCode::OK || response.content_length() != Some(0) {
        return Err(invalid_response());
    }

    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    if !options.allow_html && content_type.is_some_and(is_html) {
        return Err(EngineError::new(
            "HTML_RESPONSE",
            "the server returned an HTML page instead of a downloadable file",
        ));
    }

    Ok(DownloadMetadata {
        total_bytes: Some(0),
        range_supported: false,
        etag: header_text(&response, ETAG),
        last_modified: header_text(&response, LAST_MODIFIED),
    })
}

fn header_text(response: &reqwest::Response, name: reqwest::header::HeaderName) -> Option<String> {
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

fn status_error(status: StatusCode) -> EngineError {
    match status.as_u16() {
        401 | 403 => EngineError::new("AUTH_REQUIRED", "the server requires authorization"),
        404 => EngineError::new("NOT_FOUND", "the remote file was not found"),
        408 => EngineError::retryable("TIMEOUT", "the server reported a request timeout"),
        429 => EngineError::retryable("RATE_LIMITED", "the server asked the client to slow down"),
        _ if status.is_server_error() => {
            EngineError::retryable("SERVER_ERROR", "the server failed to serve the file")
        }
        _ => invalid_response(),
    }
}

fn invalid_response() -> EngineError {
    EngineError::new("INVALID_RESPONSE", "the server response was malformed")
}

fn checksum_mismatch(message: &str) -> EngineError {
    EngineError::new("CHECKSUM_MISMATCH", message)
}

fn hash_matches(actual: &str, expected: &str) -> bool {
    actual.eq_ignore_ascii_case(expected.trim())
}

fn mark_failed(store: &Database, task_id: &str, error: EngineError) -> EngineError {
    if let Ok(Some(task)) = store.task(task_id) {
        if task.state.can_transition_to(TaskState::Failed) {
            let _ = store.transition_task(task_id, task.state, TaskState::Failed);
        }
        let _ = store.update_task_error(task_id, error.code(), &error.to_string());
    }
    error
}

fn load_task(store: &Database, task_id: &str) -> Result<Task, EngineError> {
    store
        .task(task_id)?
        .ok_or_else(|| EngineError::new("TASK_NOT_FOUND", "the requested task does not exist"))
}

fn find_completed_final(cap: &CapDir, task: &Task) -> Result<String, EngineError> {
    let expected = task.actual_sha256.as_deref();
    for name in candidate_names(&task.filename) {
        if !exists(cap, &name) {
            continue;
        }
        if let Some(total) = task.total_bytes {
            if file_len(cap, &name)? != total {
                continue;
            }
        }
        match expected {
            Some(expected) => {
                let actual = hash_cap_file(cap, &name)?;
                if hash_matches(&actual, expected) {
                    return Ok(name);
                }
            }
            None => return Ok(name),
        }
    }
    Err(EngineError::new(
        "PART_MISSING",
        "the completed final file could not be found",
    ))
}

fn unique_final_name(cap: &CapDir, filename: &str) -> String {
    if !exists(cap, filename) {
        return filename.to_string();
    }
    let (stem, extension) = split_filename(filename);
    for suffix in 1..=10_000u32 {
        let candidate = format!("{stem} ({suffix}){extension}");
        if !exists(cap, &candidate) {
            return candidate;
        }
    }
    format!("{stem} ({}){extension}", uuid::Uuid::new_v4())
}

fn candidate_names(filename: &str) -> Vec<String> {
    let (stem, extension) = split_filename(filename);
    let mut names = Vec::with_capacity(1_001);
    names.push(filename.to_string());
    for suffix in 1..=1_000u32 {
        names.push(format!("{stem} ({suffix}){extension}"));
    }
    names
}

fn split_filename(filename: &str) -> (String, String) {
    match filename.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem.to_string(), format!(".{extension}")),
        _ => (filename.to_string(), String::new()),
    }
}

fn exists(cap: &CapDir, name: &str) -> bool {
    std::fs::symlink_metadata(cap.root().join(name)).is_ok()
}

fn file_len(cap: &CapDir, name: &str) -> Result<u64, EngineError> {
    let file = cap.open_read(name).map_err(platform_error)?;
    file.metadata()
        .map(|metadata| metadata.len())
        .map_err(|error| {
            EngineError::new("IO_ERROR", format!("the file could not be read: {error}"))
        })
}

fn hash_cap_file(cap: &CapDir, name: &str) -> Result<String, EngineError> {
    let mut file = cap.open_read(name).map_err(platform_error)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            EngineError::new(
                "IO_ERROR",
                format!("the final file could not be read: {error}"),
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn platform_error(error: PlatformError) -> EngineError {
    EngineError::new(error.code, error.message)
}
