//! Bounded multi-connection download execution and atomic finalization.
//!
//! A task probes the remote file, plans its segments, writes into one part file
//! at absolute offsets, and then finalizes by syncing, hashing, choosing a
//! non-conflicting final name, and atomically renaming the part file.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::stream::FuturesUnordered;
use futures_util::{stream, StreamExt};
use reqwest::header::{
    HeaderMap, HeaderName, HeaderValue, ACCEPT_ENCODING, CONTENT_RANGE, CONTENT_TYPE, ETAG,
    LAST_MODIFIED, RANGE,
};
use reqwest::{Client, StatusCode};
use sha2::{Digest, Sha256};

use crate::platform::files::CapDir;
use crate::platform::PlatformError;
use crate::store::types::{Segment, SegmentInput, SegmentProgressInput, Task};
use crate::store::Database;

use super::cookies::CookieJar;
use super::error::EngineError;
use super::http::{build_client, get_following_redirects_with_cookies, storage_url};
use super::part_file::PartFile;
use super::planner::{plan_segments, SegmentPlan, MIN_SEGMENT_BYTES};
use super::probe::{invalid_response, is_html, probe, ProbeOptions, ProbeOutcome};
use super::rate_limit::RateLimiter;
use super::retry;
use super::state::{SegmentState, TaskState};

/// Progress is persisted at least once per this many downloaded bytes.
///
/// One read chunk keeps the persisted snapshot close to the bytes actually
/// written, which bounds crash re-download and makes progress observable while
/// a multi-connection task is still running.
const PROGRESS_INTERVAL: u64 = READ_CHUNK_BYTES;

/// Each ranged segment is fetched as bounded reads of this size.
const READ_CHUNK_BYTES: u64 = 128 * 1024;

/// Bounded replays for a ranged read that resets before the range completes.
/// Every replay resumes from the current persisted offset.
const RANGE_REQUEST_ATTEMPTS: usize = 8;

/// Bounded probe and first-range revalidation attempts.
const PROBE_ATTEMPTS: usize = retry::MAX_ATTEMPTS;

/// Upper bound for the stored end of an unknown-length segment.
const OPEN_ENDED_END: u64 = i64::MAX as u64;

/// Bytes requested when revalidating a planned range before the worker pool.
///
/// The prefix is deliberately small so the check costs one short request and
/// is not mistaken for the probe's single-byte `bytes=0-0` request.
const REVALIDATION_PREFIX_BYTES: u64 = 64 * 1024;

/// Maximum number of times one segment may be split cooperatively.
const MAX_SPLIT_DEPTH: u8 = 2;

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
    /// Destination directory for this task, or `None` for the engine default.
    pub destination: Option<PathBuf>,
}

/// HTTP and pacing configuration shared by one task execution.
pub(crate) struct NetworkContext {
    /// Client that applies the configured proxy.
    pub client: Client,
    /// Cookies reloaded for this task run, when a cookie file is configured.
    pub cookies: Option<CookieJar>,
    /// Per-task and global rate limiters that pace every streamed chunk.
    pub limiters: Vec<Arc<RateLimiter>>,
}

impl NetworkContext {
    /// A proxy-free, cookie-free, unlimited context for direct callers.
    pub(crate) fn plain() -> Result<Self, EngineError> {
        Ok(Self {
            client: build_client()?,
            cookies: None,
            limiters: Vec::new(),
        })
    }

    async fn pace(&self, bytes: usize) {
        for limiter in &self.limiters {
            limiter.acquire(bytes as u64).await;
        }
    }
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
    /// A ranged request received a full response before any task bytes were
    /// committed, so the task must restart as one plain connection.
    RangeFallback,
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
    final_url: Option<String>,
}

impl From<ProbeOutcome> for DownloadMetadata {
    fn from(outcome: ProbeOutcome) -> Self {
        Self {
            total_bytes: outcome.total_bytes,
            range_supported: outcome.range_supported,
            etag: outcome.etag,
            last_modified: outcome.last_modified,
            final_url: Some(outcome.final_url),
        }
    }
}

/// Remote metadata compared when revalidating a paused task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteMetadata {
    /// Total file size when the server reports it.
    pub total_bytes: Option<u64>,
    /// Response entity tag.
    pub etag: Option<String>,
    /// Response last-modified value.
    pub last_modified: Option<String>,
}

/// Revalidate a paused task's remote metadata through the engine network.
pub(crate) async fn remote_metadata(
    network: &NetworkContext,
    spec: &DownloadSpec,
    start: u64,
) -> Result<RemoteMetadata, EngineError> {
    let (metadata, _) =
        probe_for_download(&network.client, network.cookies.as_ref(), spec, start).await?;
    Ok(RemoteMetadata {
        total_bytes: metadata.total_bytes,
        etag: metadata.etag,
        last_modified: metadata.last_modified,
    })
}

/// One segment as it is currently being downloaded.
#[derive(Debug, Clone)]
struct SegmentRuntime {
    index: u32,
    start: u64,
    end: u64,
    pos: u64,
    depth: u8,
    state: SegmentState,
    /// Whether a worker currently owns this segment.
    active: bool,
}

impl SegmentRuntime {
    fn downloaded(&self) -> u64 {
        self.pos
            .saturating_sub(self.start)
            .min(self.end.saturating_sub(self.start))
    }

    fn remaining(&self) -> u64 {
        self.end.saturating_sub(self.pos)
    }
}

/// Shared view of a task's active segments.
///
/// Offsets are updated under the registry lock, so a cooperating split can
/// shorten a segment without a worker ever writing past the new end.
#[derive(Debug, Default)]
struct SegmentRegistry {
    segments: Vec<SegmentRuntime>,
    next_index: u32,
    /// Set when a ranged request received a full response before any bytes were
    /// committed; the pool then restarts as one plain connection.
    fallback: bool,
}

impl SegmentRegistry {
    fn from_segments(segments: &[Segment]) -> Self {
        let mut runtime = Vec::with_capacity(segments.len());
        let mut next_index = 0u32;
        for segment in segments {
            next_index = next_index.max(segment.index.saturating_add(1));
            let length = segment.end.saturating_sub(segment.start);
            let (state, pos) =
                if segment.state == SegmentState::Done || segment.downloaded_bytes >= length {
                    (SegmentState::Done, segment.end)
                } else {
                    (
                        SegmentState::Pending,
                        segment.start.saturating_add(segment.downloaded_bytes),
                    )
                };
            runtime.push(SegmentRuntime {
                index: segment.index,
                start: segment.start,
                end: segment.end,
                pos,
                depth: 0,
                state,
                active: false,
            });
        }
        Self {
            segments: runtime,
            next_index,
            fallback: false,
        }
    }

    fn committed_bytes(&self) -> u64 {
        self.segments.iter().map(SegmentRuntime::downloaded).sum()
    }

    /// Mark the next not-yet-owned segment as taken and return its index.
    fn take_pending(&mut self) -> Option<u32> {
        let segment = self
            .segments
            .iter_mut()
            .find(|segment| segment.state == SegmentState::Pending && !segment.active)?;
        segment.active = true;
        Some(segment.index)
    }

