//! Behavior tests for HLS/DASH delegation, per-download cookies and proxy
//! forwarding, audio-only extraction, and typed merge failures.
//!
//! Every download runs a copy of the deterministic `video_fixture` binary
//! installed as the fake `yt-dlp`; the copy's file name selects the scenario.
//! The loopback catalog server counts requests, so a request count of zero
//! proves the application never fetched the manifest itself.

mod support;

use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use oneget_lib::engine::state::TaskState;
use oneget_lib::kernel::catalog::Target;
use oneget_lib::kernel::store::{ComponentInstall, ComponentStore, InstalledFile};
use oneget_lib::store::{Database, Task};
use oneget_lib::video::process::TimeoutPolicy;
use oneget_lib::video::service::{StartVideoRequest, VideoService, VideoServiceConfig};
use serde_json::json;
use sha2::{Digest, Sha256};
use support::kernel_server::{KernelServer, KernelServerConfig};
use tempfile::TempDir;

const WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

fn request(harness: &Harness, url: &str, filename: &str) -> StartVideoRequest {
    StartVideoRequest {
        url: url.to_string(),
        minimized_url: url.to_string(),
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
        full_url_persisted: true,
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
/// file name; `ffmpeg` and `ffprobe` are fixture copies too so the resolver
/// verifies a complete install.
struct Harness {
    dir: TempDir,
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
            dir,
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

    fn ffmpeg_dir(&self) -> PathBuf {
        self.kernel_root.join("ffmpeg/2-test")
    }

    fn install_download(&self, behavior: &str) {
        let yt_dir = self.kernel_root.join("yt-dlp/1-test");
        let ffmpeg_dir = self.ffmpeg_dir();
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

    fn args_log_text(&self) -> String {
        fs::read_to_string(self.destination.join(".fixture-args.jsonl")).unwrap_or_default()
    }

    fn task_temp(&self, id: &str) -> PathBuf {
        self.destination.join(format!(".oneget-video-{id}"))
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

fn assert_pair(args: &[String], flag: &str, value: &str) {
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value),
        "argv must carry `{flag} {value}`: {args:?}"
    );
}

#[test]
fn hls_url_is_delegated_without_manifest_fetch() {
    let harness = Harness::new();
    harness.install_download("yt-dlp");
    let server = KernelServer::start(
        KernelServerConfig::new(b"catalog".to_vec()).artifact("stream.m3u8", b"#EXTM3U\n".to_vec()),
    );
    let url = server.artifact_url("stream.m3u8");
    let service = VideoService::start(harness.config(harness.store(), TimeoutPolicy::default()))
        .expect("video service starts");
    let id = service
        .add(request(&harness, &url, "stream.mp4"))
        .expect("HLS video task adds");
    let reader = harness.store();

    let task = wait_for_state(&reader, &id, TaskState::Completed);
    assert_eq!(
        server.request_count(),
        0,
        "the application must never fetch the manifest itself"
    );

    let invocations = harness.args_for_task(&id);
    assert_eq!(invocations.len(), 1, "exactly one yt-dlp invocation");
    let args = &invocations[0];
    assert!(
        args.iter().any(|argument| argument == &url),
        "yt-dlp must receive the exact HLS URL: {args:?}"
    );
    let recorded_ffmpeg = args
        .windows(2)
        .find(|pair| pair[0] == "--ffmpeg-location")
        .map(|pair| PathBuf::from(&pair[1]))
        .expect("yt-dlp must receive the active ffmpeg directory");
    assert_eq!(
        fs::canonicalize(&recorded_ffmpeg).expect("recorded ffmpeg directory resolves"),
        fs::canonicalize(harness.ffmpeg_dir()).expect("installed ffmpeg directory resolves"),
        "the ffmpeg location must be the active component directory: {args:?}"
    );
    assert_pair(args, "--concurrent-fragments", "4");

    let outputs = output_files(&harness.destination);
    assert_eq!(outputs.len(), 1, "exactly one final file must exist");
    assert_eq!(
        outputs[0].extension().and_then(|value| value.to_str()),
        Some("mp4"),
        "a video choice produces an MP4"
    );
    let stored_hash = task
        .actual_sha256
        .clone()
        .expect("completion stores the hash");
    assert_eq!(
        stored_hash,
        sha256_hex(&fs::read(&outputs[0]).expect("output reads")),
        "the stored hash must match the final file"
    );
    assert!(
        !harness.task_temp(&id).exists(),
        "completion removes the task temp directory"
    );

    service.shutdown().expect("shutdown succeeds");
}

#[test]
fn dash_and_audio_only_extract_m4a() {
    let harness = Harness::new();
    harness.install_download("yt-dlp");
    let service = VideoService::start(harness.config(harness.store(), TimeoutPolicy::default()))
        .expect("video service starts");
    let mut start = request(
        &harness,
        "https://fixture.invalid/stream.mpd",
        "audio-only.m4a",
    );
    start.audio_only = true;
    let id = service.add(start).expect("DASH audio-only task adds");
    let reader = harness.store();

    let task = wait_for_state(&reader, &id, TaskState::Completed);
    let invocations = harness.args_for_task(&id);
    assert_eq!(invocations.len(), 1, "exactly one yt-dlp invocation");
    let args = &invocations[0];
    assert!(
        args.iter()
            .any(|argument| argument == "https://fixture.invalid/stream.mpd"),
        "yt-dlp must receive the exact DASH URL: {args:?}"
    );
    assert!(
        args.iter().any(|argument| argument == "--extract-audio")
            && args
                .windows(2)
                .any(|pair| pair[0] == "--audio-format" && pair[1] == "m4a"),
        "audio-only runs the extraction flags: {args:?}"
    );
    assert!(
        args.iter().any(|argument| argument == "--ffmpeg-location"),
        "audio-only still uses the active ffmpeg directory: {args:?}"
    );

    let outputs = output_files(&harness.destination);
    assert_eq!(outputs.len(), 1, "exactly one final file must exist");
    assert_eq!(
        outputs[0].extension().and_then(|value| value.to_str()),
        Some("m4a"),
        "audio-only produces an m4a output"
    );
    let stored_hash = task
        .actual_sha256
        .clone()
        .expect("completion stores the hash");
    assert_eq!(
        stored_hash,
        sha256_hex(&fs::read(&outputs[0]).expect("output reads")),
        "completion stores the m4a hash"
    );
    assert!(
        outputs
            .iter()
            .all(|path| path.extension().and_then(|value| value.to_str()) != Some("mp4")),
        "no mp4 may remain after audio-only extraction"
    );
    assert!(
        !harness.task_temp(&id).exists(),
        "completion removes the task temp directory"
    );

    service.shutdown().expect("shutdown succeeds");
}

#[test]
fn cookies_and_proxy_are_forwarded_without_values_in_storage() {
    let harness = Harness::new();
    harness.install_download("yt-dlp");
    let secret = "session-token-7b1d2c";
    let cookies_path = harness.dir.path().join("cookies.txt");
    fs::write(
        &cookies_path,
        format!("# Netscape HTTP Cookie File\n.example\tTRUE\t/\tFALSE\t0\tcookie\t{secret}\n"),
    )
    .expect("cookies file writes");
    let proxy = "http://127.0.0.1:9";
    let cookies_path_text = cookies_path.to_string_lossy().into_owned();

    let service = VideoService::start(harness.config(harness.store(), TimeoutPolicy::default()))
        .expect("video service starts");
    let mut start = request(
        &harness,
        "https://fixture.invalid/stream.m3u8",
        "cookies.mp4",
    );
    start.cookies_file = Some(cookies_path.clone());
    start.proxy_url = Some(proxy.to_string());
    let id = service.add(start).expect("cookies/proxy task adds");
    let reader = harness.store();
    let task = wait_for_state(&reader, &id, TaskState::Completed);

    let invocations = harness.args_for_task(&id);
    assert_eq!(invocations.len(), 1, "exactly one yt-dlp invocation");
    let args = &invocations[0];
    assert_pair(args, "--cookies", &cookies_path_text);
    assert_pair(args, "--proxy", proxy);
    assert!(
        !harness.args_log_text().contains(secret),
        "the argv log must carry only the cookies path, never its content"
    );

    let video = task.video.as_ref().expect("the video record is persisted");
    assert_eq!(
        video.get("cookiesFile").and_then(|value| value.as_str()),
        Some(cookies_path_text.as_str()),
        "the video record stores the cookies path"
    );
    assert_eq!(
        video.get("proxyUrl").and_then(|value| value.as_str()),
        Some(proxy),
        "the video record stores the credential-free proxy URL"
    );

    let conn = rusqlite::Connection::open(&harness.store_path).expect("raw store opens");
    let (url, video_json): (String, Option<String>) = conn
        .query_row(
            "SELECT url, video_json FROM tasks WHERE id = ?1",
            [&id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("persisted task row reads");
    let persisted = format!("{url}{}", video_json.unwrap_or_default());
    assert!(
        !persisted.contains(secret),
        "the persisted task row must never contain cookie content"
    );
    assert!(
        persisted.contains(&cookies_path_text),
        "the persisted row keeps only the cookies path"
    );
    assert!(
        !persisted.contains("user:password@"),
        "no proxy credentials may be persisted"
    );

    service.shutdown().expect("shutdown succeeds");
}

#[test]
fn merge_failure_is_typed_and_leaves_no_final_file() {
    let harness = Harness::new();
    harness.install_download("yt-dlp-merge-fail");
    let service = VideoService::start(harness.config(harness.store(), TimeoutPolicy::default()))
        .expect("video service starts");
    let id = service
        .add(request(
            &harness,
            "https://fixture.invalid/stream.m3u8",
            "merge.mp4",
        ))
        .expect("merge-failure task adds");
    let reader = harness.store();

    let failed = wait_for_state(&reader, &id, TaskState::Failed);
    assert_eq!(
        failed.error_code.as_deref(),
        Some("VIDEO_MERGE_FAILED"),
        "a failed merge must surface the typed merge code"
    );
    assert!(
        output_files(&harness.destination).is_empty(),
        "a failed merge must leave no final file"
    );
    assert!(
        harness.task_temp(&id).exists(),
        "a failed merge preserves the task temp directory"
    );

    service.shutdown().expect("shutdown succeeds");
}
