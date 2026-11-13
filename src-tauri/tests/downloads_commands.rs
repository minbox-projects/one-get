//! Behavior tests for the long-lived engine service and the download command
//! surface.
//!
//! Intended public interface under test:
//! - `oneget_lib::engine::service::{EngineService, AddDownload, ProbeRequest}`
//! - `oneget_lib::commands::downloads::{downloads_list, downloads_segments,
//!   downloads_probe, downloads_add, downloads_open_file, ...}`
//! - `oneget_lib::app_state::AppState::with_engine`
//!
//! Expected RED at authoring time: the service and download command modules do
//! not exist yet, so this target fails to compile. GREEN is left to the
//! implementation step.

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use oneget_lib::app_state::AppState;
use oneget_lib::commands::app::{
    settings_save, AutostartService, NotificationSendResult, NotificationService,
    NotificationStatus, QuitService, SecretRoundTripResult, SecretService,
};
use oneget_lib::commands::downloads::{
    downloads_add, downloads_list, downloads_open_file, downloads_probe, downloads_segments,
    AddDownloadRequest, ProbeDownloadRequest,
};
use oneget_lib::config::SettingsStore;
use oneget_lib::engine::error::EngineError;
use oneget_lib::engine::part_file::PartFile;
use oneget_lib::engine::scheduler::EngineOptions;
use oneget_lib::engine::service::{AddDownload, EngineService, ProbeRequest};
use oneget_lib::engine::state::{SegmentState, TaskState};
use oneget_lib::error::AppError;
use oneget_lib::logging::{LogConfig, Logger};
use oneget_lib::platform::files::CapDir;
use oneget_lib::platform::paths::AppPaths;
use oneget_lib::store::{Database, Task};
use serde_json::{json, Value};
use support::fixtures::pattern_bytes;
use support::range_server::{FixtureConfig, RangeMode, RangeServer};
use tempfile::tempdir;

const POLL: Duration = Duration::from_millis(20);
const WAIT: Duration = Duration::from_secs(30);

// --- Test fakes for the AppState service dependencies ---

#[derive(Clone, Copy)]
struct NoopSecrets;

impl SecretService for NoopSecrets {
    fn roundtrip(&self, _secret: &str) -> Result<SecretRoundTripResult, AppError> {
        Ok(SecretRoundTripResult { success: true })
    }
}

#[derive(Clone, Copy)]
struct NoopNotifications;

impl NotificationService for NoopNotifications {
    fn status(&self) -> Result<NotificationStatus, AppError> {
        Ok(NotificationStatus {
            permission: "granted".to_string(),
        })
    }

    fn send_test(&self) -> Result<NotificationSendResult, AppError> {
        Ok(NotificationSendResult { sent: true })
    }
}

#[derive(Clone, Copy)]
struct NoopAutostart;

impl AutostartService for NoopAutostart {
    fn is_enabled(&self) -> Result<bool, AppError> {
        Ok(false)
    }

    fn set_enabled(&self, enabled: bool) -> Result<bool, AppError> {
        Ok(enabled)
    }
}

#[derive(Clone, Copy)]
struct NoopQuit;

impl QuitService for NoopQuit {
    fn request_quit(&self) -> Result<(), AppError> {
        Ok(())
    }
}

struct StateHarness {
    _temp: tempfile::TempDir,
    state: AppState,
    settings_path: PathBuf,
}

fn build_state(engine: Option<Arc<EngineService>>) -> StateHarness {
    let temp = tempdir().expect("tempdir");
    let paths = AppPaths::with_root(temp.path().join("app")).expect("app paths");
    let settings_path = paths.data.join("settings.json");
    let settings = SettingsStore::load(&settings_path).expect("settings store");
    let logger = Logger::new(LogConfig::with_defaults(paths.logs.clone())).expect("logger");
    let mut state = AppState::new(
        paths,
        settings,
        logger,
        Arc::new(NoopSecrets),
        Arc::new(NoopNotifications),
        Arc::new(NoopAutostart),
        Arc::new(NoopQuit),
    );
    if let Some(engine) = engine {
        state = state.with_engine(engine);
    }
    StateHarness {
        _temp: temp,
        state,
        settings_path,
    }
}

// --- Helpers ---