    fn to_inputs(&self) -> Vec<SegmentInput> {
        self.segments
            .iter()
            .map(|segment| SegmentInput {
                index: segment.index,
                start: segment.start,
                end: segment.end,
                downloaded: segment.downloaded(),
                state: segment.state,
            })
            .collect()
    }
}

/// Where a streamed body writes its bytes.
#[derive(Debug, Clone, Copy)]
enum WriteTarget<'a> {
    /// Fixed-range execution that persists through a progress vector.
    Static {
        progress: &'a Mutex<Vec<u64>>,
        progress_index: usize,
        limit_end: Option<u64>,
    },
    /// Registry-backed execution that supports cooperative splitting.
    Dynamic {
        registry: &'a Mutex<SegmentRegistry>,
        segment_index: u32,
    },
}

/// Outcome of writing one streamed chunk into a dynamic segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChunkWrite {
    Written(usize),
    Closed,
    Fallback,
}

/// How one execution of the dynamic worker pool ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoolOutcome {
    Completed,
    Interrupted(WorkerFlow),
    Fallback,
}

fn lock_registry(
    registry: &Mutex<SegmentRegistry>,
) -> Result<std::sync::MutexGuard<'_, SegmentRegistry>, EngineError> {
    registry
        .lock()
        .map_err(|_| EngineError::new("INTERNAL_ERROR", "the segment registry lock was poisoned"))
}

fn registry_limit(
    registry: &Mutex<SegmentRegistry>,
    segment_index: u32,
) -> Result<Option<u64>, EngineError> {
    let registry = lock_registry(registry)?;
    let segment = registry
        .segments
        .iter()
        .find(|segment| segment.index == segment_index)
        .ok_or_else(|| {
            EngineError::new(
                "INTERNAL_ERROR",
                "the requested segment is missing from the registry",
            )
        })?;
    Ok(if segment.end >= OPEN_ENDED_END {
        None
    } else {
        Some(segment.end)
    })
}

fn reset_registry_position(
    registry: &Mutex<SegmentRegistry>,
    segment_index: u32,
) -> Result<(), EngineError> {
    let mut registry = lock_registry(registry)?;
    if let Some(segment) = registry
        .segments
        .iter_mut()
        .find(|segment| segment.index == segment_index)
    {
        segment.pos = segment.start;
    }
    Ok(())
}

fn set_dynamic_segment_state(
    store: &Database,
    registry: &Mutex<SegmentRegistry>,
    task_id: &str,
    segment_index: u32,
    from: SegmentState,
    to: SegmentState,
) -> Result<(), EngineError> {
    let mut registry = lock_registry(registry)?;
    store.transition_segment(task_id, segment_index, from, to)?;
    if let Some(segment) = registry
        .segments
        .iter_mut()
        .find(|segment| segment.index == segment_index)
    {
        segment.state = to;
        if to == SegmentState::Done {
            segment.pos = segment.end;
        }
        if to != SegmentState::Active {
            segment.active = false;
        }
    }
    Ok(())
}

fn write_dynamic_chunk(
    registry: &Mutex<SegmentRegistry>,
    part: &PartFile,
    segment_index: u32,
    position: u64,
    bytes: &[u8],
) -> Result<ChunkWrite, EngineError> {
    // The lock is held across the write so a concurrent split cannot shorten the
    // end between the remaining-bytes check and the write, and progress never
    // claims bytes that have not reached the file.
    let mut registry = lock_registry(registry)?;
    if registry.fallback {
        return Ok(ChunkWrite::Fallback);
    }
    let segment = registry
        .segments
        .iter_mut()
        .find(|segment| segment.index == segment_index)
        .ok_or_else(|| {
            EngineError::new(
                "INTERNAL_ERROR",
                "the requested segment is missing from the registry",
            )
        })?;
    let remaining = segment.end.saturating_sub(position);
    if remaining == 0 {
        return Ok(ChunkWrite::Closed);
    }
    let take = (bytes.len() as u64).min(remaining) as usize;
    if take > 0 {
        part.write_at(position, &bytes[..take])?;
        segment.pos = position + take as u64;
    }
    Ok(ChunkWrite::Written(take))
}

fn persist_registry_progress(
    store: &Database,
    task_id: &str,
    registry: &Mutex<SegmentRegistry>,
) -> Result<(), EngineError> {
    let (inputs, total) = {
        let registry = lock_registry(registry)?;
        let inputs: Vec<SegmentProgressInput> = registry
            .segments
            .iter()
            .map(|segment| SegmentProgressInput {
                segment_id: segment.index,
                downloaded_bytes: segment.downloaded(),
            })
            .collect();
        let total = registry
            .segments
            .iter()
            .map(SegmentRuntime::downloaded)
            .sum();
        (inputs, total)
    };
    store.persist_progress(task_id, &inputs, total)
}

fn persist_target_progress(
    store: &Database,
    task_id: &str,
    target: &WriteTarget<'_>,
    position: u64,
    segment_start: u64,
) -> Result<(), EngineError> {
    match target {
        WriteTarget::Static {
            progress,
            progress_index,
            ..
        } => persist_progress(
            store,
            task_id,
            progress,
            *progress_index,
            position.saturating_sub(segment_start),
        ),
        WriteTarget::Dynamic { registry, .. } => {
            persist_registry_progress(store, task_id, registry)
        }
    }
}

/// Map an `INVALID_STATE` failure to the interruption a control from another
/// engine connection caused.
///
/// A second connection can move the task to paused or cancelled while this
/// engine's workers still hold the row's previous state. Reloading the row
/// distinguishes that expected race from a real state error.
fn interruption_flow(
    store: &Database,
    task_id: &str,
    error: &EngineError,
) -> Result<Option<WorkerFlow>, EngineError> {
    if error.code() != "INVALID_STATE" {
        return Ok(None);
    }
    Ok(match load_task(store, task_id)?.state {
        TaskState::Paused => Some(WorkerFlow::Paused),
        TaskState::Cancelled => Some(WorkerFlow::Cancelled),
        _ => None,
    })
}

/// Persist progress, converting a cross-connection state change into a flow.
fn persist_target_progress_or_flow(
    store: &Database,
    task_id: &str,
    target: &WriteTarget<'_>,
    position: u64,
    segment_start: u64,
) -> Result<Option<WorkerFlow>, EngineError> {
    match persist_target_progress(store, task_id, target, position, segment_start) {
        Ok(()) => Ok(None),
        Err(error) => match interruption_flow(store, task_id, &error)? {
            Some(flow) => Ok(Some(flow)),
            None => Err(error),
        },
    }
}

