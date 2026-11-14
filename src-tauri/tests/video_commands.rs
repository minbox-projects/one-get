//! Behavior tests for the video command surface: probe choices, start
//! validation, kind-aware listing and control routing, empty video segments,
//! and the application-state wiring contract.
//!
//! Fake component installs follow the video download suite: `yt-dlp` is either a
//! copy of the deterministic `video_fixture` binary or a wrapper that runs its
//! probe mode, so no public network and no real media tools are used.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use oneget_lib::app_state::AppState;
use oneget_lib::commands::app::{
    AutostartService, NotificationSendResult, NotificationService, NotificationStatus, QuitService,
    SecretRoundTripResult, SecretService,
};
use oneget_lib::commands::downloads::{
    downloads_cancel, downloads_list, downloads_pause, downloads_retry, downloads_segments,
};
use oneget_lib::commands::kernel::KernelRuntime;
use oneget_lib::commands::video::{video_probe, video_start, VideoProbeRequest, VideoStartRequest};
use oneget_lib::config::SettingsStore;
use oneget_lib::engine::scheduler::EngineOptions;
use oneget_lib::engine::service::EngineService;
use oneget_lib::engine::state::TaskState;
use oneget_lib::kernel::catalog::Target;
use oneget_lib::kernel::store::{ComponentInstall, ComponentStore, InstalledFile};
use oneget_lib::logging::{LogConfig, Logger};
use oneget_lib::platform::paths::AppPaths;
use oneget_lib::store::{Database, TaskKind, VideoTaskInput};
use oneget_lib::video::process::TimeoutPolicy;
use oneget_lib::video::service::{VideoService, VideoServiceConfig};
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::{tempdir, TempDir};

const WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

fn wait_until<T>(timeout: Duration, context: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {timeout:?} waiting for {context}"
        );
        sleep(POLL_INTERVAL);
    }
}

fn wait_for_state(store: &Database, id: &str, wanted: TaskState) {
    wait_until(WAIT_TIMEOUT, "the requested task state", || {
        store
            .task(id)
            .ok()
            .flatten()
            .filter(|task| task.state == wanted)
    });
}

fn task_state(store: &Database, id: &str) -> TaskState {
    store
        .task(id)
        .expect("task reads")
        .expect("task exists")
        .state
}

// --- Test fakes for the AppState service dependencies -----------------------

#[derive(Clone, Copy)]
struct NoopSecrets;

impl SecretService for NoopSecrets {
    fn roundtrip(
        &self,
        _secret: &str,
    ) -> Result<SecretRoundTripResult, oneget_lib::error::AppError> {
        Ok(SecretRoundTripResult { success: true })
    }
}

#[derive(Clone, Copy)]
struct NoopNotifications;

impl NotificationService for NoopNotifications {
    fn status(&self) -> Result<NotificationStatus, oneget_lib::error::AppError> {
        Ok(NotificationStatus {
            permission: "granted".to_string(),
        })
    }

    fn send_test(&self) -> Result<NotificationSendResult, oneget_lib::error::AppError> {
        Ok(NotificationSendResult { sent: true })
    }
}

#[derive(Clone, Copy)]
struct NoopAutostart;

impl AutostartService for NoopAutostart {
    fn is_enabled(&self) -> Result<bool, oneget_lib::error::AppError> {
        Ok(false)
    }

    fn set_enabled(&self, enabled: bool) -> Result<bool, oneget_lib::error::AppError> {
        Ok(enabled)
    }
}

#[derive(Clone, Copy)]
struct NoopQuit;

impl QuitService for NoopQuit {
    fn request_quit(&self) -> Result<(), oneget_lib::error::AppError> {
        Ok(())
    }
}

// --- Harness ----------------------------------------------------------------

struct Harness {
    dir: TempDir,
    kernel_root: PathBuf,
    components_db: PathBuf,
    destination: PathBuf,
    store_path: PathBuf,
    fixture: PathBuf,
    engine: Arc<EngineService>,
    video: Arc<VideoService>,
    state: AppState,
}

