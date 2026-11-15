//! Behavior tests for the yt-dlp metadata probe and the deterministic quality model.
//!
//! Every probe test runs a generated mock program that re-executes the
//! deterministic `video_fixture` binary, so no public network and no real
//! yt-dlp installation is involved.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use oneget_lib::engine::http::ProxyConfig;
use oneget_lib::video::error::VideoError;
use oneget_lib::video::metadata::{VideoFormat, VideoMetadata};
use oneget_lib::video::probe::{probe_video, ProbeOptions};
use oneget_lib::video::quality::{build_quality_model, ChoiceKind};
use tempfile::TempDir;

const MIB: u64 = 1024 * 1024;
const MUXED_SIZE: u64 = 800 * MIB;
const UHD_SIZE: u64 = 1536 * MIB;
const VIDEO_1080_SIZE: u64 = 400 * MIB;
const AUDIO_PRIMARY_SIZE: u64 = 50 * MIB;
const AUDIO_SECONDARY_SIZE: u64 = 40 * MIB;

fn probe_options(program: PathBuf, timeout: Duration) -> ProbeOptions {
    ProbeOptions {
        yt_dlp: program,
        deno: None,
        cookies_file: None,
        proxy_url: None,
        proxy: None,
        timeout,
    }
}

/// A generated mock yt-dlp that ignores the fixed metadata flags and runs the
/// fixture binary in the requested mode.
struct MockProbe {
    _dir: TempDir,
    program: PathBuf,
}

impl MockProbe {
    fn emit(body: &str) -> Self {
        let dir = tempfile::tempdir().expect("probe temp dir");
        let payload = dir.path().join("probe.json");
        std::fs::write(&payload, body).expect("probe payload writes");
        let payload_path = payload.to_str().expect("payload path is UTF-8");
        let program = mock_program(dir.path(), &["emit", "--path", payload_path]);
        Self { _dir: dir, program }
    }

    fn mode(mode: &str) -> Self {
        let dir = tempfile::tempdir().expect("probe temp dir");
        let program = mock_program(dir.path(), &[mode]);
        Self { _dir: dir, program }
    }

    fn options(&self, timeout: Duration) -> ProbeOptions {
        probe_options(self.program.clone(), timeout)
    }
}

#[cfg(unix)]
fn mock_program(dir: &Path, arguments: &[&str]) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let mut command = shell_quote(env!("CARGO_BIN_EXE_video_fixture"));
    for argument in arguments {
        command.push(' ');
        command.push_str(&shell_quote(argument));
    }

    let path = dir.join("mock-yt-dlp");
    std::fs::write(&path, format!("#!/bin/sh\nexec {command}\n")).expect("mock script writes");
    let mut permissions = std::fs::metadata(&path)
        .expect("mock script metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("mock script becomes executable");
    path
}

#[cfg(unix)]
fn shell_quote(argument: &str) -> String {
    format!("'{}'", argument.replace('\'', "'\\''"))
}

#[cfg(windows)]
fn mock_program(dir: &Path, arguments: &[&str]) -> PathBuf {
    let mut command = format!("\"{}\"", env!("CARGO_BIN_EXE_video_fixture"));
    for argument in arguments {
        command.push(' ');
        command.push_str(&format!("\"{}\"", argument.replace('"', "\"\"")));
    }

    let path = dir.join("mock-yt-dlp.cmd");
    std::fs::write(&path, format!("@echo off\r\n{command}\r\n")).expect("mock script writes");
    path
}

fn expect_error(result: Result<VideoMetadata, VideoError>, context: &str) -> VideoError {
    match result {
        Ok(metadata) => panic!(
            "{context}: the probe unexpectedly succeeded with {} formats",
            metadata.formats.len()
        ),
        Err(error) => error,
    }
}