/// Split the active segment with the most remaining bytes, persist the
/// shortened segment and its tail in one transaction, and return the tail
/// index for an idle worker.
fn split_and_persist(
    store: &Database,
    task_id: &str,
    registry: &Mutex<SegmentRegistry>,
) -> Result<Option<u32>, EngineError> {
    let mut registry = lock_registry(registry)?;
    if registry.fallback {
        return Ok(None);
    }

    let mut chosen: Option<usize> = None;
    for (position, segment) in registry.segments.iter().enumerate() {
        if !segment.active || segment.state != SegmentState::Active {
            continue;
        }
        if segment.depth >= MAX_SPLIT_DEPTH || segment.end >= OPEN_ENDED_END {
            continue;
        }
        let remaining = segment.remaining();
        if remaining < 2 * MIN_SEGMENT_BYTES {
            continue;
        }
        if chosen.is_none_or(|best| registry.segments[best].remaining() < remaining) {
            chosen = Some(position);
        }
    }
    let Some(chosen) = chosen else {
        return Ok(None);
    };

    let (_, end, depth) = {
        let segment = &registry.segments[chosen];
        (segment.index, segment.end, segment.depth)
    };
    let position = registry.segments[chosen].pos;
    let remaining = end.saturating_sub(position);
    if remaining < 2 * MIN_SEGMENT_BYTES {
        return Ok(None);
    }
    let split = position + remaining / 2;
    if split <= position || split >= end {
        return Ok(None);
    }

    let tail_index = registry.next_index;
    registry.next_index = registry.next_index.saturating_add(1);
    let next_depth = depth.saturating_add(1);
    registry.segments[chosen].end = split;
    registry.segments[chosen].depth = next_depth;
    registry.segments.push(SegmentRuntime {
        index: tail_index,
        start: split,
        end,
        pos: split,
        depth: next_depth,
        state: SegmentState::Pending,
        active: false,
    });

    let rows = registry.to_inputs();
    store.replace_segments(task_id, &rows)?;
    Ok(Some(tail_index))
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
    let network = NetworkContext::plain()?;

    let mut probe_attempts = 0;
    let metadata = loop {
        probe_attempts += 1;
        match probe(&spec.url, &options).await {
            Ok(outcome) => break DownloadMetadata::from(outcome),
            Err(error) if error.code() == "INVALID_RESPONSE" => {
                match probe_zero_length(
                    &network.client,
                    network.cookies.as_ref(),
                    &spec.url,
                    &options,
                )
                .await
                {
                    Ok(metadata) => break metadata,
                    Err(_) => return Err(mark_failed(store, task_id, error)),
                }
            }
            Err(error) if error.is_retryable() && probe_attempts < PROBE_ATTEMPTS => {
                tokio::time::sleep(retry::delay_for_attempt(
                    probe_attempts,
                    error.retry_after(),
                ))
                .await;
            }
            Err(error) => return Err(mark_failed(store, task_id, error)),
        }
    };

    if let Some(final_url) = metadata.final_url.as_deref() {
        store.record_final_url(task_id, &storage_url(final_url))?;
    }

    let mut plan = plan_segments(
        metadata.total_bytes,
        metadata.range_supported,
        spec.connections,
    )
    .map_err(|error| mark_failed(store, task_id, error))?;
    // A known empty total finalizes without workers regardless of range support.
    let mut range_supported = metadata.range_supported;
    if metadata.total_bytes == Some(0) {
        plan = SegmentPlan::Ranged(Vec::new());
    }

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

    let task = load_task(store, task_id)?;
    let cap = CapDir::open(dest_dir).map_err(platform_error)?;
    let part_name = task_part_name(&task);
    let part = PartFile::create_named(&cap, &part_name, metadata.total_bytes)?;
    if task.part_path.is_none() {
        store.record_part_path(task_id, &part_name)?;
    }

    if range_supported {
        if let SegmentPlan::Ranged(segments) = &plan {
            if let Some(first) = segments.first() {
                if first.end > first.start {
                    match check_first_range(&network, spec, first.start, first.end).await? {
                        RangeCheck::Accepted => {}
                        RangeCheck::Ignored => {
                            range_supported = false;
                            plan = SegmentPlan::OpenEnded { start: 0 };
                        }
                    }
                }
            }
        }
    }

    let segment_rows = segment_inputs(&plan, metadata.total_bytes);
    store.replace_segments(task_id, &segment_rows)?;
    let progress = Mutex::new(vec![0u64; segment_rows.len()]);

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
                    &network,
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
            let request = if range_supported {
                RequestShape::Range {
                    start: *start,
                    end: None,
                }
            } else {
                RequestShape::Plain
            };
            match run_worker(
                &network,
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

/// Execute one claimed task, probing, planning, and resuming segments.
///
/// A fresh task probes the remote file, records the discovered metadata, plans
/// and persists its segments, preallocates the part file, and runs up to the
/// configured number of workers. A resumed task re-probes the remote file and
/// rejects changed metadata before reusing persisted segment ranges and
/// offsets. A pause or cancel request is cooperative: workers stop at the next
/// streamed chunk, persist progress, and the task is left paused or cancelled
/// before this function returns. Real failures mark the task failed and
/// preserve its part file.
pub async fn download_task(
    store: &Database,
    task_id: &str,
    dest_dir: &Path,
    spec: &DownloadSpec,
    control: &TaskControl,
) -> Result<TaskRunOutcome, EngineError> {
    let network = NetworkContext::plain()?;
    download_task_with_network(store, task_id, dest_dir, spec, control, &network).await
}

/// Execute one task with a caller-supplied HTTP, cookie, and pacing context.
pub(crate) async fn download_task_with_network(
    store: &Database,
    task_id: &str,
    dest_dir: &Path,
    spec: &DownloadSpec,
    control: &TaskControl,
    network: &NetworkContext,
) -> Result<TaskRunOutcome, EngineError> {
    if let Some(flow) = requested_flow(control) {
        return finish_interruption(store, task_id, dest_dir, flow);
    }

    let task = load_task(store, task_id)?;

    let mut persisted = store.segments_for_task(task_id)?;
    if !persisted.is_empty() && !part_exists_for_task(dest_dir, &task) {
        return Err(mark_failed(store, task_id, part_missing()));
    }

    // The part file is created before the remote metadata is revalidated so a
    // scheduled task has a durable destination as soon as it reports active
    // work. A resumed part is opened without truncating persisted bytes.
    let cap = CapDir::open(dest_dir).map_err(platform_error)?;
    let part_name = task_part_name(&task);
    let part = PartFile::create_named(&cap, &part_name, None)?;
    if task.part_path.is_none() {
        store.record_part_path(task_id, &part_name)?;
    }

    let stored = load_task(store, task_id)?;
    if stored.state == TaskState::Queued {
        if let Err(error) = store.transition_task(task_id, TaskState::Queued, TaskState::Connecting)
        {
            let flow = interruption_flow(store, task_id, &error)?;
            drop(part);
            if let Some(flow) = flow {
                return finish_interruption(store, task_id, dest_dir, flow);
            }
            return Err(error);
        }
    }
    let stored = load_task(store, task_id)?;
    if stored.state == TaskState::Connecting {
        if let Err(error) =
            store.transition_task(task_id, TaskState::Connecting, TaskState::Downloading)
        {
            let flow = interruption_flow(store, task_id, &error)?;
            drop(part);
            if let Some(flow) = flow {
                return finish_interruption(store, task_id, dest_dir, flow);
            }
            return Err(error);
        }
    } else if stored.state != TaskState::Downloading {
        let error = EngineError::invalid_state("only a connecting task can start downloading");
        let flow = interruption_flow(store, task_id, &error)?;
        drop(part);
        if let Some(flow) = flow {
            return finish_interruption(store, task_id, dest_dir, flow);
        }
        return Err(error);
    }

    if let Some(flow) = requested_flow(control) {
        drop(part);
        return finish_interruption(store, task_id, dest_dir, flow);
    }

    let probe_start = persisted
        .iter()
        .find(|segment| segment.state != SegmentState::Done)
        .map(|segment| segment.start.saturating_add(segment.downloaded_bytes))
        .unwrap_or(0);
    let probe_start = match task.total_bytes {
        Some(total) if total > 0 && probe_start >= total => total - 1,
        _ => probe_start,
    };

    let mut probe_attempts = 0;
    let (metadata, probe_bytes) = loop {
        probe_attempts += 1;
        match probe_for_download(&network.client, network.cookies.as_ref(), spec, probe_start).await
        {
            // A range probe that receives a non-range response is retried a
            // bounded number of times: a transient server or proxy error can
            // look like a server that ignores ranges, and a genuine non-range
            // server still falls back to a plain download after the bound.
            Ok((metadata, bytes)) if metadata.range_supported || probe_attempts >= 3 => {
                break (metadata, bytes);
            }
            Ok(_) => {
                tokio::time::sleep(retry::delay_for_attempt(probe_attempts, None)).await;
            }
            Err(error) if error.is_retryable() && probe_attempts < PROBE_ATTEMPTS => {
                tokio::time::sleep(retry::delay_for_attempt(
                    probe_attempts,
                    error.retry_after(),
                ))
                .await;
            }
            Err(error) => {
                drop(part);
                return Err(mark_failed(store, task_id, error));
            }
        }
    };

    if let Some(final_url) = metadata.final_url.as_deref() {
        store.record_final_url(task_id, &storage_url(final_url))?;
    }

    let mut resuming = !persisted.is_empty();
    if resuming {
        if task.etag.as_deref() != metadata.etag.as_deref()
            || task.last_modified.as_deref() != metadata.last_modified.as_deref()
            || task.total_bytes != metadata.total_bytes
        {
            drop(part);
            return Err(mark_failed(
                store,
                task_id,
                EngineError::new(
                    "FILE_CHANGED",
                    "the remote file changed since the task was paused",
                ),
            ));
        }

        let plan_is_usable =
            metadata.range_supported && segments_cover(&persisted, metadata.total_bytes);
        if !plan_is_usable {
            persisted.clear();
        }
        resuming = !persisted.is_empty();
    }

    let is_fresh = !resuming;
    let mut range_supported = metadata.range_supported;
    let mut first_byte_write = None;
    if is_fresh {
        let mut plan = plan_segments(
            metadata.total_bytes,
            metadata.range_supported,
            spec.connections,
        )
        .map_err(|error| mark_failed(store, task_id, error))?;
        // A known empty total finalizes without workers regardless of ranges.
        if metadata.total_bytes == Some(0) {
            plan = SegmentPlan::Ranged(Vec::new());
        }
        store.record_discovered_metadata(
            task_id,
            metadata.total_bytes,
            metadata.etag.as_deref(),
            metadata.last_modified.as_deref(),
        )?;
        if let Some(total) = metadata.total_bytes {
            if let Err(error) = part.preallocate(total) {
                drop(part);
                if error.code() == "DISK_FULL" {
                    return pause_disk_full(store, task_id, error);
                }
                return Err(mark_failed(store, task_id, error));
            }
        }

        let mut rows = segment_inputs(&plan, metadata.total_bytes);
        if let (Some(bytes), SegmentPlan::Ranged(segments)) = (probe_bytes.as_ref(), &plan) {
            if let (Some(first), Some(row)) = (segments.first(), rows.first_mut()) {
                let available = first.end.saturating_sub(first.start);
                let take = (bytes.len() as u64).min(available);
                if take > 0 {
                    row.downloaded = take;
                    row.state = if take >= available {
                        SegmentState::Done
                    } else {
                        SegmentState::Pending
                    };
                    first_byte_write = Some((bytes[..take as usize].to_vec(), first.start));
                }
            }
        }

        // Revalidate the first range before any worker starts. A server that
        // reports range support to the probe but returns a full body here is
        // treated as a plain download while nothing has been committed.
        if range_supported {
            if let SegmentPlan::Ranged(segments) = &plan {
                if let Some(first) = segments.first() {
                    let downloaded = rows.first().map(|row| row.downloaded).unwrap_or(0);
                    let start = first.start.saturating_add(downloaded);
                    if start < first.end {
                        match check_first_range(network, spec, start, first.end).await? {
                            RangeCheck::Accepted => {}
                            RangeCheck::Ignored => {
                                range_supported = false;
                                plan = SegmentPlan::OpenEnded { start: 0 };
                                rows = segment_inputs(&plan, metadata.total_bytes);
                                first_byte_write = None;
                            }
                        }
                    }
                }
            }
        }

        store.replace_segments(task_id, &rows)?;
    }

    if let Some(flow) = requested_flow(control) {
        drop(part);
        return finish_interruption(store, task_id, dest_dir, flow);
    }

    if let Some((bytes, offset)) = first_byte_write {
        part.write_at(offset, &bytes)?;
    }

    let mut plain = !range_supported;
    let mut attempt = 0usize;
    loop {
        attempt += 1;
        match run_pool(network, store, task_id, &part, spec, control, plain).await {
            Ok(PoolOutcome::Completed) => break,
            Ok(PoolOutcome::Interrupted(flow)) => {
                drop(part);
                return finish_interruption(store, task_id, dest_dir, flow);
            }
            Ok(PoolOutcome::Fallback) => {
                if attempt > 1 {
                    return Err(mark_failed(
                        store,
                        task_id,
                        EngineError::new(
                            "RANGE_IGNORED",
                            "the server ignored the requested byte range",
                        ),
                    ));
                }
                // No task bytes were committed, so the multi-segment plan is
                // discarded and one plain connection restarts from zero.
                let rows = plain_segment_rows(metadata.total_bytes);
                store.replace_segments(task_id, &rows)?;
                if let Some(total) = metadata.total_bytes {
                    if let Err(error) = part.preallocate(total) {
                        drop(part);
                        if error.code() == "DISK_FULL" {
                            return pause_disk_full(store, task_id, error);
                        }
                        return Err(mark_failed(store, task_id, error));
                    }
                }
                plain = true;
            }
            Err(error) if error.code() == "DISK_FULL" => {
                // A full device is recoverable: keep the part and progress and
                // leave the task paused so a later resume can finish the file.
                return pause_disk_full(store, task_id, error);
            }
            Err(error) => {
                let flow = interruption_flow(store, task_id, &error)?;
                if let Some(flow) = flow {
                    drop(part);
                    return finish_interruption(store, task_id, dest_dir, flow);
                }
                drop(part);
                return Err(error);
            }
        }
    }

    if let Some(flow) = requested_flow(control) {
        drop(part);
        return finish_interruption(store, task_id, dest_dir, flow);
    }

    part.sync_all()
        .map_err(|error| mark_failed(store, task_id, error))?;
    if let Err(error) =
        store.transition_task(task_id, TaskState::Downloading, TaskState::Finalizing)
    {
        let flow = interruption_flow(store, task_id, &error)?;
        drop(part);
        if let Some(flow) = flow {
            return finish_interruption(store, task_id, dest_dir, flow);
        }
        return Err(error);
    }
    drop(part);

    let finalized = match finalize_task(store, task_id, dest_dir, spec.expected_sha256.as_deref()) {
        Ok(finalized) => finalized,
        Err(error) => {
            let flow = interruption_flow(store, task_id, &error)?;
            if let Some(flow) = flow {
                return finish_interruption(store, task_id, dest_dir, flow);
            }
            return Err(error);
        }
    };
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

fn segments_cover(segments: &[Segment], total: Option<u64>) -> bool {
    let mut ordered: Vec<&Segment> = segments.iter().collect();
    ordered.sort_by_key(|segment| segment.start);

    let mut expected = 0u64;
    for segment in ordered {
        if segment.start != expected || segment.end <= segment.start {
            return false;
        }
        if segment.downloaded_bytes > segment.end - segment.start {
            return false;
        }
        expected = segment.end;
    }

    match total {
        Some(total) => expected == total,
        None => segments.len() == 1 && expected >= OPEN_ENDED_END,
    }
}

fn plain_segment_rows(total: Option<u64>) -> Vec<SegmentInput> {
    match total {
        Some(0) => Vec::new(),
        Some(total) => vec![SegmentInput {
            index: 0,
            start: 0,
            end: total,
            downloaded: 0,
            state: SegmentState::Pending,
        }],
        None => vec![SegmentInput {
            index: 0,
            start: 0,
            end: OPEN_ENDED_END,
            downloaded: 0,
            state: SegmentState::Pending,
        }],
    }
}

/// Resolve a task's persisted part name, falling back to the legacy name.
pub(crate) fn task_part_name(task: &Task) -> String {
    task.part_path
        .as_deref()
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| PartFile::part_name(&task.filename))
}

/// Whether a task's own resolved part file exists.
pub(crate) fn part_exists_for_task(dest_dir: &Path, task: &Task) -> bool {
    part_exists_named(dest_dir, &task_part_name(task))
}

/// Whether a named part file exists under a destination.
pub(crate) fn part_exists_named(dest_dir: &Path, part_name: &str) -> bool {
    let Ok(cap) = CapDir::open(dest_dir) else {
        return false;
    };
    cap.exists(part_name)
}

pub(crate) fn part_missing() -> EngineError {
    EngineError::new(
        "PART_MISSING",
        "the task's part file is missing and cannot be resumed",
    )
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
            if load_task(store, task_id)?.state == TaskState::Cancelled {
                cancel_task(store, task_id, dest_dir)?;
                return Ok(TaskRunOutcome::Cancelled);
            }
            Ok(TaskRunOutcome::Paused)
        }
        WorkerFlow::Cancelled => {
            cancel_task(store, task_id, dest_dir)?;
            Ok(TaskRunOutcome::Cancelled)
        }
        // `Complete` is never passed here: callers only forward a requested or
        // interrupted flow. `RangeFallback` is resolved by the pool before an
        // interruption is reported, so both arms are defensive only.
        WorkerFlow::Complete | WorkerFlow::RangeFallback => Err(EngineError::new(
            "INTERNAL_ERROR",
            "an interruption flow was reported with a non-interruption outcome",
        )),
    }
}

