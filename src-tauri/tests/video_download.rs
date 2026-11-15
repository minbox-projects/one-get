//! Behavior tests for the long-lived video download service: one-at-a-time
//! execution, persisted progress, unique output with a recorded hash,
//! pause/resume/cancel/retry, component preflight, and bounded shutdown.
//!
//! Every download runs a copy of the deterministic `video_fixture` binary
//! installed as the fake `yt-dlp`. The copy's file name selects the scenario
//! behavior, so the suite uses no public network and no real media tools.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use oneget_lib::engine::http::ProxyConfig;
use oneget_lib::engine::state::TaskState;
use oneget_lib::kernel::catalog::Target;
use oneget_lib::kernel::store::{ComponentInstall, ComponentStore, InstalledFile};
use oneget_lib::store::{Database, Task, TaskKind};
use oneget_lib::video::process::TimeoutPolicy;
use oneget_lib::video::service::{StartVideoRequest, VideoService, VideoServiceConfig};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

fn request(harness: &Harness, filename: &str, full_url_persisted: bool) -> StartVideoRequest {
    StartVideoRequest {
        url: "https://fixture.invalid/watch?v=fixture".to_string(),
        minimized_url: "https://fixture.invalid/watch".to_string(),
        filename: filename.to_string(),
        destination: Some(harness.destination.clone()),
        format_selector: "bestvideo+bestaudio/best".to_string(),
        audio_only: false,
        metadata: json!({
            "title": "Fixture Video",
            "duration": 12.5,
            "uploader": "Fixture",
        }),
        cookies_file: None,
        proxy_url: None,
        full_url_persisted,
    }
}

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

fn wait_for_state(store: &Database, id: &str, wanted: TaskState) -> Task {
    wait_until(WAIT_TIMEOUT, "the requested task state", || {
        store
            .task(id)
            .ok()
            .flatten()
            .filter(|task| task.state == wanted)
    })
}

fn wait_for_completed(store: &Database, id: &str, observed: &mut Vec<TaskState>) -> Task {
    wait_until(WAIT_TIMEOUT, "the task to complete", || {
        let task = store.task(id).ok().flatten()?;
        if observed.last() != Some(&task.state) {
            observed.push(task.state);
        }
        (task.state == TaskState::Completed).then_some(task)
    })
}

fn output_files(destination: &Path) -> Vec<PathBuf> {
    fs::read_dir(destination)
        .expect("destination reads")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| matches!(extension, "mp4" | "m4a"))
        })
        .collect()
}

/// The task id encoded in the service-provided `-o <temp>/video.%(ext)s`
/// template, where `<temp>` is the task's `.oneget-video-<id>` directory.
fn args_task_id(args: &[String]) -> Option<String> {
    let template = args.get(args.iter().position(|argument| argument == "-o")? + 1)?;
    let name = Path::new(template).parent()?.file_name()?.to_str()?;
    name.strip_prefix(".oneget-video-").map(str::to_string)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Deterministic fake component install over a temporary kernel root.
///
/// The `yt-dlp` entry is a copy of the fixture binary under a scenario-specific
/// file name, and the fixture derives its behavior from that name; `ffmpeg` and
/// `ffprobe` are fixture copies too so the resolver verifies a complete install.
struct Harness {
    _dir: TempDir,
    kernel_root: PathBuf,
    components_db: PathBuf,
    destination: PathBuf,
    store_path: PathBuf,
    fixture: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let kernel_root = dir.path().join("kernel");
        let components_db = dir.path().join("components.db");
        let destination = dir.path().join("downloads");
        let store_path = dir.path().join("tasks.db");
        fs::create_dir_all(&kernel_root).expect("kernel root creates");
        fs::create_dir_all(&destination).expect("destination creates");
        Self {
            _dir: dir,
            kernel_root,
            components_db,
            destination,
            store_path,
            fixture: PathBuf::from(env!("CARGO_BIN_EXE_video_fixture")),
        }
    }

    fn store(&self) -> Database {
        Database::open(&self.store_path).expect("open task store")
    }

    fn config(&self, store: Database, timeouts: TimeoutPolicy) -> VideoServiceConfig {
        VideoServiceConfig {
            store,
            kernel_root: self.kernel_root.clone(),
            components_db_path: self.components_db.clone(),
            default_destination: self.destination.clone(),
            timeouts,
            progress_persist_interval: Duration::from_millis(10),
            proxy: None,
        }
    }

    fn install_download(&self, behavior: &str) {
        let yt_dir = self.kernel_root.join("yt-dlp/1-test");
        let ffmpeg_dir = self.kernel_root.join("ffmpeg/2-test");
        fs::create_dir_all(&yt_dir).expect("yt-dlp version dir creates");
        fs::create_dir_all(&ffmpeg_dir).expect("ffmpeg version dir creates");

        let yt_path = copy_fixture(&self.fixture, &yt_dir, behavior);
        let ffmpeg_path = copy_fixture(&self.fixture, &ffmpeg_dir, "ffmpeg");
        let ffprobe_path = copy_fixture(&self.fixture, &ffmpeg_dir, "ffprobe");

        let yt_files = vec![installed_file("yt-dlp", file_name(&yt_path), &yt_path)];
        let ffmpeg_files = vec![
            installed_file("ffmpeg", file_name(&ffmpeg_path), &ffmpeg_path),
            installed_file("ffprobe", file_name(&ffprobe_path), &ffprobe_path),
        ];
        self.record("yt-dlp", "test", 1, yt_files);
        self.record("ffmpeg", "test", 2, ffmpeg_files);
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

    /// Every fixture invocation, in the order the fixture logged its argv.
    fn args_log(&self) -> Vec<Vec<String>> {
        let path = self.destination.join(".fixture-args.jsonl");
        let Ok(text) = fs::read_to_string(&path) else {
            return Vec::new();
        };
        text.lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str::<Vec<String>>(line)
                    .expect("args log line is a JSON string array")
            })
            .collect()
    }

    fn args_for_task(&self, task_id: &str) -> Vec<Vec<String>> {
        self.args_log()
            .into_iter()
            .filter(|args| args_task_id(args).as_deref() == Some(task_id))
            .collect()
    }
}