impl Harness {
    fn new() -> Self {
        let dir = tempdir().expect("tempdir");
        let paths = AppPaths::with_root(dir.path().join("app")).expect("app paths");
        let kernel_root = paths.data.join("kernel");
        let components_db = paths.data.join("components.db");
        let store_path = paths.data.join("tasks.db");
        fs::create_dir_all(&kernel_root).expect("kernel root creates");

        let settings =
            SettingsStore::load(paths.data.join("settings.json")).expect("settings store");
        let logger = Logger::new(LogConfig::with_defaults(paths.logs.clone())).expect("logger");

        let engine = Arc::new(
            EngineService::start(
                Database::open(&store_path).expect("open engine store"),
                paths.downloads.clone(),
                EngineOptions { max_concurrent: 2 },
            )
            .expect("engine service starts"),
        );
        let video = Arc::new(
            VideoService::start(VideoServiceConfig {
                store: Database::open(&store_path).expect("open video store"),
                kernel_root: kernel_root.clone(),
                components_db_path: components_db.clone(),
                default_destination: paths.downloads.clone(),
                timeouts: TimeoutPolicy::default(),
                progress_persist_interval: Duration::from_millis(10),
            })
            .expect("video service starts"),
        );
        let kernel = KernelRuntime::new(
            components_db.clone(),
            kernel_root.clone(),
            paths.cache.join("kernel"),
            None,
            None,
            Target::current(),
        )
        .expect("kernel runtime constructs");

        let state = AppState::new(
            paths.clone(),
            settings,
            logger,
            Arc::new(NoopSecrets),
            Arc::new(NoopNotifications),
            Arc::new(NoopAutostart),
            Arc::new(NoopQuit),
        )
        .with_engine(Arc::clone(&engine))
        .with_kernel(kernel)
        .with_video(Arc::clone(&video));

        Self {
            destination: paths.downloads.clone(),
            dir,
            kernel_root,
            components_db,
            store_path,
            fixture: PathBuf::from(env!("CARGO_BIN_EXE_video_fixture")),
            engine,
            video,
            state,
        }
    }

    fn store(&self) -> Database {
        Database::open(&self.store_path).expect("open task store")
    }

    fn shutdown(&self) {
        self.video.shutdown().expect("video shutdown succeeds");
        self.engine.shutdown();
    }

    /// Install a fake downloader plus the ffmpeg and ffprobe roles.
    fn install_download(&self) {
        let yt_dir = self.kernel_root.join("yt-dlp/1-test");
        let ffmpeg_dir = self.kernel_root.join("ffmpeg/2-test");
        fs::create_dir_all(&yt_dir).expect("yt-dlp version dir creates");
        fs::create_dir_all(&ffmpeg_dir).expect("ffmpeg version dir creates");

        let yt_path = copy_fixture(&self.fixture, &yt_dir, "yt-dlp-slow");
        let ffmpeg_path = copy_fixture(&self.fixture, &ffmpeg_dir, "ffmpeg");
        let ffprobe_path = copy_fixture(&self.fixture, &ffmpeg_dir, "ffprobe");

        self.record(
            "yt-dlp",
            "test",
            1,
            vec![installed_file("yt-dlp", file_name(&yt_path), &yt_path)],
        );
        self.record(
            "ffmpeg",
            "test",
            2,
            vec![
                installed_file("ffmpeg", file_name(&ffmpeg_path), &ffmpeg_path),
                installed_file("ffprobe", file_name(&ffprobe_path), &ffprobe_path),
            ],
        );
    }