fn fixture(body: Vec<u8>) -> RangeServer {
    RangeServer::start(FixtureConfig {
        body,
        content_type: Some("application/octet-stream".to_string()),
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    })
}

fn add_download(url: String, filename: &str, connections: u32) -> AddDownload {
    AddDownload {
        url,
        filename: filename.to_string(),
        destination: None,
        connections,
        rate_limit: None,
        expected_sha256: None,
        allow_html: false,
        extra_headers: Vec::new(),
    }
}

fn add_request(url: String, filename: &str, connections: u32) -> AddDownloadRequest {
    AddDownloadRequest {
        url,
        filename: filename.to_string(),
        destination: None,
        connections,
        rate_limit: None,
        expected_sha256: None,
        allow_html: false,
        extra_headers: Vec::new(),
    }
}

fn find_task(service: &EngineService, id: &str) -> Option<Task> {
    service
        .list()
        .expect("list tasks")
        .into_iter()
        .find(|task| task.id == id)
}

fn assert_engine_code(error: &EngineError, code: &str) {
    assert_eq!(
        error.code(),
        code,
        "expected engine code {code}, got {}",
        error.code()
    );
}

async fn wait_for_state(service: &EngineService, id: &str, expected: TaskState) -> Task {
    let start = Instant::now();
    loop {
        if let Some(task) = find_task(service, id) {
            if task.state == expected {
                return task;
            }
            if start.elapsed() > WAIT {
                panic!(
                    "timed out waiting for {id} to reach {expected:?}; current {:?}, downloaded={}, error={:?}",
                    task.state, task.downloaded_bytes, task.error_code
                );
            }
        }
        tokio::time::sleep(POLL).await;
    }
}

async fn wait_for_progress(service: &EngineService, id: &str) -> Task {
    let start = Instant::now();
    loop {
        if let Some(task) = find_task(service, id) {
            if task.downloaded_bytes > 0 {
                return task;
            }
        }
        if start.elapsed() > WAIT {
            panic!("timed out waiting for {id} to make progress");
        }
        tokio::time::sleep(POLL).await;
    }
}

fn summary_values(state: &AppState) -> Vec<Value> {
    downloads_list(state)
        .expect("list download summaries")
        .iter()
        .map(|summary| serde_json::to_value(summary).expect("summary json"))
        .collect()
}

fn summary_state(summary: &Value) -> &str {
    summary
        .get("state")
        .and_then(Value::as_str)
        .expect("summary exposes a string state")
}

// --- 1. Startup recovery, listing, added download, segment query ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_recovers_interrupted_task_and_completes_added_download() {
    let body = pattern_bytes(11, 256 * 1024);
    let server = fixture(body.clone());
    let store_dir = tempdir().expect("store tempdir");
    let dest = tempdir().expect("dest tempdir");
    let store = Database::open(store_dir.path().join("tasks.db")).expect("open store");

    let interrupted_id = store
        .create_task(&server.url("/interrupted.bin"), "interrupted.bin")
        .expect("create interrupted task");
    store
        .record_connections(&interrupted_id, 1)
        .expect("record connections");
    {
        let cap = CapDir::open(dest.path()).expect("open destination");
        PartFile::create(&cap, "interrupted.bin", Some(body.len() as u64))
            .expect("create part")
            .sync_all()
            .expect("sync part");
    }
    store
        .transition_task(&interrupted_id, TaskState::Queued, TaskState::Connecting)
        .expect("mark connecting");
    store
        .transition_task(
            &interrupted_id,
            TaskState::Connecting,
            TaskState::Downloading,
        )
        .expect("mark downloading");

    let service = EngineService::start(
        store,
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 2 },
    )
    .expect("start service");

    assert!(
        find_task(&service, &interrupted_id).is_some(),
        "startup recovery must keep the interrupted task listed"
    );

    wait_for_state(&service, &interrupted_id, TaskState::Completed).await;
    assert_eq!(
        std::fs::read(dest.path().join("interrupted.bin")).expect("read recovered final file"),
        body,
        "recovered task must produce the fixture bytes"
    );

    let added_id = service
        .add(add_download(server.url("/added.bin"), "added.bin", 2))
        .expect("add download");
    wait_for_state(&service, &added_id, TaskState::Completed).await;

    let tasks = service.list().expect("list tasks");
    for id in [&interrupted_id, &added_id] {
        let task = tasks
            .iter()
            .find(|task| &task.id == id)
            .expect("completed task must be listed");
        assert_eq!(task.state, TaskState::Completed);
    }

    let segments = service.segments(&added_id).expect("query segments");
    assert!(!segments.is_empty(), "completed task must expose segments");
    assert_eq!(segments[0].start, 0, "segments start at zero");
    assert_eq!(
        segments[segments.len() - 1].end,
        body.len() as u64,
        "segments cover the full file"
    );
    for pair in segments.windows(2) {
        assert_eq!(pair[0].end, pair[1].start, "segments must be gap-free");
    }
    assert!(
        segments
            .iter()
            .all(|segment| segment.state == SegmentState::Done),
        "completed task segments must all be done"
    );

    service.shutdown();
}

