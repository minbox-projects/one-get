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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use oneget_lib::app_state::AppState;
use oneget_lib::commands::app::{
    hf_token_clear, hf_token_save, hf_token_status, proxy_credentials_clear,
    proxy_credentials_save, proxy_credentials_status, settings_save, AutostartService,
    NotificationSendResult, NotificationService, NotificationStatus, QuitService,
    SecretRoundTripResult, SecretService, SecretStatus,
};
use oneget_lib::commands::downloads::{
    downloads_add, downloads_delete_file, downloads_list, downloads_open_file, downloads_probe,
    downloads_segments, AddDownloadRequest, ProbeDownloadRequest,
};
use oneget_lib::config::{ProxyMode, ProxyType, SettingsStore, VideoQualityPreference};
use oneget_lib::engine::error::EngineError;
use oneget_lib::engine::part_file::PartFile;
use oneget_lib::engine::scheduler::EngineOptions;
use oneget_lib::engine::service::{AddDownload, EngineService, ProbeRequest};
use oneget_lib::engine::state::{SegmentState, TaskState};
use oneget_lib::error::AppError;
use oneget_lib::logging::{LogConfig, Logger};
use oneget_lib::platform::files::CapDir;
use oneget_lib::platform::paths::AppPaths;
use oneget_lib::platform::secrets::{MemorySecretStore, SecretError, SecretStore, SecretValue};
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

fn secret_unavailable() -> AppError {
    AppError::new("secret_unavailable", "platform secret store is unavailable")
}

impl SecretService for NoopSecrets {
    fn roundtrip(&self, _secret: &str) -> Result<SecretRoundTripResult, AppError> {
        Ok(SecretRoundTripResult { success: true })
    }

    fn get_secret(&self, _key: &str) -> Result<Option<SecretValue>, AppError> {
        Err(secret_unavailable())
    }

    fn set_secret(&self, _key: &str, _value: &SecretValue) -> Result<(), AppError> {
        Err(secret_unavailable())
    }

    fn delete_secret(&self, _key: &str) -> Result<(), AppError> {
        Err(secret_unavailable())
    }
}

/// In-memory credential store used by the write-only secret command tests.
struct MemorySecrets {
    inner: MemorySecretStore,
}

impl MemorySecrets {
    fn new() -> Self {
        Self {
            inner: MemorySecretStore::new(),
        }
    }
}

impl SecretService for MemorySecrets {
    fn roundtrip(&self, _secret: &str) -> Result<SecretRoundTripResult, AppError> {
        Ok(SecretRoundTripResult { success: true })
    }

    fn get_secret(&self, key: &str) -> Result<Option<SecretValue>, AppError> {
        self.inner.get(key).map_err(map_secret_error)
    }

    fn set_secret(&self, key: &str, value: &SecretValue) -> Result<(), AppError> {
        self.inner.set(key, value).map_err(map_secret_error)
    }

    fn delete_secret(&self, key: &str) -> Result<(), AppError> {
        self.inner.delete(key).map_err(map_secret_error)
    }
}

fn map_secret_error(error: SecretError) -> AppError {
    AppError::new(error.code, error.message)
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
    build_state_with_secrets(engine, Arc::new(NoopSecrets))
}