/// Install a fixture copy under the platform-appropriate executable name.
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
        sha256: sha256_hex(&data),
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

#[test]
fn add_runs_one_task_with_progress_output_hash_and_completion() {
    let harness = Harness::new();
    harness.install_download("yt-dlp-slow");
    let service = VideoService::start(harness.config(harness.store(), TimeoutPolicy::default()))
        .expect("video service starts");
    let id = service
        .add(request(&harness, "clip.mp4", true))
        .expect("video task is added");
    let reader = harness.store();
    let mut observed = Vec::new();

    let task = wait_for_completed(&reader, &id, &mut observed);

    assert_eq!(task.kind, TaskKind::Video, "the persisted kind is video");
    assert!(
        task.downloaded_bytes > 0,
        "progress bytes must be persisted, got {}",
        task.downloaded_bytes
    );
    assert!(
        task.total_bytes.is_some_and(|total| total > 0),
        "the total from the progress template is persisted"
    );
    assert!(
        observed.contains(&TaskState::Connecting) && observed.contains(&TaskState::Downloading),
        "the observed state sequence must include connecting and downloading: {observed:?}"
    );
    assert!(
        task.video
            .as_ref()
            .and_then(|video| video.get("mergeStage"))
            .is_none_or(Value::is_null),
        "a non-merge download must not record a merge stage"
    );

    let outputs = output_files(&harness.destination);
    assert_eq!(outputs.len(), 1, "exactly one output file must exist");
    let final_path = outputs[0].clone();
    assert_eq!(
        final_path.file_name().and_then(|name| name.to_str()),
        Some(task.filename.as_str()),
        "the stored filename must name the final file"
    );
    let stored_hash = task
        .actual_sha256
        .clone()
        .expect("completion stores the hash");
    assert_eq!(
        stored_hash,
        sha256_hex(&fs::read(&final_path).expect("final output reads")),
        "the stored SHA-256 must match the final file"
    );
    let final_path_value = task
        .video
        .as_ref()
        .and_then(|video| video.get("finalPath"))
        .and_then(Value::as_str)
        .expect("the video record stores finalPath");
    assert_eq!(Path::new(final_path_value), final_path.as_path());
    assert!(
        task.completed_at_ms.is_some(),
        "completion time is recorded"
    );
    assert!(
        !harness
            .destination
            .join(format!(".oneget-video-{id}"))
            .exists(),
        "the task temporary directory must be removed after completion"
    );

    let invocations = harness.args_for_task(&id);
    assert_eq!(
        invocations.len(),
        1,
        "one invocation per completed download"
    );
    let args = &invocations[0];
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--concurrent-fragments" && pair[1] == "4"),
        "yt-dlp must receive the bounded fragment count: {args:?}"
    );
    assert!(
        args.iter().any(|argument| argument == "--ffmpeg-location"),
        "yt-dlp must receive the active ffmpeg directory: {args:?}"
    );
    assert!(
        args.iter()
            .any(|argument| argument == "https://fixture.invalid/watch?v=fixture"),
        "yt-dlp must receive the full source URL: {args:?}"
    );

    service.shutdown().expect("shutdown succeeds");
}