// --- 2. Controls through the service while the run loop is active ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_controls_pause_resume_cancel_and_retry_while_running() {
    let body = pattern_bytes(17, 2 * 1024 * 1024);
    let server = RangeServer::start(FixtureConfig {
        body: body.clone(),
        stream_delay: Some(Duration::from_millis(25)),
        stream_chunk_size: 16 * 1024,
        ..Default::default()
    });
    let store_dir = tempdir().expect("store tempdir");
    let dest = tempdir().expect("dest tempdir");
    let store = Database::open(store_dir.path().join("tasks.db")).expect("open store");
    let service = EngineService::start(
        store,
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    )
    .expect("start service");

    let slow_id = service
        .add(add_download(server.url("/slow.bin"), "slow.bin", 2))
        .expect("add slow download");
    let queued_id = service
        .add(add_download(server.url("/queued.bin"), "queued.bin", 1))
        .expect("add queued download");

    wait_for_state(&service, &slow_id, TaskState::Downloading).await;
    assert_eq!(
        find_task(&service, &queued_id).expect("queued task").state,
        TaskState::Queued,
        "a second task must wait for the single concurrency slot"
    );

    service.cancel(&queued_id).expect("cancel queued task");
    assert_eq!(
        find_task(&service, &queued_id)
            .expect("cancelled task")
            .state,
        TaskState::Cancelled
    );

    wait_for_progress(&service, &slow_id).await;
    service.pause(&slow_id).expect("pause downloading task");
    assert_eq!(
        find_task(&service, &slow_id).expect("paused task").state,
        TaskState::Paused
    );
    let part_name = PartFile::part_name("slow.bin");
    assert!(
        dest.path().join(&part_name).exists(),
        "pause must preserve the part file"
    );

    service.resume(&slow_id).expect("resume paused task");
    wait_for_state(&service, &slow_id, TaskState::Completed).await;

    server.set_runtime_status_override(Some(404));
    let retry_id = service
        .add(add_download(server.url("/retry.bin"), "retry.bin", 1))
        .expect("add failing download");
    wait_for_state(&service, &retry_id, TaskState::Failed).await;
    let retry_part = PartFile::part_name("retry.bin");
    assert!(
        dest.path().join(&retry_part).exists(),
        "a failed task must preserve its part file"
    );

    server.clear_runtime_status_override();
    service.retry(&retry_id).expect("retry failed task");
    wait_for_state(&service, &retry_id, TaskState::Completed).await;
    assert_eq!(
        std::fs::read(dest.path().join("retry.bin")).expect("read retried final file"),
        body,
        "retry must finish with the fixture bytes"
    );

    service.shutdown();
}