    /// Install a `yt-dlp` wrapper that returns the fixture probe JSON.
    fn install_probe(&self) {
        let yt_dir = self.kernel_root.join("yt-dlp/1-probe");
        let ffmpeg_dir = self.kernel_root.join("ffmpeg/2-probe");
        fs::create_dir_all(&yt_dir).expect("yt-dlp version dir creates");
        fs::create_dir_all(&ffmpeg_dir).expect("ffmpeg version dir creates");

        let yt_path = mock_probe_program(&yt_dir, &["probe-json"]);
        let ffmpeg_path = copy_fixture(&self.fixture, &ffmpeg_dir, "ffmpeg");
        let ffprobe_path = copy_fixture(&self.fixture, &ffmpeg_dir, "ffprobe");

        self.record(
            "yt-dlp",
            "probe",
            1,
            vec![installed_file("yt-dlp", file_name(&yt_path), &yt_path)],
        );
        self.record(
            "ffmpeg",
            "probe",
            2,
            vec![
                installed_file("ffmpeg", file_name(&ffmpeg_path), &ffmpeg_path),
                installed_file("ffprobe", file_name(&ffprobe_path), &ffprobe_path),
            ],
        );
    }

    fn record(&self, component: &str, version: &str, sequence: u64, files: Vec<InstalledFile>) {
        let install = ComponentInstall {
            component: component.to_string(),
            version: version.to_string(),
            sequence,
            target: Target::current().as_str().to_string(),
            version_dir: format!("{component}/{sequence}-{version}"),
            files,
            installed_at_ms: 1_760_000_000_000,
        };
        ComponentStore::open(&self.components_db)
            .expect("open component store")
            .record_active(&install)
            .expect("record active component");
    }
}

fn copy_fixture(source: &Path, directory: &Path, base: &str) -> PathBuf {
    let path = directory.join(fake_name(base));
    fs::copy(source, &path).expect("fixture copy writes");
    make_executable(&path);
    path
}

#[cfg(windows)]
fn fake_name(base: &str) -> String {
    format!("{base}.exe")
}

#[cfg(not(windows))]
fn fake_name(base: &str) -> String {
    base.to_string()
}

fn file_name(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .expect("fixture copy has a UTF-8 file name")
}