#[test]
fn second_video_waits_as_queued_and_runs_after_the_first() {
    let harness = Harness::new();
    harness.install_download("yt-dlp-slow");
    let service = VideoService::start(harness.config(harness.store(), TimeoutPolicy::default()))
        .expect("video service starts");
    let first = service
        .add(request(&harness, "first.mp4", true))
        .expect("first video adds");
    let second = service
        .add(request(&harness, "second.mp4", true))
        .expect("second video adds");
    let reader = harness.store();

    wait_for_state(&reader, &first, TaskState::Downloading);
    let second_task = reader
        .task(&second)
        .expect("second task reads")
        .expect("second task exists");
    assert_eq!(
        second_task.state,
        TaskState::Queued,
        "a second video must stay queued while one is active"
    );

    let mut max_active = 0;
    wait_until(WAIT_TIMEOUT, "both videos to complete", || {
        let tasks = reader.list_tasks().expect("tasks list");
        let active = tasks
            .iter()
            .filter(|task| task.kind == TaskKind::Video && task.state.is_active())
            .count();
        max_active = max_active.max(active);
        let first_done = tasks
            .iter()
            .any(|task| task.id == first && task.state == TaskState::Completed);
        let second_done = tasks
            .iter()
            .any(|task| task.id == second && task.state == TaskState::Completed);
        (first_done && second_done).then_some(())
    });
    assert!(
        max_active <= 1,
        "two video processes must never be active at once, observed {max_active}"
    );
    let log = harness.args_log();
    assert_eq!(
        harness.args_for_task(&first).len(),
        1,
        "the first task must record exactly one invocation"
    );
    assert_eq!(
        harness.args_for_task(&second).len(),
        1,
        "the second task must record exactly one invocation"
    );
    let first_index = log
        .iter()
        .position(|args| args_task_id(args).as_deref() == Some(first.as_str()))
        .expect("the first invocation is logged");
    let second_index = log
        .iter()
        .position(|args| args_task_id(args).as_deref() == Some(second.as_str()))
        .expect("the second invocation is logged");
    assert!(
        first_index < second_index,
        "the queued second video must only start after the first"
    );
    assert_eq!(
        output_files(&harness.destination).len(),
        2,
        "both videos must produce one output each"
    );

    service.shutdown().expect("shutdown succeeds");
}

#[test]
fn pause_preserves_temp_and_resume_continues() {
    let harness = Harness::new();
    harness.install_download("yt-dlp-slow");
    let service = VideoService::start(harness.config(harness.store(), TimeoutPolicy::default()))
        .expect("video service starts");
    let id = service
        .add(request(&harness, "pause.mp4", true))
        .expect("video task adds");
    let reader = harness.store();
    let temp_dir = harness.destination.join(format!(".oneget-video-{id}"));

    wait_until(WAIT_TIMEOUT, "downloading progress", || {
        reader
            .task(&id)
            .ok()
            .flatten()
            .filter(|task| task.state == TaskState::Downloading && task.downloaded_bytes > 0)
    });
    service.pause(&id).expect("pause succeeds");
    let paused = wait_for_state(&reader, &id, TaskState::Paused);
    assert!(
        paused.downloaded_bytes > 0,
        "pause preserves the persisted progress"
    );
    assert!(temp_dir.exists(), "pause preserves the task temp directory");

    service.resume(&id).expect("resume succeeds");
    let mut observed = Vec::new();
    let completed = wait_for_completed(&reader, &id, &mut observed);

    let invocations = harness.args_for_task(&id);
    assert!(
        invocations.len() >= 2,
        "pause and resume must both log their invocation, saw {}",
        invocations.len()
    );
    assert!(
        invocations
            .iter()
            .any(|args| args.iter().any(|argument| argument == "--continue")),
        "the resumed invocation must pass --continue"
    );
    assert!(
        completed.downloaded_bytes >= paused.downloaded_bytes,
        "resume must not lose the previously persisted progress"
    );
    assert_eq!(
        output_files(&harness.destination).len(),
        1,
        "completion produces exactly one final file"
    );
    assert!(!temp_dir.exists(), "completion removes the temp directory");

    service.shutdown().expect("shutdown succeeds");
}