// --- 3. Probe metadata and HTML classification ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_probe_reports_metadata_and_classifies_html() {
    let body = pattern_bytes(23, 64 * 1024);
    let server = RangeServer::start(FixtureConfig {
        body: body.clone(),
        content_type: Some("application/octet-stream".to_string()),
        etag: Some("\"probe-etag\"".to_string()),
        last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".to_string()),
        content_disposition: Some("attachment; filename=\"probe-target.bin\"".to_string()),
        ..Default::default()
    });
    let html_server = RangeServer::start(FixtureConfig {
        body: b"<html><body>login</body></html>".to_vec(),
        content_type: Some("text/html; charset=utf-8".to_string()),
        ..Default::default()
    });

    let store_dir = tempdir().expect("store tempdir");
    let dest = tempdir().expect("dest tempdir");
    let store = Database::open(store_dir.path().join("tasks.db")).expect("open store");
    let service = EngineService::start(
        store,
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    )
    .expect("start service");

    let outcome = service
        .probe(ProbeRequest {
            url: server.url("/files/original-name"),
            allow_html: false,
            extra_headers: Vec::new(),
        })
        .expect("probe file fixture");
    assert_eq!(outcome.total_bytes, Some(body.len() as u64));
    assert!(
        outcome.range_supported,
        "range fixture must report range support"
    );
    assert_eq!(outcome.filename, "probe-target.bin");
    assert_eq!(
        outcome.content_type.as_deref(),
        Some("application/octet-stream")
    );
    assert_eq!(outcome.etag.as_deref(), Some("\"probe-etag\""));
    assert_eq!(
        outcome.last_modified.as_deref(),
        Some("Wed, 21 Oct 2015 07:28:00 GMT")
    );
    assert!(
        outcome.final_url.ends_with("/files/original-name"),
        "probe must report the final URL"
    );

    let error = service
        .probe(ProbeRequest {
            url: html_server.url("/login"),
            allow_html: false,
            extra_headers: Vec::new(),
        })
        .expect_err("an HTML page must be classified without an explicit allowance");
    assert_engine_code(&error, "HTML_RESPONSE");

    let allowed = service
        .probe(ProbeRequest {
            url: html_server.url("/login"),
            allow_html: true,
            extra_headers: Vec::new(),
        })
        .expect("an explicit HTML allowance must classify the page");
    assert_eq!(
        allowed.content_type.as_deref(),
        Some("text/html; charset=utf-8")
    );

    service.shutdown();
}

// --- 4. Delete record vs delete file vs active deletion ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_delete_record_keeps_final_file_delete_file_removes_both() {
    let body = pattern_bytes(29, 64 * 1024);
    let server = fixture(body.clone());
    let store_dir = tempdir().expect("store tempdir");
    let dest = tempdir().expect("dest tempdir");
    let store = Database::open(store_dir.path().join("tasks.db")).expect("open store");
    let service = EngineService::start(
        store,
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    )
    .expect("start service");

    let kept_id = service
        .add(add_download(server.url("/keep.bin"), "keep.bin", 1))
        .expect("add kept download");
    wait_for_state(&service, &kept_id, TaskState::Completed).await;
    let kept_path = dest.path().join("keep.bin");
    assert!(kept_path.exists(), "completed final file must exist");

    service.delete_record(&kept_id).expect("delete record");
    assert!(
        find_task(&service, &kept_id).is_none(),
        "delete record must remove the task row"
    );
    assert!(
        kept_path.exists(),
        "delete record must keep the completed final file"
    );

    let removed_id = service
        .add(add_download(server.url("/remove.bin"), "remove.bin", 1))
        .expect("add removable download");
    wait_for_state(&service, &removed_id, TaskState::Completed).await;
    let removed_path = dest.path().join("remove.bin");
    assert!(removed_path.exists(), "completed final file must exist");

    service.delete_file(&removed_id).expect("delete file");
    assert!(
        find_task(&service, &removed_id).is_none(),
        "delete file must remove the task row"
    );
    assert!(
        !removed_path.exists(),
        "delete file must remove the final file"
    );

    let slow = RangeServer::start(FixtureConfig {
        body: pattern_bytes(31, 1024 * 1024),
        stream_delay: Some(Duration::from_millis(40)),
        stream_chunk_size: 16 * 1024,
        ..Default::default()
    });
    let active_id = service
        .add(add_download(slow.url("/active.bin"), "active.bin", 1))
        .expect("add active download");
    wait_for_state(&service, &active_id, TaskState::Downloading).await;

    let record_error = service
        .delete_record(&active_id)
        .expect_err("an active task record cannot be deleted");
    assert_engine_code(&record_error, "INVALID_STATE");
    let file_error = service
        .delete_file(&active_id)
        .expect_err("an active task file cannot be deleted");
    assert_engine_code(&file_error, "INVALID_STATE");

    service.cancel(&active_id).expect("cancel active task");
    service.shutdown();
}