fn build_state_with_secrets(
    engine: Option<Arc<EngineService>>,
    secrets: Arc<dyn SecretService>,
) -> StateHarness {
    let temp = tempdir().expect("tempdir");
    let paths = AppPaths::with_root(temp.path().join("app")).expect("app paths");
    let settings_path = paths.data.join("settings.json");
    let settings = SettingsStore::load(&settings_path).expect("settings store");
    let logger = Logger::new(LogConfig::with_defaults(paths.logs.clone())).expect("logger");
    let mut state = AppState::new(
        paths,
        settings,
        logger,
        secrets,
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
        cookies_file: None,
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
        cookies_file: None,
        cookie_handle: None,
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

// --- 9. A pending probe does not block list or controls ---

#[test]
fn stalled_probe_does_not_block_list_while_pending() {
    let body = pattern_bytes(71, 32 * 1024);
    let server = RangeServer::start(FixtureConfig {
        body,
        stall_before_response: Some(Duration::from_millis(1_000)),
        ..Default::default()
    });
    let probe_url = server.url("/stalled-probe.bin");
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

    let probe_service = Arc::clone(&service);
    let finished = Arc::new(AtomicBool::new(false));
    let probe_finished = Arc::clone(&finished);
    let probe_thread = std::thread::spawn(move || {
        let result = probe_service.probe(ProbeRequest {
            url: probe_url,
            allow_html: false,
            extra_headers: Vec::new(),
        });
        probe_finished.store(true, Ordering::SeqCst);
        result
    });

    let start = Instant::now();
    while server.requests().is_empty() {
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "the stalled probe request never reached the fixture"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    let list_start = Instant::now();
    let tasks = service.list().expect("list while a probe is pending");
    let list_elapsed = list_start.elapsed();
    assert!(
        list_elapsed < Duration::from_millis(300),
        "list must return while the probe is still pending, took {list_elapsed:?}"
    );
    assert!(
        !finished.load(Ordering::SeqCst),
        "the list call must be measured while the probe is still pending"
    );
    assert!(tasks.is_empty(), "no download tasks were added");

    let outcome = probe_thread
        .join()
        .expect("probe thread must not panic")
        .expect("the stalled probe must eventually complete");
    assert_eq!(outcome.total_bytes, Some(32 * 1024));
    service.shutdown();
}

// --- 10. Final URL redaction and persistence ---

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn downloads_list_redacts_and_persists_final_url_after_redirect() {
    let body = pattern_bytes(73, 128 * 1024);
    let target = RangeServer::start(FixtureConfig {
        body: body.clone(),
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let redirect = RangeServer::start(FixtureConfig {
        redirect_location: Some(target.url("/final.bin?token=redirect-secret#fragment")),
        ..Default::default()
    });

    let store_dir = tempdir().expect("store tempdir");
    let dest = tempdir().expect("dest tempdir");
    let db_path = store_dir.path().join("tasks.db");
    let store = Database::open(&db_path).expect("open store");
    let service = Arc::new(
        EngineService::start(
            store,
            dest.path().to_path_buf(),
            EngineOptions { max_concurrent: 1 },
        )
        .expect("start service"),
    );
    let harness = build_state(Some(Arc::clone(&service)));

    let id = downloads_add(
        &harness.state,
        add_request(
            redirect.url("/original.bin?token=original-secret"),
            "redacted.bin",
            1,
        ),
    )
    .expect("add redirected download");
    wait_for_state(&service, &id, TaskState::Completed).await;
    assert_eq!(
        std::fs::read(dest.path().join("redacted.bin")).expect("read final file"),
        body,
        "the redirected download must finalize the target bytes"
    );

    let summaries = downloads_list(&harness.state).expect("list download summaries");
    let summary = summaries
        .iter()
        .find(|summary| summary.id == id)
        .expect("completed task must be summarized");
    let final_url = summary
        .final_url
        .as_deref()
        .expect("a completed redirected download must expose its final URL");
    assert!(
        final_url.ends_with("/final.bin"),
        "final URL must name the redirected target, got {final_url}"
    );
    assert!(
        !final_url.contains('?')
            && !final_url.contains('#')
            && !final_url.contains('@')
            && !final_url.contains("secret"),
        "final URL must be redacted, got {final_url}"
    );
    let serialized = serde_json::to_value(summary).expect("summary json");
    assert_eq!(
        serialized.get("finalUrl").and_then(Value::as_str),
        Some(final_url),
        "DownloadSummary must serialize final_url as finalUrl"
    );

    service.shutdown();
    drop(harness);

    let reopened = Database::open(&db_path).expect("reopen store after service shutdown");
    let task = reopened
        .task(&id)
        .expect("lookup task after reopen")
        .expect("completed task must survive reopen");
    assert_eq!(
        task.final_url.as_deref(),
        Some(final_url),
        "the redacted final URL must persist across a reopened store"
    );
}

// --- 11. Settings save rejects invalid new field values atomically ---

#[test]
fn settings_save_rejects_invalid_new_field_values_and_keeps_file_byte_identical() {
    let harness = build_state(None);
    let before = std::fs::read(&harness.settings_path).expect("read settings before invalid saves");
    let base = harness.state.settings();

    type SettingsMutation = Box<dyn Fn(&mut oneget_lib::config::Settings)>;
    let cases: Vec<(&str, SettingsMutation)> = vec![
        (
            "proxy_port_zero_in_manual_mode",
            Box::new(|settings| {
                settings.proxy_mode = ProxyMode::Manual;
                settings.proxy_host = Some("127.0.0.1".to_string());
                settings.proxy_port = Some(0);
            }),
        ),
        (
            "hf_connection_cap_zero",
            Box::new(|settings| {
                settings.hf_connection_cap = 0;
            }),
        ),
        (
            "hf_connection_cap_above_maximum",
            Box::new(|settings| {
                settings.hf_connection_cap = 65;
            }),
        ),
        (
            "connect_timeout_below_minimum",
            Box::new(|settings| {
                settings.connect_timeout_secs = 4;
            }),
        ),
        (
            "connect_timeout_above_maximum",
            Box::new(|settings| {
                settings.connect_timeout_secs = 121;
            }),
        ),
        (
            "read_timeout_below_minimum",
            Box::new(|settings| {
                settings.read_timeout_secs = 4;
            }),
        ),
        (
            "read_timeout_above_maximum",
            Box::new(|settings| {
                settings.read_timeout_secs = 301;
            }),
        ),
        (
            "retry_attempts_zero",
            Box::new(|settings| {
                settings.retry_attempts = 0;
            }),
        ),
        (
            "retry_attempts_eleven",
            Box::new(|settings| {
                settings.retry_attempts = 11;
            }),
        ),
        (
            "user_agent_empty",
            Box::new(|settings| {
                settings.user_agent = Some(String::new());
            }),
        ),
        (
            "user_agent_with_carriage_return",
            Box::new(|settings| {
                settings.user_agent = Some("fixture\rvalue".to_string());
            }),
        ),
        (
            "user_agent_with_line_feed",
            Box::new(|settings| {
                settings.user_agent = Some("fixture\nvalue".to_string());
            }),
        ),
        (
            "manual_mode_without_proxy_host",
            Box::new(|settings| {
                settings.proxy_mode = ProxyMode::Manual;
                settings.proxy_host = None;
            }),
        ),
        (
            "manual_mode_with_empty_proxy_host",
            Box::new(|settings| {
                settings.proxy_mode = ProxyMode::Manual;
                settings.proxy_host = Some(String::new());
            }),
        ),
    ];

    for (case, mutate) in cases {
        let mut candidate = base.clone();
        mutate(&mut candidate);
        let error = settings_save(&harness.state, candidate)
            .expect_err("invalid new field values must be rejected");
        assert_eq!(
            error.code, "INVALID_SETTINGS",
            "{case} must fail with INVALID_SETTINGS"
        );
        let after =
            std::fs::read(&harness.settings_path).expect("read settings after rejected save");
        assert_eq!(
            after, before,
            "{case} must leave the settings file byte-identical"
        );
        assert_eq!(
            harness.state.settings(),
            base,
            "{case} must not change the in-memory settings snapshot"
        );
    }
}

#[test]
fn settings_save_accepts_valid_boundary_documents() {
    for low_boundary in [true, false] {
        let harness = build_state(None);
        let cookies = harness._temp.path().join("cookies.txt");
        std::fs::write(&cookies, b"# fixture cookies\n").expect("write cookies fixture");

        let mut settings = harness.state.settings();
        settings.proxy_mode = ProxyMode::Manual;
        settings.proxy_type = ProxyType::Socks5;
        settings.proxy_host = Some("proxy.example.test".to_string());
        if low_boundary {
            settings.proxy_port = Some(1);
            settings.connect_timeout_secs = 5;
            settings.read_timeout_secs = 5;
            settings.retry_attempts = 1;
            settings.hf_connection_cap = 1;
            settings.video_quality_preference = VideoQualityPreference::Mp4Compatible;
        } else {
            settings.proxy_port = Some(65535);
            settings.connect_timeout_secs = 120;
            settings.read_timeout_secs = 300;
            settings.retry_attempts = 10;
            settings.hf_connection_cap = 64;
            settings.video_quality_preference = VideoQualityPreference::Highest;
        }
        settings.always_ask_quality = false;
        settings.user_agent = Some("OneGetFixture/1.0".to_string());
        settings.default_cookies_file = Some(cookies);

        let saved =
            settings_save(&harness.state, settings).expect("a valid boundary document must save");
        assert_eq!(saved.schema_version, 2);
        assert!(matches!(saved.proxy_mode, ProxyMode::Manual));
        assert!(matches!(saved.proxy_type, ProxyType::Socks5));
        assert_eq!(saved.proxy_host.as_deref(), Some("proxy.example.test"));
        if low_boundary {
            assert_eq!(saved.proxy_port, Some(1));
            assert_eq!(saved.connect_timeout_secs, 5);
            assert_eq!(saved.read_timeout_secs, 5);
            assert_eq!(saved.retry_attempts, 1);
            assert_eq!(saved.hf_connection_cap, 1);
            assert!(matches!(
                saved.video_quality_preference,
                VideoQualityPreference::Mp4Compatible
            ));
        } else {
            assert_eq!(saved.proxy_port, Some(65535));
            assert_eq!(saved.connect_timeout_secs, 120);
            assert_eq!(saved.read_timeout_secs, 300);
            assert_eq!(saved.retry_attempts, 10);
            assert_eq!(saved.hf_connection_cap, 64);
            assert!(matches!(
                saved.video_quality_preference,
                VideoQualityPreference::Highest
            ));
        }

        let reloaded = SettingsStore::load(&harness.settings_path)
            .expect("reload saved boundary settings")
            .settings();
        assert_eq!(
            reloaded, saved,
            "saved boundary document must reload identically"
        );
    }
}

// --- 12. Write-only secret commands ---

fn secret_state() -> StateHarness {
    build_state_with_secrets(None, Arc::new(MemorySecrets::new()))
}

fn assert_single_boolean_status(status: SecretStatus, configured: bool) {
    assert_eq!(status.configured, configured);
    let serialized = serde_json::to_value(status).expect("status must serialize");
    let object = serialized
        .as_object()
        .expect("status must serialize as an object");
    assert_eq!(object.len(), 1, "status must expose exactly one key");
    assert_eq!(object.get("configured"), Some(&json!(configured)));
}

#[test]
fn secret_statuses_start_unconfigured_and_serialize_as_a_single_boolean() {
    let harness = secret_state();

    let hf = hf_token_status(&harness.state).expect("hf token status");
    let proxy = proxy_credentials_status(&harness.state).expect("proxy credentials status");
    assert_single_boolean_status(hf, false);
    assert_single_boolean_status(proxy, false);
}

#[test]
fn hf_token_save_status_and_clear_round_trip_without_exposing_values() {
    let harness = secret_state();

    let saved = hf_token_save(&harness.state, "hf_fixture_token_value_123".to_string())
        .expect("save hf token");
    assert_single_boolean_status(saved, true);

    let status = hf_token_status(&harness.state).expect("status after save");
    assert_single_boolean_status(status, true);

    let cleared = hf_token_clear(&harness.state).expect("clear hf token");
    assert_single_boolean_status(cleared, false);

    let status = hf_token_status(&harness.state).expect("status after clear");
    assert_single_boolean_status(status, false);

    let rendered = serde_json::to_string(&status).expect("render status");
    assert!(
        !rendered.contains("hf_fixture_token_value_123"),
        "the status payload must not carry the token value"
    );
}

#[test]
fn proxy_credentials_save_status_and_clear_round_trip_without_exposing_values() {
    let harness = secret_state();

    let saved = proxy_credentials_save(
        &harness.state,
        "fixture-proxy-user".to_string(),
        "fixture-proxy-password".to_string(),
    )
    .expect("save proxy credentials");
    assert_single_boolean_status(saved, true);

    let status = proxy_credentials_status(&harness.state).expect("status after save");
    assert_single_boolean_status(status, true);

    let cleared = proxy_credentials_clear(&harness.state).expect("clear proxy credentials");
    assert_single_boolean_status(cleared, false);

    let status = proxy_credentials_status(&harness.state).expect("status after clear");
    assert_single_boolean_status(status, false);

    let rendered = serde_json::to_string(&status).expect("render status");
    assert!(
        !rendered.contains("fixture-proxy-user") && !rendered.contains("fixture-proxy-password"),
        "the status payload must not carry credential values"
    );
}

#[test]
fn empty_secret_inputs_are_rejected_and_never_become_configured() {
    let harness = secret_state();

    let error = hf_token_save(&harness.state, String::new())
        .expect_err("an empty hf token must be rejected");
    assert!(
        !error.code.is_empty(),
        "an empty hf token must fail with a typed error"
    );
    assert!(
        !hf_token_status(&harness.state)
            .expect("status after rejected token save")
            .configured,
        "a rejected token save must not become configured"
    );

    let error = proxy_credentials_save(
        &harness.state,
        String::new(),
        "fixture-proxy-password".to_string(),
    )
    .expect_err("an empty proxy username must be rejected");
    assert!(
        !error.code.is_empty(),
        "an empty proxy username must fail with a typed error"
    );
    assert!(
        !proxy_credentials_status(&harness.state)
            .expect("status after rejected username save")
            .configured,
        "a rejected credential save must not become configured"
    );

    let error = proxy_credentials_save(
        &harness.state,
        "fixture-proxy-user".to_string(),
        String::new(),
    )
    .expect_err("an empty proxy password must be rejected");
    assert!(
        !error.code.is_empty(),
        "an empty proxy password must fail with a typed error"
    );
    assert!(
        !proxy_credentials_status(&harness.state)
            .expect("status after rejected password save")
            .configured,
        "a rejected credential save must not become configured"
    );
}

// --- 13. AC-016: file deletion failure keeps the record and file ---

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn delete_file_on_read_only_destination_keeps_record_and_file() {
    use std::os::unix::fs::PermissionsExt;

    struct RestorePermissions {
        path: PathBuf,
        mode: u32,
    }

    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            let _ =
                std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(self.mode));
        }
    }

    let body = pattern_bytes(79, 64 * 1024);
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
        .add(add_download(server.url("/locked.bin"), "locked.bin", 1))
        .expect("add download");
    wait_for_state(&service, &id, TaskState::Completed).await;
    let final_path = dest.path().join("locked.bin");
    assert!(final_path.exists(), "completed final file must exist");

    let original_mode = std::fs::metadata(dest.path())
        .expect("destination metadata")
        .permissions()
        .mode()
        & 0o777;
    let restore = RestorePermissions {
        path: dest.path().to_path_buf(),
        mode: original_mode,
    };
    std::fs::set_permissions(dest.path(), std::fs::Permissions::from_mode(0o555))
        .expect("make destination read-only");

    let error = downloads_delete_file(&harness.state, &id)
        .expect_err("deleting from a read-only destination must fail");
    assert!(
        !error.code.is_empty(),
        "the failure must carry a typed code"
    );
    assert!(
        find_task(&service, &id).is_some(),
        "a failed delete_file must keep the task record"
    );
    assert!(
        final_path.exists(),
        "a failed delete_file must keep the completed final file"
    );

    drop(restore);
    assert!(
        final_path.exists(),
        "the final file must remain after cleanup"
    );
    service.shutdown();
}

// --- Hugging Face command-surface behavior (RED) ---

#[path = "support/hf_server.rs"]
mod hf_server;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn downloads_list_exposes_huggingface_summary_and_delete_removes_hf_row() {
    use hf_server::{HfFixtureConfig, HfFixtureServer, ResolverBehavior};
    use oneget_lib::commands::downloads::downloads_delete_record;
    use oneget_lib::engine::scheduler::HuggingFaceOptions;
    use oneget_lib::store::hf::HfMetadataStore;
    use sha2::{Digest, Sha256};

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    let body = b"hf command summary fixture bytes".to_vec();
    let expected_hash = sha256_hex(&body);
    let cdn_url = "http://cdn-lfs.huggingface.co/repos/fixture/summary.bin";
    let server = HfFixtureServer::start(HfFixtureConfig {
        resolver_behavior: ResolverBehavior::RedirectToCdn(cdn_url.to_string()),
        cdn_body: body.clone(),
        ..Default::default()
    });

    let store_dir = tempdir().expect("store tempdir");
    let dest = tempdir().expect("dest tempdir");
    let meta_dir = tempdir().expect("meta tempdir");
    let hf_store = HfMetadataStore::open(meta_dir.path().join("hf.db")).expect("open hf store");
    let hf_store_for_engine =
        HfMetadataStore::open(meta_dir.path().join("hf.db")).expect("open hf store handle");

    let store = Database::open(store_dir.path().join("tasks.db")).expect("open store");
    let service = Arc::new(
        EngineService::start(
            store,
            dest.path().to_path_buf(),
            EngineOptions { max_concurrent: 2 },
        )
        .expect("start service"),
    );

    // Route resolver/CDN traffic through the loopback forward proxy and enable HF.
    service
        .set_proxy(Some(oneget_lib::engine::http::ProxyConfig {
            url: server.proxy_url(),
            username: None,
            password: None,
        }))
        .expect("set proxy");
    service
        .apply_huggingface_options(HuggingFaceOptions {
            token: None,
            connection_cap: 4,
            metadata: Some(Arc::new(hf_store_for_engine)),
        })
        .expect("apply hf options");

    let harness = build_state(Some(Arc::clone(&service)));

    let id = service
        .add(add_download(
            "http://huggingface.co/gpt2/resolve/main/summary.bin".to_string(),
            "summary.bin",
            8,
        ))
        .expect("add hf download");
    wait_for_state(&service, &id, TaskState::Completed).await;

    let final_bytes = std::fs::read(dest.path().join("summary.bin")).expect("read final file");
    assert_eq!(sha256_hex(&final_bytes), expected_hash);

    let summaries = summary_values(&harness.state);
    let summary = summaries
        .iter()
        .find(|summary| summary.get("id") == Some(&json!(id)))
        .expect("completed hf task must be summarized");
    let object = summary.as_object().expect("summary must be an object");
    assert!(
        object.contains_key("huggingface"),
        "DownloadSummary must expose a huggingface field (camelCase), got keys: {:?}",
        object.keys().collect::<Vec<_>>()
    );
    let hf = summary
        .get("huggingface")
        .and_then(Value::as_object)
        .expect("huggingface must be an object for an HF task");
    for key in [
        "repoId",
        "filePath",
        "revision",
        "effectiveConnections",
        "refreshCount",
        "downgraded",
    ] {
        assert!(
            hf.contains_key(key),
            "huggingface summary must expose {key}, got {:?}",
            hf.keys().collect::<Vec<_>>()
        );
    }
    assert_eq!(hf.get("repoId").and_then(Value::as_str), Some("gpt2"));
    assert_eq!(
        hf.get("filePath").and_then(Value::as_str),
        Some("summary.bin")
    );
    assert_eq!(hf.get("revision").and_then(Value::as_str), Some("main"));
    assert_eq!(
        hf.get("effectiveConnections").and_then(Value::as_u64),
        Some(4),
        "effective connections must reflect hfConnectionCap"
    );
    assert_eq!(hf.get("refreshCount").and_then(Value::as_u64), Some(0));
    assert_eq!(hf.get("downgraded").and_then(Value::as_bool), Some(false));

    let rendered = serde_json::to_string(summary).expect("render summary");
    assert!(
        !rendered.contains("hf_test")
            && !rendered.contains("Bearer")
            && !rendered.contains("token"),
        "summary must never expose tokens or signed URLs: {rendered}"
    );

    // Deleting the record through the existing command must remove the HF row.
    downloads_delete_record(&harness.state, &id).expect("delete record via command");
    let after = hf_store.get(&id).expect("get hf metadata after delete");
    assert!(
        after.is_none(),
        "downloads_delete_record must remove the HuggingFace metadata row"
    );

    service.shutdown();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn downloads_list_reports_null_huggingface_when_metadata_unavailable() {
    use hf_server::{HfFixtureConfig, HfFixtureServer, ResolverBehavior};
    use oneget_lib::engine::scheduler::HuggingFaceOptions;
    use sha2::{Digest, Sha256};

    fn sha256_hex(bytes: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(bytes);
        hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    let body = b"hf missing metadata fixture bytes".to_vec();
    let expected_hash = sha256_hex(&body);
    let cdn_url = "http://cdn-lfs.huggingface.co/repos/fixture/nullhf.bin";
    let server = HfFixtureServer::start(HfFixtureConfig {
        resolver_behavior: ResolverBehavior::RedirectToCdn(cdn_url.to_string()),
        cdn_body: body.clone(),
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
    service
        .set_proxy(Some(oneget_lib::engine::http::ProxyConfig {
            url: server.proxy_url(),
            username: None,
            password: None,
        }))
        .expect("set proxy");
    // Deliberately do not attach HF metadata store/options metadata: listing must
    // remain usable and report huggingface as null/unavailable.
    service
        .apply_huggingface_options(HuggingFaceOptions {
            token: None,
            connection_cap: 4,
            metadata: None,
        })
        .expect("apply hf options without metadata store");

    let harness = build_state(Some(Arc::clone(&service)));
    let id = service
        .add(add_download(
            "http://huggingface.co/gpt2/resolve/main/nullhf.bin".to_string(),
            "nullhf.bin",
            4,
        ))
        .expect("add hf download");
    wait_for_state(&service, &id, TaskState::Completed).await;

    let final_bytes = std::fs::read(dest.path().join("nullhf.bin")).expect("read final file");
    assert_eq!(sha256_hex(&final_bytes), expected_hash);

    let summaries = summary_values(&harness.state);
    let summary = summaries
        .iter()
        .find(|summary| summary.get("id") == Some(&json!(id)))
        .expect("completed hf task must be summarized");
    assert!(
        summary.get("huggingface").is_some() && summary.get("huggingface").unwrap().is_null()
            || summary.get("huggingface").is_none(),
        "when hf metadata store is unavailable, huggingface details must be null/unavailable, got: {summary}"
    );

    service.shutdown();
}