fn installed_file(role: &str, name: &str, path: &Path) -> InstalledFile {
    let data = fs::read(path).expect("installed file reads");
    InstalledFile {
        role: role.to_string(),
        name: name.to_string(),
        sha256: hex::encode(Sha256::digest(&data)),
        size: data.len() as u64,
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path).expect("file metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("file becomes executable");
}

#[cfg(windows)]
fn make_executable(_path: &Path) {}

#[cfg(unix)]
fn mock_probe_program(dir: &Path, arguments: &[&str]) -> PathBuf {
    let mut command = shell_quote(env!("CARGO_BIN_EXE_video_fixture"));
    for argument in arguments {
        command.push(' ');
        command.push_str(&shell_quote(argument));
    }
    let path = dir.join("yt-dlp");
    fs::write(&path, format!("#!/bin/sh\nexec {command}\n")).expect("mock script writes");
    make_executable(&path);
    path
}

#[cfg(unix)]
fn shell_quote(argument: &str) -> String {
    format!("'{}'", argument.replace('\'', "'\\''"))
}

#[cfg(windows)]
fn mock_probe_program(dir: &Path, arguments: &[&str]) -> PathBuf {
    let mut command = format!("\"{}\"", env!("CARGO_BIN_EXE_video_fixture"));
    for argument in arguments {
        command.push(' ');
        command.push_str(&format!("\"{}\"", argument.replace('"', "\"\"")));
    }
    let path = dir.join("yt-dlp.cmd");
    fs::write(&path, format!("@echo off\r\n{command}\r\n")).expect("mock script writes");
    path
}

fn start_request(harness: &Harness, filename: &str) -> VideoStartRequest {
    VideoStartRequest {
        url: "https://fixture.invalid/watch?v=fixture".to_string(),
        filename: filename.to_string(),
        destination: Some(harness.destination.clone()),
        format_selector: "137+140".to_string(),
        audio_only: false,
        metadata: json!({
            "title": "Fixture Video",
            "duration": 12.5,
            "uploader": "Fixture",
        }),
        cookies_file: None,
        proxy_url: None,
    }
}

fn create_persisted_video(store: &Database, harness: &Harness, quality: &str) -> String {
    let input = VideoTaskInput {
        url: "https://fixture.invalid/watch".to_string(),
        filename: "clip.mp4".to_string(),
        destination: Some(harness.destination.to_string_lossy().into_owned()),
        format_selector: quality.to_string(),
        cookies_file: None,
        proxy_url: None,
        metadata: json!({
            "title": "Fixture Video",
            "duration": 123.5,
            "uploader": "Fixture",
            "quality": quality,
        }),
        full_url_persisted: true,
    };
    let id = store.create_video_task(&input).expect("video task creates");
    store
        .patch_video_json(
            &id,
            &json!({ "audioOnly": true, "mergeStage": "merge", "quality": quality }),
        )
        .expect("video record patches");
    id
}

// --- Tests ------------------------------------------------------------------

#[test]
fn downloads_list_exposes_video_kind_and_metadata() {
    let harness = Harness::new();
    let store = harness.store();
    let video_id = create_persisted_video(&store, &harness, "1080p");
    let http_id = store
        .create_task("https://fixture.invalid/file.bin", "file.bin")
        .expect("http task creates");
    store
        .transition_task(&http_id, TaskState::Queued, TaskState::Paused)
        .expect("the http task pauses");

    let summaries = downloads_list(&harness.state).expect("list summaries");
    let video = summaries
        .iter()
        .find(|summary| summary.id == video_id)
        .expect("the video row is listed");
    assert_eq!(video.kind, "video");
    let metadata = video.video.as_ref().expect("the video metadata is exposed");
    assert_eq!(metadata.title.as_deref(), Some("Fixture Video"));
    assert_eq!(metadata.duration_secs, Some(123.5));
    assert_eq!(metadata.uploader.as_deref(), Some("Fixture"));
    assert_eq!(metadata.quality.as_deref(), Some("1080p"));
    assert_eq!(metadata.stage.as_deref(), Some("merge"));
    assert!(metadata.audio_only, "the audio-only flag is exposed");

    let http = summaries
        .iter()
        .find(|summary| summary.id == http_id)
        .expect("the http row is listed");
    assert_eq!(http.kind, "http");
    assert!(
        http.video.is_none(),
        "an http row exposes no video metadata"
    );

    harness.shutdown();
}

#[test]
fn control_commands_route_by_task_kind() {
    let harness = Harness::new();
    harness.install_download();
    let store = harness.store();
    let http_id = store
        .create_task("https://fixture.invalid/http.bin", "http.bin")
        .expect("http task creates");
    store
        .transition_task(&http_id, TaskState::Queued, TaskState::Paused)
        .expect("the http task pauses");
    assert_eq!(task_state(&store, &http_id), TaskState::Paused);

    let video_id = video_start(&harness.state, start_request(&harness, "clip.mp4"))
        .expect("a video task starts");
    wait_for_state(&harness.store(), &video_id, TaskState::Downloading);

    downloads_pause(&harness.state, &video_id).expect("pause routes to the video service");
    wait_for_state(&harness.store(), &video_id, TaskState::Paused);
    assert_eq!(
        task_state(&store, &http_id),
        TaskState::Paused,
        "pausing a video must not touch the http task"
    );

    downloads_retry(&harness.state, &video_id).expect("retry routes to the video service");
    wait_for_state(&harness.store(), &video_id, TaskState::Downloading);
    assert_eq!(
        task_state(&store, &http_id),
        TaskState::Paused,
        "retrying a video must not touch the http task"
    );

    downloads_cancel(&harness.state, &video_id).expect("cancel routes to the video service");
    wait_for_state(&harness.store(), &video_id, TaskState::Cancelled);
    assert_eq!(
        task_state(&store, &http_id),
        TaskState::Paused,
        "cancelling a video must not touch the http task"
    );

    harness.shutdown();
}

#[test]
fn downloads_segments_for_video_is_empty() {
    let harness = Harness::new();
    let video_id = create_persisted_video(&harness.store(), &harness, "1080p");

    let segments = downloads_segments(&harness.state, &video_id).expect("segments load");
    assert!(segments.is_empty(), "a video task has no byte segment rows");

    harness.shutdown();
}

#[test]
fn video_start_rejects_invalid_options_before_persisting() {
    let harness = Harness::new();
    harness.install_download();
    let store = harness.store();

    let mut requests = Vec::new();
    let mut missing_cookies = start_request(&harness, "clip.mp4");
    missing_cookies.cookies_file = Some(harness.dir.path().join("missing-cookies.txt"));
    requests.push(missing_cookies);

    let mut userinfo_proxy = start_request(&harness, "clip.mp4");
    userinfo_proxy.proxy_url = Some("http://user:pass@127.0.0.1:8080".to_string());
    requests.push(userinfo_proxy);

    let mut non_http_proxy = start_request(&harness, "clip.mp4");
    non_http_proxy.proxy_url = Some("socks5://127.0.0.1:1080".to_string());
    requests.push(non_http_proxy);

    let mut invalid_proxy_port = start_request(&harness, "clip.mp4");
    invalid_proxy_port.proxy_url = Some("http://127.0.0.1:70000".to_string());
    requests.push(invalid_proxy_port);

    let destination_file = harness.dir.path().join("not-a-directory");
    fs::write(&destination_file, b"file").expect("destination file writes");
    let mut invalid_destination = start_request(&harness, "clip.mp4");
    invalid_destination.destination = Some(destination_file);
    requests.push(invalid_destination);

    let mut non_http_source = start_request(&harness, "clip.mp4");
    non_http_source.url = "ftp://fixture.invalid/video".to_string();
    requests.push(non_http_source);

    for (index, request) in requests.into_iter().enumerate() {
        let error = video_start(&harness.state, request)
            .expect_err(&format!("case {index} must be rejected"));
        assert!(
            !error.code.is_empty(),
            "case {index} must carry a typed code"
        );
        assert!(
            store.list_tasks().expect("tasks list").is_empty(),
            "case {index} must create no task row"
        );
    }

    let id = video_start(&harness.state, start_request(&harness, "valid.mp4"))
        .expect("a valid request creates one task");
    let tasks = store.list_tasks().expect("tasks list");
    assert_eq!(tasks.len(), 1, "a valid request creates exactly one task");
    assert_eq!(tasks[0].id, id);
    assert_eq!(tasks[0].kind, TaskKind::Video);

    harness.shutdown();
}

#[test]
fn video_probe_returns_quality_choices() {
    let harness = Harness::new();
    harness.install_probe();

    let result = video_probe(
        &harness.state,
        VideoProbeRequest {
            url: "https://fixture.invalid/watch?v=abc".to_string(),
            cookies_file: None,
            proxy_url: None,
        },
    )
    .expect("the probe succeeds");

    assert_eq!(result.title, "Fixture Video");
    assert_eq!(result.duration_secs, Some(123.5));
    assert_eq!(result.uploader.as_deref(), Some("Fixture"));
    assert_eq!(result.video.len(), 1, "one video choice");
    assert_eq!(result.video[0].id, "137");
    assert_eq!(result.video[0].label, "1080p");
    assert!(result.video[0].mp4_compatible);
    assert_eq!(result.video[0].format_selector, "137+bestaudio");
    assert!(
        result.video[0].size_bytes.is_none(),
        "unknown size stays unknown"
    );
    assert_eq!(result.audio.len(), 1, "one audio-only choice");
    assert_eq!(result.audio[0].id, "140");
    assert_eq!(result.audio[0].format_selector, "140");
    assert_eq!(result.default_index, Some(0));
    assert!(
        harness.store().list_tasks().expect("tasks list").is_empty(),
        "a probe creates no task"
    );

    harness.shutdown();

    let missing = Harness::new();
    let error = video_probe(
        &missing.state,
        VideoProbeRequest {
            url: "https://fixture.invalid/watch?v=abc".to_string(),
            cookies_file: None,
            proxy_url: None,
        },
    )
    .expect_err("a missing component must fail the probe");
    assert_eq!(error.code, "COMPONENT_MISSING");
    assert!(
        missing.store().list_tasks().expect("tasks list").is_empty(),
        "a failed probe creates no task"
    );
    missing.shutdown();
}