// --- 5. Shutdown semantics ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn service_shutdown_stops_later_operations_and_is_idempotent() {
    let store_dir = tempdir().expect("store tempdir");
    let dest = tempdir().expect("dest tempdir");
    let store = Database::open(store_dir.path().join("tasks.db")).expect("open store");
    let service = EngineService::start(
        store,
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    )
    .expect("start service");

    service.shutdown();
    service.shutdown();

    let list_error = service
        .list()
        .expect_err("a stopped service must reject list");
    assert_engine_code(&list_error, "SERVICE_STOPPED");
    let add_error = service
        .add(add_download(
            "http://127.0.0.1/unused.bin".to_string(),
            "unused.bin",
            1,
        ))
        .expect_err("a stopped service must reject add");
    assert_engine_code(&add_error, "SERVICE_STOPPED");
}

// --- 6. Command mapping and missing-service error ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn commands_map_completed_task_and_segments_to_camel_case_and_require_engine() {
    let body = pattern_bytes(37, 128 * 1024);
    let server = fixture(body.clone());
    let store_dir = tempdir().expect("store tempdir");
    let dest = tempdir().expect("dest tempdir");
    let store = Database::open(store_dir.path().join("tasks.db")).expect("open store");
    let service = Arc::new(
        EngineService::start(
            store,
            dest.path().to_path_buf(),
            EngineOptions { max_concurrent: 1 },
        )
        .expect("start service"),
    );
    let harness = build_state(Some(Arc::clone(&service)));

    let id = service
        .add(add_download(server.url("/command.bin"), "command.bin", 2))
        .expect("add download");
    wait_for_state(&service, &id, TaskState::Completed).await;

    let summaries = summary_values(&harness.state);
    let summary = summaries
        .iter()
        .find(|summary| summary.get("id") == Some(&json!(id)))
        .expect("completed task must be summarized");
    let object = summary.as_object().expect("summary must be an object");
    for key in [
        "id",
        "url",
        "filename",
        "destination",
        "state",
        "downloadedBytes",
        "totalBytes",
        "connections",
        "createdAtMs",
        "updatedAtMs",
        "completedAtMs",
        "errorCode",
        "errorMessage",
    ] {
        assert!(object.contains_key(key), "summary must expose {key}");
    }
    assert_eq!(summary_state(summary), "completed");
    assert_eq!(
        summary.get("downloadedBytes").and_then(Value::as_u64),
        Some(body.len() as u64)
    );
    assert_eq!(
        summary.get("totalBytes").and_then(Value::as_u64),
        Some(body.len() as u64)
    );
    assert!(
        summary.get("completedAtMs").is_some_and(Value::is_number),
        "a completed task must expose a completion timestamp"
    );

    let segments = downloads_segments(&harness.state, &id).expect("query command segments");
    assert!(!segments.is_empty(), "segment mapping must not be empty");
    let segment_values: Vec<Value> = segments
        .iter()
        .map(|segment| serde_json::to_value(segment).expect("segment json"))
        .collect();
    let first = segment_values[0]
        .as_object()
        .expect("segment must be an object");
    for key in ["index", "start", "end", "downloadedBytes", "state"] {
        assert!(first.contains_key(key), "segment must expose {key}");
    }
    assert_eq!(first.get("index").and_then(Value::as_u64), Some(0));
    assert_eq!(first.get("start").and_then(Value::as_u64), Some(0));
    assert_eq!(first.get("state").and_then(Value::as_str), Some("done"));
    assert_eq!(
        segment_values
            .last()
            .and_then(|segment| segment.get("end"))
            .and_then(Value::as_u64),
        Some(body.len() as u64)
    );

    let probe = downloads_probe(
        &harness.state,
        ProbeDownloadRequest {
            url: server.url("/command.bin"),
            allow_html: false,
            extra_headers: Vec::new(),
        },
    )
    .expect("probe command");
    assert_eq!(probe.total_bytes, Some(body.len() as u64));
    assert!(probe.range_supported, "fixture must report range support");
    assert!(
        probe.final_url.ends_with("/command.bin"),
        "probe command must report the final URL"
    );
    assert_eq!(
        probe.content_type.as_deref(),
        Some("application/octet-stream")
    );

    let bare = build_state(None);
    let error = downloads_list(&bare.state).expect_err("a missing service must fail");
    assert_eq!(error.code, "engine_unavailable");

    service.shutdown();
}