#[test]
fn cancel_removes_only_task_temp_and_rejects_completed() {
    let harness = Harness::new();
    harness.install_download("yt-dlp-slow");
    let service = VideoService::start(harness.config(harness.store(), TimeoutPolicy::default()))
        .expect("video service starts");
    let running = service
        .add(request(&harness, "running.mp4", true))
        .expect("running video adds");
    let queued = service
        .add(request(&harness, "queued.mp4", true))
        .expect("queued video adds");
    let reader = harness.store();
    let running_temp = harness.destination.join(format!(".oneget-video-{running}"));

    wait_for_state(&reader, &running, TaskState::Downloading);
    service
        .cancel(&running)
        .expect("cancel of the running task succeeds");
    wait_for_state(&reader, &running, TaskState::Cancelled);
    assert!(
        !running_temp.exists(),
        "cancel removes only the running task temp directory"
    );
    let queued_task = reader
        .task(&queued)
        .expect("queued task reads")
        .expect("queued task exists");
    assert_ne!(
        queued_task.state,
        TaskState::Cancelled,
        "cancelling the running task must not cancel the queued one"
    );

    wait_for_state(&reader, &queued, TaskState::Completed);
    let error = service
        .cancel(&queued)
        .expect_err("cancelling a completed task must fail");
    assert!(
        !error.code().is_empty(),
        "the rejection must carry a stable typed code"
    );
    assert_eq!(
        reader
            .task(&queued)
            .expect("completed task reads")
            .expect("completed task exists")
            .state,
        TaskState::Completed,
        "a rejected cancel leaves the completed state unchanged"
    );
    assert!(
        output_files(&harness.destination).len() == 1,
        "the completed file survives the rejected cancel"
    );

    service.shutdown().expect("shutdown succeeds");
}

#[test]
fn missing_output_fails_without_final_file_and_retry_recovers() {
    let missing = Harness::new();
    missing.install_download("yt-dlp-missing");
    let missing_service =
        VideoService::start(missing.config(missing.store(), TimeoutPolicy::default()))
            .expect("video service starts");
    let missing_id = missing_service
        .add(request(&missing, "missing.mp4", true))
        .expect("missing-output task adds");
    let missing_reader = missing.store();
    let failed = wait_for_state(&missing_reader, &missing_id, TaskState::Failed);
    assert_eq!(
        failed.error_code.as_deref(),
        Some("VIDEO_OUTPUT_MISSING"),
        "a missing output fails with the stable output-missing code"
    );
    assert!(
        output_files(&missing.destination).is_empty(),
        "a missing output must create no final file"
    );
    missing_service.shutdown().expect("shutdown succeeds");

    let retry = Harness::new();
    retry.install_download("yt-dlp-fail-once");
    let retry_service = VideoService::start(retry.config(retry.store(), TimeoutPolicy::default()))
        .expect("video service starts");
    let retry_id = retry_service
        .add(request(&retry, "retry.mp4", true))
        .expect("retry task adds");
    let retry_reader = retry.store();
    let first_failure = wait_for_state(&retry_reader, &retry_id, TaskState::Failed);
    assert_eq!(
        first_failure.error_code.as_deref(),
        Some("VIDEO_PROCESS_FAILED"),
        "the first fixture run must fail with the process-failure code"
    );

    retry_service
        .retry(&retry_id)
        .expect("retry requeues a failed task");
    wait_for_state(&retry_reader, &retry_id, TaskState::Completed);
    assert_eq!(
        output_files(&retry.destination).len(),
        1,
        "the retried task produces exactly one final file"
    );

    retry_service.shutdown().expect("shutdown succeeds");
}

