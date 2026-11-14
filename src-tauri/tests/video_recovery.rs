//! Behavior tests for video task recovery: interrupted active work must fail
//! with a stable code while queued and paused work survives, and a retry after
//! a restart must require a full persisted source URL and reuse the preserved
//! temporary directory with `--continue`.
//!
//! Downloads run a copy of the deterministic `video_fixture` binary installed
//! as the fake `yt-dlp`, so no public network or real media tools are used.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use oneget_lib::engine::state::TaskState;
use oneget_lib::kernel::catalog::Target;
use oneget_lib::kernel::store::{ComponentInstall, ComponentStore, InstalledFile};
use oneget_lib::store::{Database, Task, TaskKind, VideoTaskInput};
use oneget_lib::video::process::TimeoutPolicy;
use oneget_lib::video::service::{StartVideoRequest, VideoService, VideoServiceConfig};
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

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

fn wait_for_state(store: &Database, id: &str, wanted: TaskState) -> Task {
    wait_until(WAIT_TIMEOUT, "the requested task state", || {
        store
            .task(id)
            .ok()
            .flatten()
            .filter(|task| task.state == wanted)
    })
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

fn request(
    harness: &Harness,
    filename: &str,
    full_url: &str,
    minimized_url: &str,
    full_url_persisted: bool,
) -> StartVideoRequest {
    StartVideoRequest {
        url: full_url.to_string(),
        minimized_url: minimized_url.to_string(),
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

    fn config(&self, store: Database) -> VideoServiceConfig {
        VideoServiceConfig {
            store,
            kernel_root: self.kernel_root.clone(),
            components_db_path: self.components_db.clone(),
            default_destination: self.destination.clone(),
            timeouts: TimeoutPolicy::default(),
            progress_persist_interval: Duration::from_millis(10),
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

fn create_video_task(store: &Database, harness: &Harness, filename: &str) -> (String, PathBuf) {
    let input = VideoTaskInput {
        url: "https://fixture.invalid/watch".to_string(),
        filename: filename.to_string(),
        destination: Some(harness.destination.to_string_lossy().into_owned()),
        format_selector: "bestvideo+bestaudio/best".to_string(),
        cookies_file: None,
        proxy_url: None,
        metadata: json!({"title": "Fixture Video"}),
        full_url_persisted: true,
    };
    let id = store.create_video_task(&input).expect("video task creates");
    let temp_dir = harness.destination.join(format!(".oneget-video-{id}"));
    fs::create_dir_all(&temp_dir).expect("task temp dir creates");
    store
        .patch_video_json(&id, &json!({"tempDir": temp_dir.to_string_lossy()}))
        .expect("temp dir persists into the video record");
    (id, temp_dir)
}

#[test]
fn recovery_marks_interrupted_failed_and_preserves_state() {
    let harness = Harness::new();
    let store = harness.store();

    let (queued, _) = create_video_task(&store, &harness, "queued.mp4");
    let (paused, paused_temp) = create_video_task(&store, &harness, "paused.mp4");
    store
        .transition_task(&paused, TaskState::Queued, TaskState::Paused)
        .expect("paused transition");
    let (connecting, connecting_temp) = create_video_task(&store, &harness, "connecting.mp4");
    store
        .transition_task(&connecting, TaskState::Queued, TaskState::Connecting)
        .expect("connecting transition");
    let (downloading, downloading_temp) = create_video_task(&store, &harness, "downloading.mp4");
    store
        .transition_task(&downloading, TaskState::Queued, TaskState::Connecting)
        .expect("connecting transition");
    store
        .transition_task(&downloading, TaskState::Connecting, TaskState::Downloading)
        .expect("downloading transition");
    let (finalizing, finalizing_temp) = create_video_task(&store, &harness, "finalizing.mp4");
    store
        .transition_task(&finalizing, TaskState::Queued, TaskState::Connecting)
        .expect("connecting transition");
    store
        .transition_task(&finalizing, TaskState::Connecting, TaskState::Downloading)
        .expect("downloading transition");
    store
        .transition_task(&finalizing, TaskState::Downloading, TaskState::Finalizing)
        .expect("finalizing transition");

    let service =
        VideoService::start(harness.config(store)).expect("video service recovers the store");
    let reader = harness.store();
    let tasks = reader.list_tasks().expect("tasks list");

    let by_id = |id: &str| -> &Task {
        tasks
            .iter()
            .find(|task| task.id == id)
            .expect("task remains persisted")
    };

    assert_eq!(
        by_id(&queued).state,
        TaskState::Queued,
        "queued stays queued"
    );
    assert_eq!(
        by_id(&paused).state,
        TaskState::Paused,
        "paused stays paused"
    );
    assert!(paused_temp.exists(), "paused preserves its temp dir");

    for (id, temp_dir) in [
        (&connecting, &connecting_temp),
        (&downloading, &downloading_temp),
        (&finalizing, &finalizing_temp),
    ] {
        let task = by_id(id);
        assert_eq!(
            task.state,
            TaskState::Failed,
            "an interrupted active or finalizing task must fail"
        );
        assert_eq!(
            task.error_code.as_deref(),
            Some("VIDEO_INTERRUPTED"),
            "the interrupted task carries the stable recovery code"
        );
        assert!(
            temp_dir.exists(),
            "recovery preserves the interrupted task temp dir"
        );
    }
    assert!(
        tasks.iter().all(|task| task.state != TaskState::Completed),
        "recovery must never mark a task completed without a verified output"
    );
    assert!(tasks.iter().all(|task| task.kind == TaskKind::Video));

    service.shutdown().expect("shutdown succeeds");
}

#[test]
fn retry_after_restart_requires_the_full_url() {
    let harness = Harness::new();
    harness.install_download("yt-dlp-slow");

    let first_service =
        VideoService::start(harness.config(harness.store())).expect("first service starts");
    let unpersisted = first_service
        .add(request(
            &harness,
            "unpersisted.mp4",
            "https://fixture.invalid/watch?v=token",
            "https://fixture.invalid/watch",
            false,
        ))
        .expect("unpersisted video adds");
    let reader = harness.store();
    wait_for_state(&reader, &unpersisted, TaskState::Downloading);
    first_service
        .pause(&unpersisted)
        .expect("pause preserves the temp dir");
    wait_for_state(&reader, &unpersisted, TaskState::Paused);
    assert!(harness
        .destination
        .join(format!(".oneget-video-{unpersisted}"))
        .exists());
    first_service.shutdown().expect("first service shuts down");

    let second_service =
        VideoService::start(harness.config(harness.store())).expect("second service starts");
    let error = second_service
        .retry(&unpersisted)
        .expect_err("retry without a persisted full URL must fail");
    assert_eq!(
        error.code(),
        "VIDEO_URL_UNAVAILABLE",
        "a minimized URL must never stand in for the full source"
    );
    assert_eq!(
        reader
            .task(&unpersisted)
            .expect("task reads")
            .expect("task exists")
            .state,
        TaskState::Paused,
        "a rejected retry leaves the stored state unchanged"
    );

    let persisted = second_service
        .add(request(
            &harness,
            "persisted.mp4",
            "https://fixture.invalid/watch?token=persisted",
            "https://fixture.invalid/watch",
            true,
        ))
        .expect("persisted video adds");
    wait_for_state(&reader, &persisted, TaskState::Downloading);
    second_service
        .pause(&persisted)
        .expect("pause preserves the persisted temp dir");
    wait_for_state(&reader, &persisted, TaskState::Paused);
    assert!(harness
        .destination
        .join(format!(".oneget-video-{persisted}"))
        .exists());
    second_service
        .shutdown()
        .expect("second service shuts down");

    let third_service =
        VideoService::start(harness.config(harness.store())).expect("third service starts");
    third_service
        .retry(&persisted)
        .expect("retry with a persisted full URL requeues the task");
    wait_for_state(&reader, &persisted, TaskState::Completed);

    let invocations = harness.args_for_task(&persisted);
    assert!(
        invocations.len() >= 2,
        "the paused and retried runs must both log their invocation, saw {}",
        invocations.len()
    );
    let resumed = invocations
        .iter()
        .find(|args| args.iter().any(|argument| argument == "--continue"))
        .expect("the retried run must reuse the preserved temp dir with --continue");
    assert!(
        resumed
            .iter()
            .any(|argument| argument == "https://fixture.invalid/watch?token=persisted"),
        "the retried run must pass the full persisted URL, not the minimized one: {resumed:?}"
    );
    assert!(
        !harness
            .destination
            .join(format!(".oneget-video-{persisted}"))
            .exists(),
        "completion removes the preserved temp dir"
    );

    third_service.shutdown().expect("third service shuts down");
}