// --- 7. Settings persistence and live application to the service ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn settings_save_persists_schema_2_and_applies_limits_to_service() {
    let body = pattern_bytes(41, 512 * 1024);
    let server = RangeServer::start(FixtureConfig {
        body,
        stream_chunk_size: 16 * 1024,
        ..Default::default()
    });
    let store_dir = tempdir().expect("store tempdir");
    let dest = tempdir().expect("dest tempdir");
    let store = Database::open(store_dir.path().join("tasks.db")).expect("open store");
    let service = Arc::new(
        EngineService::start(
            store,
            dest.path().to_path_buf(),
            EngineOptions { max_concurrent: 1 },
        )
        .expect("start service"),
    );
    let harness = build_state(Some(Arc::clone(&service)));

    let mut settings = harness.state.settings();
    settings.max_concurrent_tasks = 2;
    settings.global_rate_limit = Some(32 * 1024);
    settings.default_directory = Some(dest.path().to_path_buf());
    settings.default_connections = 4;
    let saved = settings_save(&harness.state, settings).expect("save settings");
    assert_eq!(saved.schema_version, 2, "settings save must keep schema 2");

    let raw: Value = serde_json::from_str(
        &std::fs::read_to_string(&harness.settings_path).expect("read settings file"),
    )
    .expect("settings file must be JSON");
    assert_eq!(raw.get("schemaVersion"), Some(&json!(2)));
    assert_eq!(raw.get("maxConcurrentTasks"), Some(&json!(2)));
    assert_eq!(raw.get("globalRateLimit"), Some(&json!(32768)));
    assert_eq!(
        raw.get("defaultDirectory"),
        Some(&json!(dest.path().to_str().expect("dest path")))
    );

    let mut ids = Vec::new();
    for name in ["one.bin", "two.bin", "three.bin"] {
        ids.push(
            downloads_add(
                &harness.state,
                add_request(server.url(&format!("/{name}")), name, 4),
            )
            .expect("add download through command"),
        );
    }

    let start = Instant::now();
    loop {
        let summaries = summary_values(&harness.state);
        let active = summaries
            .iter()
            .filter(|summary| {
                matches!(
                    summary_state(summary),
                    "connecting" | "downloading" | "finalizing"
                )
            })
            .count();
        let queued = summaries
            .iter()
            .filter(|summary| summary_state(summary) == "queued")
            .count();
        if active == 2 && queued == 1 {
            break;
        }
        assert!(
            start.elapsed() < WAIT,
            "saved concurrency limit was not applied: {summaries:?}"
        );
        tokio::time::sleep(POLL).await;
    }

    let start = Instant::now();
    loop {
        let downloaded: u64 = service
            .list()
            .expect("list tasks")
            .iter()
            .map(|task| task.downloaded_bytes)
            .sum();
        if downloaded > 0 {
            assert!(
                downloaded < 512 * 1024u64,
                "the saved global rate limit must pace the active tasks"
            );
            break;
        }
        assert!(
            start.elapsed() < WAIT,
            "no progress was observed under the saved rate limit"
        );
        tokio::time::sleep(POLL).await;
    }

    for id in ids {
        service
            .cancel(&id)
            .expect("cancel task after settings check");
    }
    service.shutdown();
}

// --- 8. Open file reports a missing final file without launching ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn downloads_open_file_reports_file_missing_for_a_removed_final_file() {
    let body = pattern_bytes(43, 64 * 1024);
    let server = fixture(body);
    let store_dir = tempdir().expect("store tempdir");
    let dest = tempdir().expect("dest tempdir");
    let store = Database::open(store_dir.path().join("tasks.db")).expect("open store");
    let service = Arc::new(
        EngineService::start(
            store,
            dest.path().to_path_buf(),
            EngineOptions { max_concurrent: 1 },
        )
        .expect("start service"),
    );
    let harness = build_state(Some(Arc::clone(&service)));

    let id = service
        .add(add_download(server.url("/open.bin"), "open.bin", 1))
        .expect("add download");
    wait_for_state(&service, &id, TaskState::Completed).await;
    let final_path = dest.path().join("open.bin");
    assert!(final_path.exists(), "completed final file must exist");
    std::fs::remove_file(&final_path).expect("remove final file");

    let error = downloads_open_file(&harness.state, &id)
        .expect_err("opening a removed final file must fail before launching");
    assert_eq!(error.code, "FILE_MISSING");

    service.shutdown();
}