fn pause_disk_full(
    store: &Database,
    task_id: &str,
    error: EngineError,
) -> Result<TaskRunOutcome, EngineError> {
    persist_pause(store, task_id)?;
    store.update_task_error(task_id, error.code(), &error.to_string())?;
    Ok(TaskRunOutcome::Paused)
}

pub(crate) fn persist_pause(store: &Database, task_id: &str) -> Result<(), EngineError> {
    // A control request from another engine connection may already have moved
    // the row to cancelled; pausing must not overwrite that decision.
    if load_task(store, task_id)?.state == TaskState::Cancelled {
        return Ok(());
    }
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
    let cap = CapDir::open(dest_dir).map_err(platform_error)?;
    let part_name = task_part_name(&task);
    match cap.remove_file(&part_name) {
        Ok(()) => {}
        Err(error) if error.code == "not_found" => {}
        Err(error) => return Err(platform_error(error)),
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

    let part_name = task_part_name(&task);
    let part_exists = exists(&cap, &part_name);
    let final_exists = exists(&cap, &task.filename);

    if !part_exists {
        if final_exists {
            let actual = hash_cap_file(&cap, &task.filename)?;
            match task.actual_sha256.as_deref() {
                Some(stored) => {
                    if !hash_matches(&actual, stored) {
                        return Err(mark_failed(
                            store,
                            task_id,
                            checksum_mismatch("the final file does not match the stored SHA-256"),
                        ));
                    }
                }
                None => {
                    if let Some(total) = task.total_bytes {
                        if file_len(&cap, &task.filename)? != total {
                            return Err(mark_failed(
                                store,
                                task_id,
                                checksum_mismatch(
                                    "the final file size does not match the recorded total",
                                ),
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
                }
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

    let part = PartFile::open_named(&cap, &part_name)?;
    part.sync_all()?;
    let actual = part.try_sha256()?;
    drop(part);

    if task
        .actual_sha256
        .as_deref()
        .is_some_and(|stored| !hash_matches(&actual, stored))
    {
        return Err(mark_failed(
            store,
            task_id,
            checksum_mismatch("the downloaded bytes do not match the stored SHA-256"),
        ));
    }
    store.record_actual_hash(task_id, &actual)?;
    if expected.is_some_and(|expected| !hash_matches(&actual, expected)) {
        return Err(mark_failed(
            store,
            task_id,
            checksum_mismatch("the downloaded bytes do not match the expected SHA-256"),
        ));
    }

    let chosen = unique_final_name(&cap, &task.filename);
    if let Err(error) = cap.replace_blocking(&part_name, &chosen) {
        // A cancel from another engine connection may remove the part after it
        // was hashed. The cancellation stays authoritative and no file is left.
        if load_task(store, task_id)?.state == TaskState::Cancelled {
            let _ = cap.remove_file(&part_name);
            return Err(EngineError::invalid_state(
                "the task was cancelled during finalization",
            ));
        }
        return Err(mark_failed(store, task_id, platform_error(error)));
    }
    cap.sync_dir().map_err(platform_error)?;
    if let Err(error) = store.complete_task(task_id, Some(&actual)) {
        if error.code() == "INVALID_STATE"
            && load_task(store, task_id)?.state == TaskState::Cancelled
        {
            // The rename won the race but the row was cancelled before it could
            // be marked completed, so undo the final file this call created.
            let _ = cap.remove_file(&chosen);
            let _ = cap.remove_file(&part_name);
        }
        return Err(error);
    }

    Ok(FinalizeOutcome {
        final_path: dest_dir.join(chosen),
    })
}

#[allow(clippy::too_many_arguments)]
async fn run_worker(
    network: &NetworkContext,
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
    let target = WriteTarget::Static {
        progress,
        progress_index,
        limit_end,
    };
    let result = stream_request(
        network,
        store,
        task_id,
        part,
        spec,
        control,
        request,
        segment_start,
        &target,
    )
    .await
    .map(|(_, flow, _)| flow);

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

type PoolFuture<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<(u32, WorkerFlow), EngineError>> + 'a>,
>;

/// Run one registry-backed worker pool until every segment is done.
async fn run_pool(
    network: &NetworkContext,
    store: &Database,
    task_id: &str,
    part: &PartFile,
    spec: &DownloadSpec,
    control: &TaskControl,
    plain: bool,
) -> Result<PoolOutcome, EngineError> {
    let segments = store.segments_for_task(task_id)?;
    for segment in &segments {
        if segment.state == SegmentState::Done || segment.state == SegmentState::Pending {
            continue;
        }
        let _ =
            store.transition_segment(task_id, segment.index, segment.state, SegmentState::Pending);
    }
    let segments = store.segments_for_task(task_id)?;
    let registry = Mutex::new(SegmentRegistry::from_segments(&segments));
    let concurrency = (spec.connections as usize).max(1);
    let mut active: FuturesUnordered<PoolFuture<'_>> = FuturesUnordered::new();

    loop {
        if active.len() >= concurrency {
            break;
        }
        let pending = lock_registry(&registry)?.take_pending();
        let Some(segment_index) = pending else {
            break;
        };
        active.push(Box::pin(run_dynamic_worker(
            network,
            store,
            task_id,
            part,
            spec,
            control,
            &registry,
            segment_index,
            plain,
        )));
    }

    let mut interrupted = None;
    while let Some(result) = active.next().await {
        match result {
            Ok((_, WorkerFlow::Complete)) => {}
            Ok((_, WorkerFlow::RangeFallback)) => {
                drop(active);
                return Ok(PoolOutcome::Fallback);
            }
            Ok((_, flow)) => {
                interrupted = Some(flow);
                break;
            }
            Err(error) => {
                if control.cancel_requested() {
                    interrupted = Some(WorkerFlow::Cancelled);
                    break;
                }
                if control.pause_requested() {
                    interrupted = Some(WorkerFlow::Paused);
                    break;
                }
                drop(active);
                return Err(error);
            }
        }

        if let Some(flow) = requested_flow(control) {
            interrupted = Some(flow);
            break;
        }

        // An idle worker first takes a pending tail, then cooperatively splits
        // the active segment with the most remaining bytes.
        loop {
            if active.len() >= concurrency {
                break;
            }
            let pending = lock_registry(&registry)?.take_pending();
            if let Some(segment_index) = pending {
                active.push(Box::pin(run_dynamic_worker(
                    network,
                    store,
                    task_id,
                    part,
                    spec,
                    control,
                    &registry,
                    segment_index,
                    plain,
                )));
                continue;
            }
            if plain {
                break;
            }
            let Some(tail_index) = split_and_persist(store, task_id, &registry)? else {
                break;
            };
            active.push(Box::pin(run_dynamic_worker(
                network, store, task_id, part, spec, control, &registry, tail_index, plain,
            )));
        }
    }

    drop(active);
    match interrupted {
        Some(flow) => Ok(PoolOutcome::Interrupted(flow)),
        None => Ok(PoolOutcome::Completed),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_dynamic_worker(
    network: &NetworkContext,
    store: &Database,
    task_id: &str,
    part: &PartFile,
    spec: &DownloadSpec,
    control: &TaskControl,
    registry: &Mutex<SegmentRegistry>,
    segment_index: u32,
    plain: bool,
) -> Result<(u32, WorkerFlow), EngineError> {
    if let Some(flow) = requested_flow(control) {
        return Ok((segment_index, flow));
    }
    set_dynamic_segment_state(
        store,
        registry,
        task_id,
        segment_index,
        SegmentState::Pending,
        SegmentState::Active,
    )?;

    let (segment_start, position, end) = {
        let registry = lock_registry(registry)?;
        let segment = registry
            .segments
            .iter()
            .find(|segment| segment.index == segment_index)
            .ok_or_else(|| {
                EngineError::new(
                    "INTERNAL_ERROR",
                    "the requested segment is missing from the registry",
                )
            })?;
        (segment.start, segment.pos, segment.end)
    };

    let request = if plain {
        RequestShape::Plain
    } else if end < OPEN_ENDED_END && position >= end {
        set_dynamic_segment_state(
            store,
            registry,
            task_id,
            segment_index,
            SegmentState::Active,
            SegmentState::Done,
        )?;
        return Ok((segment_index, WorkerFlow::Complete));
    } else if end >= OPEN_ENDED_END {
        RequestShape::Range {
            start: position,
            end: None,
        }
    } else {
        RequestShape::Range {
            start: position,
            end: Some(end),
        }
    };

    let target = WriteTarget::Dynamic {
        registry,
        segment_index,
    };
    let result = stream_request(
        network,
        store,
        task_id,
        part,
        spec,
        control,
        request,
        segment_start,
        &target,
    )
    .await;

    match result {
        Ok((_, WorkerFlow::Complete, _)) => {
            set_dynamic_segment_state(
                store,
                registry,
                task_id,
                segment_index,
                SegmentState::Active,
                SegmentState::Done,
            )?;
            Ok((segment_index, WorkerFlow::Complete))
        }
        Ok((_, WorkerFlow::RangeFallback, _)) => Ok((segment_index, WorkerFlow::RangeFallback)),
        Ok((_, flow, _)) => {
            let _ = set_dynamic_segment_state(
                store,
                registry,
                task_id,
                segment_index,
                SegmentState::Active,
                SegmentState::Pending,
            );
            Ok((segment_index, flow))
        }
        Err(error) => {
            let _ = set_dynamic_segment_state(
                store,
                registry,
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
async fn stream_request(
    network: &NetworkContext,
    store: &Database,
    task_id: &str,
    part: &PartFile,
    spec: &DownloadSpec,
    control: &TaskControl,
    request: RequestShape,
    segment_start: u64,
    target: &WriteTarget<'_>,
) -> Result<(u64, WorkerFlow, Option<u64>), EngineError> {
    let mut position = match request {
        RequestShape::Range { start, .. } => start,
        RequestShape::Plain => 0,
    };
    let mut range_total = None;
    let mut last_persisted = 0u64;
    let mut retries = 0usize;

    loop {
        if let Some(flow) = requested_flow(control) {
            if let Some(flow) =
                persist_target_progress_or_flow(store, task_id, target, position, segment_start)?
            {
                return Ok((position, flow, range_total));
            }
            return Ok((position, flow, range_total));
        }
        if matches!(request, RequestShape::Plain) {
            // A plain response always starts at byte zero; restarting
            // overwrites earlier bytes instead of appending them.
            position = 0;
            if let WriteTarget::Dynamic {
                registry,
                segment_index,
            } = target
            {
                reset_registry_position(registry, *segment_index)?;
            }
        }

        let current = match request {
            RequestShape::Plain => RequestShape::Plain,
            RequestShape::Range { end, .. } => match target {
                WriteTarget::Dynamic {
                    registry,
                    segment_index,
                } => RequestShape::Range {
                    start: position,
                    end: registry_limit(registry, *segment_index)?,
                },
                WriteTarget::Static { .. } => RequestShape::Range {
                    start: position,
                    end,
                },
            },
        };

        if let RequestShape::Range {
            start,
            end: Some(end),
        } = current
        {
            if start >= end {
                if let Some(flow) = persist_target_progress_or_flow(
                    store,
                    task_id,
                    target,
                    position,
                    segment_start,
                )? {
                    return Ok((position, flow, range_total));
                }
                return Ok((position, WorkerFlow::Complete, range_total));
            }
        }

        let mut send_attempts = 0usize;
        let response = loop {
            if let Some(flow) = requested_flow(control) {
                if let Some(flow) = persist_target_progress_or_flow(
                    store,
                    task_id,
                    target,
                    position,
                    segment_start,
                )? {
                    return Ok((position, flow, range_total));
                }
                return Ok((position, flow, range_total));
            }
            send_attempts += 1;
            let headers = request_headers(spec, current.range_header())?;
            let response = match get_following_redirects_with_cookies(
                &network.client,
                &spec.url,
                headers,
                network.cookies.as_ref(),
            )
            .await
            {
                Ok((response, _final_url)) => response,
                Err(error) if error.is_retryable() && send_attempts < retry::MAX_ATTEMPTS => {
                    tokio::time::sleep(retry::delay_for_attempt(
                        send_attempts,
                        error.retry_after(),
                    ))
                    .await;
                    continue;
                }
                Err(error) => return Err(error),
            };
            let status = response.status();
            if current.expects_partial() {
                if status == StatusCode::OK {
                    // A full response to a ranged request is never written at
                    // its nonzero offset. With no committed bytes the pool can
                    // restart as one plain connection from zero.
                    if let WriteTarget::Dynamic { registry, .. } = target {
                        let mut registry = lock_registry(registry)?;
                        if registry.committed_bytes() == 0 {
                            registry.fallback = true;
                            return Ok((position, WorkerFlow::RangeFallback, range_total));
                        }
                    }
                    return Err(EngineError::new(
                        "RANGE_IGNORED",
                        "the server ignored the requested byte range",
                    ));
                }
                if status != StatusCode::PARTIAL_CONTENT {
                    let error = response_status_error(&response);
                    if error.is_retryable() && send_attempts < retry::MAX_ATTEMPTS {
                        tokio::time::sleep(retry::delay_for_attempt(
                            send_attempts,
                            error.retry_after(),
                        ))
                        .await;
                        continue;
                    }
                    return Err(error);
                }
                let (start, total) = parse_content_range(&response)?;
                if start != position {
                    return Err(EngineError::new(
                        "RANGE_MISMATCH",
                        "the server returned a different range than requested",
                    ));
                }
                range_total = total;
            } else if status != StatusCode::OK {
                let error = response_status_error(&response);
                if error.is_retryable() && send_attempts < retry::MAX_ATTEMPTS {
                    tokio::time::sleep(retry::delay_for_attempt(
                        send_attempts,
                        error.retry_after(),
                    ))
                    .await;
                    continue;
                }
                return Err(error);
            } else {
                range_total = response.content_length();
            }
            break response;
        };

        let mut body = response.bytes_stream();
        let mut interrupted = false;
        loop {
            if let Some(flow) = requested_flow(control) {
                if let Some(flow) = persist_target_progress_or_flow(
                    store,
                    task_id,
                    target,
                    position,
                    segment_start,
                )? {
                    return Ok((position, flow, range_total));
                }
                return Ok((position, flow, range_total));
            }
            let Some(chunk) = body.next().await else {
                break;
            };
            let chunk: bytes::Bytes = match chunk {
                Ok(chunk) => chunk,
                Err(_) => {
                    interrupted = true;
                    break;
                }
            };
            match target {
                WriteTarget::Static { limit_end, .. } => {
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
                        network.pace(bytes.len()).await;
                        if let Err(error) = part.write_at(position, bytes) {
                            if let Some(flow) = persist_target_progress_or_flow(
                                store,
                                task_id,
                                target,
                                position,
                                segment_start,
                            )? {
                                return Ok((position, flow, range_total));
                            }
                            return Err(error);
                        }
                        position += bytes.len() as u64;
                    }
                    let downloaded = position.saturating_sub(segment_start);
                    if downloaded >= last_persisted + PROGRESS_INTERVAL {
                        if let Some(flow) = persist_target_progress_or_flow(
                            store,
                            task_id,
                            target,
                            position,
                            segment_start,
                        )? {
                            return Ok((position, flow, range_total));
                        }
                        last_persisted = downloaded;
                    }
                    if limit_end.is_some_and(|end| position >= end) {
                        break;
                    }
                }
                WriteTarget::Dynamic {
                    registry,
                    segment_index,
                } => {
                    network.pace(chunk.len()).await;
                    let write =
                        match write_dynamic_chunk(registry, part, *segment_index, position, &chunk)
                        {
                            Ok(write) => write,
                            Err(error) => {
                                if let Some(flow) = persist_target_progress_or_flow(
                                    store,
                                    task_id,
                                    target,
                                    position,
                                    segment_start,
                                )? {
                                    return Ok((position, flow, range_total));
                                }
                                return Err(error);
                            }
                        };
                    match write {
                        ChunkWrite::Written(take) => {
                            let truncated = take < chunk.len();
                            position += take as u64;
                            let downloaded = position.saturating_sub(segment_start);
                            if downloaded >= last_persisted + PROGRESS_INTERVAL {
                                if let Some(flow) = persist_target_progress_or_flow(
                                    store,
                                    task_id,
                                    target,
                                    position,
                                    segment_start,
                                )? {
                                    return Ok((position, flow, range_total));
                                }
                                last_persisted = downloaded;
                            }
                            if truncated {
                                break;
                            }
                        }
                        ChunkWrite::Closed => break,
                        ChunkWrite::Fallback => {
                            return Ok((position, WorkerFlow::RangeFallback, range_total));
                        }
                    }
                }
            }
        }

        if let Some(flow) =
            persist_target_progress_or_flow(store, task_id, target, position, segment_start)?
        {
            return Ok((position, flow, range_total));
        }

        if !interrupted {
            let limit = match target {
                WriteTarget::Static { limit_end, .. } => *limit_end,
                WriteTarget::Dynamic {
                    registry,
                    segment_index,
                } => registry_limit(registry, *segment_index)?,
            };
            let reached_total = range_total.is_some_and(|total| position >= total);
            let complete = match limit {
                Some(end) => position >= end || reached_total,
                None => true,
            };
            if complete {
                return Ok((position, WorkerFlow::Complete, range_total));
            }
        }

        retries += 1;
        if retries >= RANGE_REQUEST_ATTEMPTS {
            return Err(EngineError::retryable(
                if interrupted {
                    "NETWORK_INTERRUPTED"
                } else {
                    "INCOMPLETE_BODY"
                },
                "the response ended before the range was complete",
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RangeCheck {
    Accepted,
    Ignored,
}

/// Revalidate the first planned range with a short non-empty prefix.
///
/// A matching 206 keeps the multi-segment plan. A 200 means the server ignores
/// ranges, so the caller discards the plan before any worker writes.
async fn check_first_range(
    network: &NetworkContext,
    spec: &DownloadSpec,
    start: u64,
    end: u64,
) -> Result<RangeCheck, EngineError> {
    let length = end.saturating_sub(start);
    if length == 0 {
        return Ok(RangeCheck::Accepted);
    }
    let prefix = length.min(REVALIDATION_PREFIX_BYTES);
    let request_end = start.saturating_add(prefix);
    let mut attempts = 0usize;
    loop {
        attempts += 1;
        let headers = request_headers(
            spec,
            Some(format!("bytes={start}-{}", request_end.saturating_sub(1))),
        )?;
        let result = match get_following_redirects_with_cookies(
            &network.client,
            &spec.url,
            headers,
            network.cookies.as_ref(),
        )
        .await
        {
            Ok((response, _final_url)) => match response.status() {
                StatusCode::PARTIAL_CONTENT => {
                    let (range_start, _) = parse_content_range(&response)?;
                    if range_start != start {
                        Err(EngineError::new(
                            "RANGE_MISMATCH",
                            "the server returned a different range than requested",
                        ))
                    } else {
                        Ok(RangeCheck::Accepted)
                    }
                }
                StatusCode::OK => Ok(RangeCheck::Ignored),
                _ => Err(response_status_error(&response)),
            },
            Err(error) => Err(error),
        };

        match result {
            Ok(check) => return Ok(check),
            Err(error) if error.is_retryable() && attempts < PROBE_ATTEMPTS => {
                tokio::time::sleep(retry::delay_for_attempt(attempts, error.retry_after())).await;
            }
            Err(error) => return Err(error),
        }
    }
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

/// Probe a task's remote file with a one-byte request at `start`.
///
/// The response body is consumed only when it is a single ranged byte, so a
/// fresh download can persist that byte and resume its workers at the next
/// offset without re-requesting byte zero. A 200 response is classified as a
/// non-range download and its body is not consumed.
async fn probe_for_download(
    client: &Client,
    cookies: Option<&CookieJar>,
    spec: &DownloadSpec,
    start: u64,
) -> Result<(DownloadMetadata, Option<Vec<u8>>), EngineError> {
    let headers = request_headers(spec, Some(format!("bytes={start}-{start}")))?;
    let (response, final_url) =
        get_following_redirects_with_cookies(client, &spec.url, headers, cookies).await?;
    let final_url = final_url.as_str().to_string();

    let content_type = header_text(&response, CONTENT_TYPE);
    if !spec.allow_html && content_type.as_deref().is_some_and(is_html) {
        return Err(EngineError::new(
            "HTML_RESPONSE",
            "the server returned an HTML page instead of a downloadable file",
        ));
    }

    let etag = header_text(&response, ETAG);
    let last_modified = header_text(&response, LAST_MODIFIED);
    match response.status() {
        StatusCode::PARTIAL_CONTENT => {
            let (range_start, total) = parse_content_range(&response)?;
            if range_start != start {
                return Err(EngineError::new(
                    "RANGE_MISMATCH",
                    "the server returned a different range than requested",
                ));
            }
            // The probe asks for exactly one byte. Read at most one byte from
            // the stream so an overlong or mislabelled body cannot contribute
            // extra bytes to the persisted prefix.
            let mut body = response.bytes_stream();
            let first_byte = match body.next().await {
                Some(Ok(chunk)) => Some(chunk[..chunk.len().min(1)].to_vec()),
                Some(Err(_)) => {
                    return Err(EngineError::retryable(
                        "NETWORK_INTERRUPTED",
                        "the probe response body was interrupted",
                    ))
                }
                None => None,
            };
            Ok((
                DownloadMetadata {
                    total_bytes: total,
                    range_supported: true,
                    etag,
                    last_modified,
                    final_url: Some(final_url),
                },
                first_byte,
            ))
        }
        StatusCode::OK => Ok((
            DownloadMetadata {
                total_bytes: response.content_length(),
                range_supported: false,
                etag,
                last_modified,
                final_url: Some(final_url),
            },
            None,
        )),
        StatusCode::RANGE_NOT_SATISFIABLE => Ok((
            DownloadMetadata {
                total_bytes: Some(parse_unsatisfied_total(&response)?),
                range_supported: false,
                etag,
                last_modified,
                final_url: Some(final_url),
            },
            None,
        )),
        _ => Err(response_status_error(&response)),
    }
}

fn parse_unsatisfied_total(response: &reqwest::Response) -> Result<u64, EngineError> {
    let value = response
        .headers()
        .get(CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(invalid_response)?;
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

async fn probe_zero_length(
    client: &Client,
    cookies: Option<&CookieJar>,
    url: &str,
    options: &ProbeOptions,
) -> Result<DownloadMetadata, EngineError> {
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

    let (response, final_url) =
        get_following_redirects_with_cookies(client, url, headers, cookies).await?;
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
        final_url: Some(final_url.as_str().to_string()),
    })
}

fn header_text(response: &reqwest::Response, name: reqwest::header::HeaderName) -> Option<String> {
    response
        .headers()
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

fn status_error(status: StatusCode) -> EngineError {
    match status.as_u16() {
        401 | 403 => EngineError::new("AUTH_REQUIRED", "the server requires authorization"),
        404 => EngineError::new("NOT_FOUND", "the remote file was not found"),
        407 => EngineError::new("PROXY_AUTH_REQUIRED", "the proxy requires authentication"),
        408 => EngineError::retryable("TIMEOUT", "the server reported a request timeout"),
        429 => EngineError::retryable("RATE_LIMITED", "the server asked the client to slow down"),
        _ if status.is_server_error() => {
            EngineError::retryable("SERVER_ERROR", "the server failed to serve the file")
        }
        _ => invalid_response(),
    }
}

fn response_status_error(response: &reqwest::Response) -> EngineError {
    let mut error = status_error(response.status());
    if let Some(delay) = retry::retry_after(response.headers()) {
        error = error.with_retry_after(delay);
    }
    error
}

fn checksum_mismatch(message: &str) -> EngineError {
    EngineError::new("CHECKSUM_MISMATCH", message)
}

fn hash_matches(actual: &str, expected: &str) -> bool {
    actual.eq_ignore_ascii_case(expected.trim())
}

pub(crate) fn mark_failed(store: &Database, task_id: &str, error: EngineError) -> EngineError {
    if let Ok(Some(task)) = store.task(task_id) {
        if task.state.can_transition_to(TaskState::Failed) {
            let _ = store.transition_task(task_id, task.state, TaskState::Failed);
        }
        let _ = store.update_task_error(task_id, error.code(), &error.to_string());
    }
    error
}

pub(crate) fn load_task(store: &Database, task_id: &str) -> Result<Task, EngineError> {
    store
        .task(task_id)?
        .ok_or_else(|| EngineError::new("TASK_NOT_FOUND", "the requested task does not exist"))
}

pub(crate) fn find_completed_final(cap: &CapDir, task: &Task) -> Result<String, EngineError> {
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
    cap.exists(name)
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