// A flat literal helper keeps each fixture format readable at its call site.
#[allow(clippy::too_many_arguments)]
fn video_format(
    id: &str,
    height: Option<u32>,
    fps: Option<f64>,
    bitrate: Option<u64>,
    filesize: Option<u64>,
    vcodec: Option<&str>,
    acodec: Option<&str>,
    ext: Option<&str>,
    has_drm: bool,
) -> VideoFormat {
    VideoFormat {
        format_id: id.to_string(),
        height,
        fps,
        bitrate,
        filesize,
        vcodec: vcodec.map(str::to_string),
        acodec: acodec.map(str::to_string),
        ext: ext.map(str::to_string),
        has_drm,
    }
}

fn metadata_with(formats: Vec<VideoFormat>) -> VideoMetadata {
    VideoMetadata {
        title: "Fixture title".to_string(),
        duration_secs: Some(60.0),
        uploader: Some("Fixture uploader".to_string()),
        is_live: false,
        formats,
    }
}

fn format_ids(model: &oneget_lib::video::quality::QualityModel, video: bool) -> Vec<&str> {
    let choices = if video { &model.video } else { &model.audio };
    choices.iter().map(|choice| choice.id.as_str()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_parses_vod_metadata_and_formats() {
    let payload = r#"{
        "id": "fixture-vod",
        "title": "Example Clip",
        "duration": 754.25,
        "uploader": "Creator Name",
        "is_live": false,
        "formats": [
            {
                "format_id": "137",
                "height": 1080,
                "fps": 30.0,
                "filesize": 812345678,
                "vcodec": "avc1.640028",
                "acodec": "none",
                "ext": "mp4"
            },
            {
                "format_id": "251",
                "filesize": 52428800,
                "vcodec": "none",
                "acodec": "opus",
                "ext": "webm"
            },
            {
                "format_id": "18",
                "height": 360,
                "fps": 30.0,
                "filesize": 7340032,
                "vcodec": "avc1.42001E",
                "acodec": "mp4a.40.2",
                "ext": "mp4"
            },
            {
                "format_id": "999",
                "height": 2160,
                "filesize": 2147483648,
                "vcodec": "avc1.640033",
                "acodec": "mp4a.40.2",
                "ext": "mp4",
                "has_drm": true
            }
        ]
    }"#;

    let mock = MockProbe::emit(payload);
    let metadata = match probe_video(
        "https://example.com/watch?v=fixture",
        &mock.options(Duration::from_secs(10)),
    )
    .await
    {
        Ok(metadata) => metadata,
        Err(error) => panic!("a valid VOD payload must parse: {error}"),
    };

    assert_eq!(metadata.title, "Example Clip");
    assert_eq!(metadata.duration_secs, Some(754.25));
    assert_eq!(metadata.uploader.as_deref(), Some("Creator Name"));
    assert!(!metadata.is_live, "a VOD result must not be marked live");
    assert_eq!(metadata.formats.len(), 4);

    let video = metadata
        .formats
        .iter()
        .find(|format| format.format_id == "137")
        .expect("format 137 must be mapped");
    assert_eq!(video.height, Some(1080));
    assert_eq!(video.fps, Some(30.0));
    assert_eq!(video.filesize, Some(812_345_678));
    assert_eq!(video.vcodec.as_deref(), Some("avc1.640028"));
    assert_eq!(video.acodec.as_deref(), Some("none"));
    assert_eq!(video.ext.as_deref(), Some("mp4"));
    assert!(!video.has_drm);

    let audio = metadata
        .formats
        .iter()
        .find(|format| format.format_id == "251")
        .expect("format 251 must be mapped");
    assert_eq!(audio.height, None);
    assert_eq!(audio.filesize, Some(52_428_800));
    assert_eq!(audio.vcodec.as_deref(), Some("none"));
    assert_eq!(audio.acodec.as_deref(), Some("opus"));
    assert_eq!(audio.ext.as_deref(), Some("webm"));
    assert!(!audio.has_drm);

    let muxed = metadata
        .formats
        .iter()
        .find(|format| format.format_id == "18")
        .expect("format 18 must be mapped");
    assert_eq!(muxed.height, Some(360));
    assert_eq!(muxed.vcodec.as_deref(), Some("avc1.42001E"));
    assert_eq!(muxed.acodec.as_deref(), Some("mp4a.40.2"));
    assert_eq!(muxed.ext.as_deref(), Some("mp4"));

    let drm = metadata
        .formats
        .iter()
        .find(|format| format.format_id == "999")
        .expect("format 999 must be mapped");
    assert!(drm.has_drm, "the per-format DRM marker must be preserved");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_rejects_live_drm_and_malformed_results() {
    let cases = [
        (
            "live stream",
            r#"{
                "title": "Live stream",
                "is_live": true,
                "formats": [{"format_id": "1", "height": 1080, "vcodec": "avc1", "ext": "mp4"}]
            }"#,
            "VIDEO_LIVE_UNSUPPORTED",
        ),
        (
            "upcoming stream",
            r#"{
                "title": "Upcoming stream",
                "is_live": false,
                "live_status": "is_upcoming",
                "formats": [{"format_id": "1", "height": 1080, "vcodec": "avc1", "ext": "mp4"}]
            }"#,
            "VIDEO_LIVE_UNSUPPORTED",
        ),
        (
            "top-level DRM",
            r#"{
                "title": "DRM video",
                "has_drm": true,
                "formats": [{"format_id": "1", "height": 1080, "vcodec": "avc1", "ext": "mp4"}]
            }"#,
            "VIDEO_DRM_UNSUPPORTED",
        ),
        (
            "DRM-only formats",
            r#"{
                "title": "DRM formats",
                "formats": [
                    {"format_id": "1", "height": 1080, "vcodec": "avc1", "ext": "mp4", "has_drm": true},
                    {"format_id": "2", "vcodec": "none", "acodec": "mp4a", "ext": "m4a", "has_drm": true}
                ]
            }"#,
            "VIDEO_DRM_UNSUPPORTED",
        ),
        (
            "malformed JSON",
            r#"{"title": "Broken", "formats": ["#,
            "VIDEO_PROBE_INVALID",
        ),
        (
            "missing title",
            r#"{
                "duration": 12.5,
                "formats": [{"format_id": "1", "height": 1080, "vcodec": "avc1", "ext": "mp4"}]
            }"#,
            "VIDEO_PROBE_INVALID",
        ),
        (
            "missing formats",
            r#"{"title": "No formats", "duration": 12.5}"#,
            "VIDEO_PROBE_INVALID",
        ),
    ];

    for (label, payload, expected) in cases {
        let mock = MockProbe::emit(payload);
        let error = expect_error(
            probe_video(
                "https://example.com/watch?v=fixture",
                &mock.options(Duration::from_secs(10)),
            )
            .await,
            label,
        );
        assert_eq!(error.code(), expected, "{label} must report {expected}");
        assert!(
            !error.message().is_empty(),
            "{label} must carry a user-safe message"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_rejects_unsupported_urls_before_spawning() {
    let dir = tempfile::tempdir().expect("probe temp dir");
    let missing_program = dir.path().join("definitely-not-yt-dlp");
    let options = probe_options(missing_program, Duration::from_secs(5));

    for url in ["", "file:///x"] {
        let error = expect_error(
            probe_video(url, &options).await,
            "an unsupported URL must be rejected before spawning",
        );
        assert_eq!(
            error.code(),
            "VIDEO_URL_UNSUPPORTED",
            "URL {url:?} must be rejected as unsupported"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_timeout_and_output_limit_are_typed() {
    let hang = MockProbe::mode("hang");
    let started = Instant::now();
    let error = expect_error(
        probe_video(
            "https://example.com/watch?v=fixture",
            &hang.options(Duration::from_millis(150)),
        )
        .await,
        "a hanging probe must time out",
    );
    assert_eq!(error.code(), "VIDEO_TIMEOUT");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the timeout must return promptly instead of waiting for the child: {:?}",
        started.elapsed()
    );

    let flood = MockProbe::mode("flood");
    let error = expect_error(
        probe_video(
            "https://example.com/watch?v=fixture",
            &flood.options(Duration::from_secs(10)),
        )
        .await,
        "a probe above the output cap must report VIDEO_OUTPUT_LIMIT",
    );
    assert_eq!(error.code(), "VIDEO_OUTPUT_LIMIT");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quality_model_orders_estimates_and_marks_mp4() {
    let metadata = metadata_with(vec![
        video_format(
            "muxed-1080",
            Some(1080),
            Some(60.0),
            Some(5_000_000),
            Some(MUXED_SIZE),
            Some("avc1.640028"),
            Some("mp4a.40.2"),
            Some("mp4"),
            false,
        ),
        video_format(
            "v2160",
            Some(2160),
            Some(30.0),
            Some(9_000_000),
            Some(UHD_SIZE),
            Some("vp9"),
            None,
            Some("webm"),
            false,
        ),
        video_format(
            "v1080",
            Some(1080),
            Some(30.0),
            Some(3_000_000),
            Some(VIDEO_1080_SIZE),
            Some("avc1.640028"),
            None,
            Some("mp4"),
            false,
        ),
        video_format(
            "a-m4a",
            None,
            None,
            Some(192_000),
            Some(AUDIO_PRIMARY_SIZE),
            Some("none"),
            Some("mp4a.40.2"),
            Some("m4a"),
            false,
        ),
        video_format(
            "a-opus",
            None,
            None,
            Some(160_000),
            Some(AUDIO_SECONDARY_SIZE),
            Some("none"),
            Some("opus"),
            Some("webm"),
            false,
        ),
        video_format(
            "drm-2160",
            Some(2160),
            Some(60.0),
            Some(12_000_000),
            Some(2048 * MIB),
            Some("avc1.640033"),
            Some("mp4a.40.2"),
            Some("mp4"),
            true,
        ),
        video_format(
            "v720",
            Some(720),
            Some(30.0),
            Some(2_000_000),
            None,
            Some("avc1.4d401f"),
            None,
            Some("mp4"),
            false,
        ),
    ]);

    let model = build_quality_model(&metadata);

    assert_eq!(
        format_ids(&model, true),
        ["v2160", "muxed-1080", "v1080", "v720"],
        "video choices must be ordered by height, frame rate, bitrate, and size"
    );
    assert!(
        model
            .video
            .iter()
            .all(|choice| matches!(choice.kind, ChoiceKind::Video)),
        "every video choice must be marked as video"
    );
    assert!(
        model.video.iter().all(|choice| choice.id != "drm-2160"),
        "a DRM format must never become a video choice"
    );

    assert_eq!(
        format_ids(&model, false),
        ["a-m4a", "a-opus"],
        "audio-only choices must be separate and ordered by bitrate"
    );
    assert!(
        model
            .audio
            .iter()
            .all(|choice| matches!(choice.kind, ChoiceKind::AudioOnly)),
        "every audio choice must be marked audio-only"
    );

    assert_eq!(
        model.video[0].size_bytes,
        Some(UHD_SIZE + AUDIO_PRIMARY_SIZE),
        "a video-only choice must add the best known audio size"
    );
    assert_eq!(
        model.video[1].size_bytes,
        Some(MUXED_SIZE),
        "a muxed choice must keep its reported size"
    );
    assert_eq!(
        model.video[2].size_bytes,
        Some(VIDEO_1080_SIZE + AUDIO_PRIMARY_SIZE),
        "a video-only choice must add the best known audio size"
    );
    assert_eq!(
        model.video[3].size_bytes, None,
        "an unknown size must stay unknown"
    );

    assert!(
        !model.video[0].mp4_compatible,
        "a webm choice must not be marked MP4-compatible"
    );
    assert!(
        model.video[1].mp4_compatible && model.video[2].mp4_compatible,
        "mp4 choices must be marked MP4-compatible"
    );
    assert!(
        model.video[3].mp4_compatible,
        "an mp4 choice must be marked MP4-compatible"
    );

    let default_index = model
        .default_index
        .expect("a fallback video choice must exist when video formats exist");
    assert_eq!(
        model.video[default_index].id, "muxed-1080",
        "the default must be the highest MP4-compatible video choice"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quality_model_default_falls_back_and_handles_audio_only() {
    let non_mp4 = metadata_with(vec![
        video_format(
            "webm-1080",
            Some(1080),
            Some(30.0),
            Some(3_000_000),
            Some(300 * MIB),
            Some("vp9"),
            None,
            Some("webm"),
            false,
        ),
        video_format(
            "webm-720",
            Some(720),
            Some(30.0),
            Some(1_500_000),
            Some(150 * MIB),
            Some("vp9"),
            None,
            Some("webm"),
            false,
        ),
    ]);
    let non_mp4_model = build_quality_model(&non_mp4);

    assert!(
        non_mp4_model
            .video
            .iter()
            .all(|choice| !choice.mp4_compatible),
        "webm choices must not be marked MP4-compatible"
    );
    assert_eq!(
        non_mp4_model.default_index,
        Some(0),
        "without an MP4-compatible choice the default must fall back to the highest video"
    );
    assert_eq!(non_mp4_model.video[0].id, "webm-1080");

    let audio_only = metadata_with(vec![video_format(
        "audio-1",
        None,
        None,
        Some(128_000),
        Some(20 * MIB),
        Some("none"),
        Some("mp4a.40.2"),
        Some("m4a"),
        false,
    )]);
    let audio_model = build_quality_model(&audio_only);

    assert!(
        audio_model.video.is_empty(),
        "audio-only metadata must produce no video choices"
    );
    assert_eq!(
        audio_model.default_index, None,
        "audio-only metadata must not choose a video default"
    );
    assert_eq!(format_ids(&audio_model, false), ["audio-1"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quality_model_selector_requests_merge_for_video_only() {
    let video_only = metadata_with(vec![video_format(
        "137",
        Some(1080),
        Some(30.0),
        Some(4_000_000),
        Some(500 * MIB),
        Some("avc1.640028"),
        None,
        Some("mp4"),
        false,
    )]);
    let video_only_model = build_quality_model(&video_only);
    let selector = &video_only_model.video[0].format_selector;
    assert!(
        selector.starts_with("137"),
        "a video-only selector must start with its format id, got {selector}"
    );
    assert!(
        selector.contains("+bestaudio"),
        "a video-only selector must request the best audio, got {selector}"
    );

    let muxed = metadata_with(vec![video_format(
        "18",
        Some(360),
        Some(30.0),
        Some(1_000_000),
        Some(50 * MIB),
        Some("avc1.42001E"),
        Some("mp4a.40.2"),
        Some("mp4"),
        false,
    )]);
    let muxed_model = build_quality_model(&muxed);
    assert_eq!(
        muxed_model.video[0].format_selector, "18",
        "a muxed selector must use only its format id"
    );
    assert!(
        !muxed_model.video[0].format_selector.contains('+'),
        "a muxed selector must not request a separate audio format"
    );

    let audio = metadata_with(vec![video_format(
        "140",
        None,
        None,
        Some(128_000),
        Some(10 * MIB),
        Some("none"),
        Some("mp4a.40.2"),
        Some("m4a"),
        false,
    )]);
    let audio_model = build_quality_model(&audio);
    assert_eq!(
        audio_model.audio[0].format_selector, "140",
        "an audio-only selector must use only its format id"
    );
    assert!(
        !audio_model.audio[0].format_selector.contains('+'),
        "an audio-only selector must not request a merge"
    );
}

// ---------------------------------------------------------------------------
// Proxy delivery to the yt-dlp child: environment variables only, never argv.
// ---------------------------------------------------------------------------

const GLOBAL_PROXY_URL: &str = "http://127.0.0.1:18923";
const GLOBAL_PROXY_AUTHORITY: &str = "127.0.0.1:18923";
const GLOBAL_PROXY_USER: &str = "oneget-probe-proxy-user";
const GLOBAL_PROXY_PASSWORD: &str = "oneget-probe-proxy-password-secret";
const PER_VIDEO_PROXY_URL: &str = "http://127.0.0.1:18924";
const PER_VIDEO_PROXY_AUTHORITY: &str = "127.0.0.1:18924";

const RECORDED_PAYLOAD: &str = r#"{
    "id": "fixture-proxy",
    "title": "Proxy Fixture",
    "duration": 42.0,
    "uploader": "Fixture",
    "is_live": false,
    "formats": [
        {
            "format_id": "137",
            "height": 1080,
            "filesize": 1024,
            "vcodec": "avc1.640028",
            "ext": "mp4"
        }
    ]
}"#;

/// A generated mock yt-dlp that records its argv and environment to files
/// under a temp dir, then executes the deterministic fixture in emit mode so
/// the probe succeeds.
struct RecordingProbe {
    _dir: TempDir,
    program: PathBuf,
    argv_path: PathBuf,
    env_path: PathBuf,
}

impl RecordingProbe {
    fn emit(body: &str) -> Self {
        let dir = tempfile::tempdir().expect("probe temp dir");
        let payload = dir.path().join("probe.json");
        std::fs::write(&payload, body).expect("probe payload writes");
        let argv_path = dir.path().join("argv.txt");
        let env_path = dir.path().join("env.txt");
        let program = recording_program(dir.path(), &argv_path, &env_path, &payload);
        Self {
            _dir: dir,
            program,
            argv_path,
            env_path,
        }
    }

    fn options(&self, timeout: Duration) -> ProbeOptions {
        probe_options(self.program.clone(), timeout)
    }

    fn recorded_argv(&self) -> String {
        std::fs::read_to_string(&self.argv_path).expect("argv record must exist")
    }

    fn recorded_env(&self) -> String {
        std::fs::read_to_string(&self.env_path).expect("environment record must exist")
    }
}

#[cfg(unix)]
fn recording_program(dir: &Path, argv_path: &Path, env_path: &Path, payload: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let fixture = shell_quote(env!("CARGO_BIN_EXE_video_fixture"));
    let argv = shell_quote(argv_path.to_str().expect("argv path is UTF-8"));
    let env = shell_quote(env_path.to_str().expect("env path is UTF-8"));
    let payload = shell_quote(payload.to_str().expect("payload path is UTF-8"));
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > {argv}\nenv > {env}\nexec {fixture} emit --path {payload}\n"
    );
    let path = dir.join("mock-yt-dlp-record");
    std::fs::write(&path, script).expect("mock script writes");
    let mut permissions = std::fs::metadata(&path)
        .expect("mock script metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&path, permissions).expect("mock script becomes executable");
    path
}

#[cfg(windows)]
fn recording_program(dir: &Path, argv_path: &Path, env_path: &Path, payload: &Path) -> PathBuf {
    let quote = |argument: &str| format!("\"{}\"", argument.replace('"', "\"\""));
    let fixture = quote(env!("CARGO_BIN_EXE_video_fixture"));
    let argv = quote(argv_path.to_str().expect("argv path is UTF-8"));
    let env = quote(env_path.to_str().expect("env path is UTF-8"));
    let payload = quote(payload.to_str().expect("payload path is UTF-8"));
    let script = format!(
        "@echo off\r\n(for %%A in (%*) do @echo %%A)>{argv}\r\nset>{env}\r\n{fixture} emit --path {payload}\r\n"
    );
    let path = dir.join("mock-yt-dlp-record.cmd");
    std::fs::write(&path, script).expect("mock script writes");
    path
}

fn proxy_env_lines<'a>(env: &'a str, variable: &str) -> Vec<&'a str> {
    env.lines()
        .filter(|line| line.starts_with(&format!("{variable}=")))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_global_proxy_credentials_arrive_via_environment_only() {
    let mock = RecordingProbe::emit(RECORDED_PAYLOAD);
    let mut options = mock.options(Duration::from_secs(10));
    options.proxy = Some(ProxyConfig {
        url: GLOBAL_PROXY_URL.to_string(),
        username: Some(GLOBAL_PROXY_USER.to_string()),
        password: Some(GLOBAL_PROXY_PASSWORD.to_string()),
    });

    let metadata = probe_video("https://example.com/watch?v=fixture", &options)
        .await
        .expect("the probe must succeed while recording the proxy environment");
    assert_eq!(metadata.title, "Proxy Fixture");

    let env = mock.recorded_env();
    for variable in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
        let lines = proxy_env_lines(&env, variable);
        assert_eq!(
            lines.len(),
            1,
            "{variable} must be set exactly once for the child"
        );
        let line = lines[0];
        assert!(
            line.contains(GLOBAL_PROXY_AUTHORITY),
            "{variable} must carry the proxy authority: {line}"
        );
        assert!(
            line.contains(GLOBAL_PROXY_USER),
            "{variable} must carry the proxy username"
        );
        assert!(
            line.contains(GLOBAL_PROXY_PASSWORD),
            "{variable} must carry the proxy password"
        );
    }

    let argv = mock.recorded_argv();
    assert!(
        !argv.contains(GLOBAL_PROXY_USER),
        "the child argv must not carry the proxy username"
    );
    assert!(
        !argv.contains(GLOBAL_PROXY_PASSWORD),
        "the child argv must not carry the proxy password"
    );
    assert!(
        !argv.contains(GLOBAL_PROXY_AUTHORITY),
        "the child argv must not carry the proxy authority"
    );
    assert!(
        !argv.lines().any(|argument| argument == "--proxy"),
        "the child argv must not carry a --proxy argument"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_per_video_proxy_url_overrides_the_global_proxy() {
    let mock = RecordingProbe::emit(RECORDED_PAYLOAD);
    let mut options = mock.options(Duration::from_secs(10));
    options.proxy = Some(ProxyConfig {
        url: GLOBAL_PROXY_URL.to_string(),
        username: Some(GLOBAL_PROXY_USER.to_string()),
        password: Some(GLOBAL_PROXY_PASSWORD.to_string()),
    });
    options.proxy_url = Some(PER_VIDEO_PROXY_URL.to_string());

    let metadata = probe_video("https://example.com/watch?v=fixture", &options)
        .await
        .expect("the probe must succeed with the per-video override");
    assert_eq!(metadata.title, "Proxy Fixture");

    let env = mock.recorded_env();
    for variable in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
        let lines = proxy_env_lines(&env, variable);
        assert_eq!(
            lines.len(),
            1,
            "{variable} must be set exactly once for the child"
        );
        let line = lines[0];
        assert!(
            line.contains(PER_VIDEO_PROXY_AUTHORITY),
            "{variable} must carry the per-video proxy authority: {line}"
        );
        assert!(
            !line.contains(GLOBAL_PROXY_AUTHORITY),
            "{variable} must not carry the global proxy authority: {line}"
        );
    }
    assert!(
        !env.contains(GLOBAL_PROXY_USER) && !env.contains(GLOBAL_PROXY_PASSWORD),
        "the global proxy credentials must not reach the child when a per-video proxy is set"
    );

    let argv = mock.recorded_argv();
    assert!(!argv.contains(GLOBAL_PROXY_USER));
    assert!(!argv.contains(GLOBAL_PROXY_PASSWORD));
    assert!(!argv.contains(GLOBAL_PROXY_AUTHORITY));
    assert!(!argv.contains(PER_VIDEO_PROXY_AUTHORITY));
    assert!(!argv.lines().any(|argument| argument == "--proxy"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_without_proxy_injects_no_proxy_variables() {
    let mock = RecordingProbe::emit(RECORDED_PAYLOAD);
    let options = mock.options(Duration::from_secs(10));

    let metadata = probe_video("https://example.com/watch?v=fixture", &options)
        .await
        .expect("the probe must succeed without a proxy");
    assert_eq!(metadata.title, "Proxy Fixture");

    let env = mock.recorded_env();
    for variable in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
        assert!(
            proxy_env_lines(&env, variable).is_empty(),
            "{variable} must not be injected when no proxy is configured: {env}"
        );
    }

    let argv = mock.recorded_argv();
    assert!(!argv.lines().any(|argument| argument == "--proxy"));
}