#[test]
fn component_preflight_blocks_before_process_and_task() {
    let harness = Harness::new();
    let service = VideoService::start(harness.config(harness.store(), TimeoutPolicy::default()))
        .expect("video service starts without installed components");

    let error = service
        .add(request(&harness, "blocked.mp4", true))
        .expect_err("a missing component must block the add");
    assert_eq!(
        error.code(),
        "COMPONENT_MISSING",
        "the kernel preflight code must be preserved through the video error"
    );

    let reader = harness.store();
    assert!(
        reader.list_tasks().expect("tasks list").is_empty(),
        "preflight failure must create no task row"
    );
    assert!(
        fs::read_dir(&harness.destination)
            .expect("destination reads")
            .next()
            .is_none(),
        "preflight failure must create no output or temp data"
    );
    assert!(
        !harness.destination.join(".fixture-args.jsonl").exists(),
        "no process was started"
    );

    service.shutdown().expect("shutdown succeeds");
}

#[test]
fn shutdown_pauses_active_within_bound() {
    let harness = Harness::new();
    harness.install_download("yt-dlp-slow");
    let timeouts = TimeoutPolicy {
        shutdown: Duration::from_secs(1),
        ..TimeoutPolicy::default()
    };
    let service =
        VideoService::start(harness.config(harness.store(), timeouts)).expect("service starts");
    let id = service
        .add(request(&harness, "shutdown.mp4", true))
        .expect("video task adds");
    let reader = harness.store();
    wait_for_state(&reader, &id, TaskState::Downloading);

    let started = Instant::now();
    service.shutdown().expect("shutdown succeeds");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(6),
        "shutdown must stay inside the five-second bound, took {elapsed:?}"
    );

    let task = reader.task(&id).expect("task reads").expect("task exists");
    assert_eq!(task.state, TaskState::Paused, "shutdown pauses active work");
    assert!(
        harness
            .destination
            .join(format!(".oneget-video-{id}"))
            .exists(),
        "shutdown preserves the task temp directory"
    );

    // The slow fake writes its output only after about two seconds, so waiting
    // past that window proves shutdown terminated the child before it finished.
    let temp_dir = harness.destination.join(format!(".oneget-video-{id}"));
    sleep(Duration::from_millis(2500));
    assert!(
        output_files(&temp_dir).is_empty(),
        "shutdown must terminate the fixture before it writes output"
    );
}

#[test]
fn download_js_runtime_failure_is_typed() {
    let harness = Harness::new();
    harness.install_download("yt-dlp-js-runtime-error");
    let service = VideoService::start(harness.config(harness.store(), TimeoutPolicy::default()))
        .expect("video service starts");
    let id = service
        .add(request(&harness, "js-runtime.mp4", true))
        .expect("js-runtime-error task adds");
    let reader = harness.store();
    let failed = wait_for_state(&reader, &id, TaskState::Failed);
    assert_eq!(
        failed.error_code.as_deref(),
        Some("VIDEO_JS_RUNTIME_MISSING"),
        "a JavaScript-runtime failure must map to the typed code"
    );
    assert!(
        harness
            .destination
            .join(format!(".oneget-video-{id}"))
            .exists(),
        "the task temporary directory must be preserved"
    );
    assert!(
        output_files(&harness.destination).is_empty(),
        "a JavaScript-runtime failure must create no final file"
    );

    service.shutdown().expect("shutdown succeeds");
}

// ---------------------------------------------------------------------------
// Global proxy delivery to the yt-dlp child: environment variables only.
// ---------------------------------------------------------------------------

const RECORDING_PROXY_URL: &str = "http://127.0.0.1:18931";
const RECORDING_PROXY_AUTHORITY: &str = "127.0.0.1:18931";
const RECORDING_PROXY_USER: &str = "oneget-download-proxy-user";
const RECORDING_PROXY_PASSWORD: &str = "oneget-download-proxy-password-secret";

impl Harness {
    /// Install a recording mock as the yt-dlp component: the mock writes its
    /// argv and environment to files under the returned directory, then exits
    /// with a deterministic failure so the task fails VIDEO_PROCESS_FAILED.
    fn install_recording_mock(&self) -> PathBuf {
        let record_dir = self.destination.join(".recording");
        fs::create_dir_all(&record_dir).expect("record dir creates");
        let yt_dir = self.kernel_root.join("yt-dlp/1-test");
        let ffmpeg_dir = self.kernel_root.join("ffmpeg/2-test");
        fs::create_dir_all(&yt_dir).expect("yt-dlp version dir creates");
        fs::create_dir_all(&ffmpeg_dir).expect("ffmpeg version dir creates");

        let yt_path = write_recording_script(&yt_dir, &record_dir);
        let ffmpeg_path = copy_fixture(&self.fixture, &ffmpeg_dir, "ffmpeg");
        let ffprobe_path = copy_fixture(&self.fixture, &ffmpeg_dir, "ffprobe");

        let yt_files = vec![installed_file("yt-dlp", file_name(&yt_path), &yt_path)];
        let ffmpeg_files = vec![
            installed_file("ffmpeg", file_name(&ffmpeg_path), &ffmpeg_path),
            installed_file("ffprobe", file_name(&ffprobe_path), &ffprobe_path),
        ];
        self.record("yt-dlp", "test", 1, yt_files);
        self.record("ffmpeg", "test", 2, ffmpeg_files);
        record_dir
    }
}

#[cfg(unix)]
fn write_recording_script(dir: &Path, record_dir: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let quote = |path: &Path| {
        format!(
            "'{}'",
            path.to_str()
                .expect("record path is UTF-8")
                .replace('\'', "'\\''")
        )
    };
    let argv = quote(&record_dir.join("argv.txt"));
    let env = quote(&record_dir.join("env.txt"));
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > {argv}\nenv > {env}\necho \"mock recording failure\" >&2\nexit 3\n"
    );
    let path = dir.join("yt-dlp-proxy-env");
    fs::write(&path, script).expect("recording script writes");
    let mut permissions = fs::metadata(&path).expect("script metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&path, permissions).expect("script becomes executable");
    path
}

#[cfg(windows)]
fn write_recording_script(dir: &Path, record_dir: &Path) -> PathBuf {
    let quote = |path: &Path| {
        format!(
            "\"{}\"",
            path.to_str()
                .expect("record path is UTF-8")
                .replace('"', "\"\"")
        )
    };
    let argv = quote(&record_dir.join("argv.txt"));
    let env = quote(&record_dir.join("env.txt"));
    let script = format!(
        "@echo off\r\n(for %%A in (%*) do @echo %%A)>{argv}\r\nset>{env}\r\necho mock recording failure 1>&2\r\nexit /b 3\r\n"
    );
    let path = dir.join("yt-dlp-proxy-env.cmd");
    fs::write(&path, script).expect("recording script writes");
    path
}

#[test]
fn global_proxy_is_delivered_to_the_child_via_environment_only() {
    let harness = Harness::new();
    let record_dir = harness.install_recording_mock();
    let mut config = harness.config(harness.store(), TimeoutPolicy::default());
    config.proxy = Some(ProxyConfig {
        url: RECORDING_PROXY_URL.to_string(),
        username: Some(RECORDING_PROXY_USER.to_string()),
        password: Some(RECORDING_PROXY_PASSWORD.to_string()),
    });
    let service = VideoService::start(config).expect("video service starts");
    let id = service
        .add(request(&harness, "proxy-env.mp4", true))
        .expect("video task is added");
    let reader = harness.store();

    let task = wait_for_state(&reader, &id, TaskState::Failed);
    assert_eq!(
        task.error_code.as_deref(),
        Some("VIDEO_PROCESS_FAILED"),
        "the recording mock exits with a deterministic failure"
    );

    let env = fs::read_to_string(record_dir.join("env.txt"))
        .expect("the child must record its environment");
    for variable in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
        let lines: Vec<&str> = env
            .lines()
            .filter(|line| line.starts_with(&format!("{variable}=")))
            .collect();
        assert_eq!(
            lines.len(),
            1,
            "{variable} must be set exactly once for the child"
        );
        let line = lines[0];
        assert!(
            line.contains(RECORDING_PROXY_AUTHORITY),
            "{variable} must carry the proxy authority: {line}"
        );
        assert!(
            line.contains(RECORDING_PROXY_USER),
            "{variable} must carry the proxy username"
        );
        assert!(
            line.contains(RECORDING_PROXY_PASSWORD),
            "{variable} must carry the proxy password"
        );
    }

    let argv =
        fs::read_to_string(record_dir.join("argv.txt")).expect("the child must record its argv");
    assert!(
        !argv.contains(RECORDING_PROXY_USER),
        "the child argv must not carry the proxy username"
    );
    assert!(
        !argv.contains(RECORDING_PROXY_PASSWORD),
        "the child argv must not carry the proxy password"
    );
    assert!(
        !argv.contains(RECORDING_PROXY_AUTHORITY),
        "the child argv must not carry the proxy authority"
    );
    assert!(
        !argv.lines().any(|argument| argument == "--proxy"),
        "the child argv must not carry a --proxy argument"
    );

    service.shutdown().expect("shutdown succeeds");
}
