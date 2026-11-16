//! Behavior tests for cooperative dynamic splitting and the safe Range
//! fallback. Every fixture is local and deterministic.
//!
//! Exact engine configuration boundary exercised by the network-settings tests:
//!
//! ```text
//! impl Engine {
//!     pub fn set_global_rate_limit(&self, bytes_per_second: Option<u64>);
//!     pub fn set_task_rate_limit(&self, task_id: &str, bytes_per_second: Option<u64>);
//!     pub fn set_proxy(&self, proxy: Option<ProxyConfig>) -> Result<(), EngineError>;
//!     pub fn set_cookie_file(&self, path: Option<std::path::PathBuf>);
//!     pub fn set_logger(&self, logger: std::sync::Arc<oneget_lib::logging::Logger>);
//! }
//!
//! pub struct ProxyConfig {
//!     pub url: String,
//!     pub username: Option<String>,
//!     pub password: Option<String>,
//! }
//! ```
//!
//! `ProxyConfig` lives in `oneget_lib::engine::http`. `add_task` rejects a
//! forbidden or newline-bearing header with `EngineError::code()` equal to
//! `INVALID_HEADER` before it creates a task. `set_proxy` rejects a URL with
//! embedded userinfo with `INVALID_PROXY_URL`. A proxy authentication failure
//! surfaces as task error code `PROXY_AUTH_REQUIRED`.

mod support;

#[path = "support/cookies.rs"]
mod cookies;
#[path = "support/proxy_server.rs"]
mod proxy_server;

#[path = "support/hf_server.rs"]
mod hf_server;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cookies::{
    cookie_names, cookie_value, future_expiry, past_expiry, write_cookie_file, NetscapeCookie,
};
use hf_server::{HfFixtureConfig, HfFixtureServer, ResolverBehavior};
use oneget_lib::commands::downloads::DownloadSummary;
use oneget_lib::engine::cookies::{BrowserCookie, CookieSource};
use oneget_lib::engine::http::{build_client_with_timeouts, ProxyConfig};
use oneget_lib::engine::retry::classify_reqwest_error;
use oneget_lib::engine::scheduler::{Engine, EngineOptions, NetworkOptions};
use oneget_lib::engine::service::{AddDownload, EngineService};
use oneget_lib::engine::state::TaskState;
use oneget_lib::engine::worker::DownloadSpec;
use oneget_lib::logging::{LogConfig, LogLevel, Logger};
use oneget_lib::store::{Database, Segment};
use proxy_server::{ProxyCredentials, ProxyServer};
use sha2::{Digest, Sha256};
use support::fixtures::pattern_bytes;
use support::range_server::{FixtureConfig, RangeMode, RangeServer};
use tempfile::tempdir;
use tokio::time::{sleep, timeout, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(15);
const WAIT_TIMEOUT: Duration = Duration::from_secs(20);
const MIB: u64 = 1024 * 1024;
const RATE_STARTUP: Duration = Duration::from_millis(1_500);
const RATE_FINALIZE: Duration = Duration::from_millis(1_500);
const SAFE_HEADER_NAME: &str = "X-Custom-Trace";
const SAFE_HEADER_VALUE: &str = "oneget-safe-trace-value";
const FORBIDDEN_HEADERS: &[&str] = &[
    "Host",
    "Authorization",
    "Cookie",
    "Range",
    "Proxy-Authorization",
    "Content-Length",
    "Connection",
    "Transfer-Encoding",
    "Upgrade",
    "TE",
    "Trailer",
    "Set-Cookie",
];

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn non_periodic_bytes(len: usize) -> Vec<u8> {
    // A 251-byte cycle that does not align with any 1 MiB range boundary, so a
    // body written at the wrong offset cannot match the expected bytes.
    (0..len).map(|i| ((i * 37 + 11) % 251 + 1) as u8).collect()
}

fn open_store(dir: &tempfile::TempDir) -> Database {
    Database::open(dir.path().join("tasks.db")).expect("open store")
}

fn make_spec(
    url: String,
    filename: &str,
    connections: u32,
    expected_sha256: Option<String>,
) -> DownloadSpec {
    DownloadSpec {
        url,
        filename: filename.to_string(),
        connections,
        allow_html: false,
        expected_sha256,
        extra_headers: Vec::new(),
        destination: None,
    }
}

fn part_path(dest_dir: &Path, filename: &str) -> PathBuf {
    dest_dir.join(format!("{filename}.oneget.part"))
}

async fn wait_for_terminal(store: &Database, id: &str, deadline: Duration) -> TaskState {
    let start = Instant::now();
    loop {
        let task = store.task(id).expect("load task").expect("task exists");
        if matches!(
            task.state,
            TaskState::Completed | TaskState::Failed | TaskState::Cancelled | TaskState::Paused
        ) {
            return task.state;
        }
        if start.elapsed() > deadline {
            panic!(
                "timed out waiting for task {id}, current {:?}, downloaded={}",
                task.state, task.downloaded_bytes
            );
        }
        sleep(POLL_INTERVAL).await;
    }
}

/// Assert that one persisted snapshot of segment rows is non-overlapping,
/// gap-free, unique, and covers exactly `[0, total)`.
fn assert_segment_ranges(segments: &[Segment], total: u64) {
    assert!(
        !segments.is_empty(),
        "a planned task must persist segment rows"
    );

    let mut indices: Vec<u32> = segments.iter().map(|segment| segment.index).collect();
    indices.sort_unstable();
    indices.dedup();
    assert_eq!(
        indices.len(),
        segments.len(),
        "segment indexes must be unique: {segments:?}"
    );

    let mut ranges: Vec<(u64, u64)> = segments
        .iter()
        .map(|segment| (segment.start, segment.end))
        .collect();
    ranges.sort_unstable();

    let mut expected_start = 0u64;
    for (position, (start, end)) in ranges.iter().enumerate() {
        assert_eq!(
            *start, expected_start,
            "segment {position} must start where the previous range ended, ranges={ranges:?}"
        );
        assert!(
            end > start,
            "segment {position} must have a positive length, ranges={ranges:?}"
        );
        expected_start = *end;
    }
    assert_eq!(
        expected_start, total,
        "segment rows must cover [0,{total}) without gaps or overlap, ranges={ranges:?}"
    );

    for segment in segments {
        assert!(
            segment.downloaded_bytes <= segment.end - segment.start,
            "segment {} progress {} exceeds its range {}..{}",
            segment.index,
            segment.downloaded_bytes,
            segment.start,
            segment.end
        );
    }
}

#[tokio::test]
async fn fresh_task_that_gets_200_for_ranges_falls_back_to_one_connection() {
    let body = non_periodic_bytes(4 * 1024 * 1024);
    let hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body: body.clone(),
        range_mode: RangeMode::Probe206Then200,
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    let id = engine
        .add_task(make_spec(
            fixture.url("/fresh-ignored-range"),
            "fresh-ignored.bin",
            4,
            Some(hash.clone()),
        ))
        .expect("add task");

    let ((), observed) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("engine run timed out")
                .expect("engine run failed");
        },
        wait_for_terminal(engine.store(), &id, WAIT_TIMEOUT),
    );

    assert_eq!(
        observed,
        TaskState::Completed,
        "a fresh task with no written bytes must fall back to one connection and complete"
    );
    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(task.state, TaskState::Completed);
    assert_eq!(task.error_code, None, "completed task must carry no error");
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));

    let final_path = dest.path().join("fresh-ignored.bin");
    assert_eq!(
        std::fs::read(&final_path).expect("read final file"),
        body,
        "final file must exactly match the fixture body"
    );
    assert!(
        !part_path(dest.path(), "fresh-ignored.bin").exists(),
        "no part file may remain after completion"
    );
    let mut names: Vec<String> = std::fs::read_dir(dest.path())
        .expect("read dest")
        .map(|entry| {
            entry
                .expect("dir entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["fresh-ignored.bin".to_string()],
        "only the complete final file may exist"
    );

    let request_count = fixture.requests().len();
    assert!(
        request_count <= 40,
        "the range fallback must be bounded, observed {request_count} fixture requests for a 4 MiB body"
    );
}

#[tokio::test]
async fn ranged_requests_that_switch_to_200_after_data_fail_safely() {
    let body = non_periodic_bytes(4 * 1024 * 1024);
    let total = body.len() as u64;
    let fixture = RangeServer::start(FixtureConfig {
        body: body.clone(),
        range_mode: RangeMode::HonorRange,
        ignore_after_bytes: Some(2 * 1024 * 1024),
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    let id = engine
        .add_task(make_spec(
            fixture.url("/late-ignored-range"),
            "late-ignored.bin",
            4,
            None,
        ))
        .expect("add task");

    let ((), observed) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("engine run timed out")
                .expect("engine run failed");
        },
        wait_for_terminal(engine.store(), &id, WAIT_TIMEOUT),
    );

    assert_eq!(
        observed,
        TaskState::Failed,
        "a ranged request answered with 200 after data was written must fail the task; requests={:?}",
        fixture
            .requests()
            .iter()
            .map(|request| request.range.clone())
            .collect::<Vec<_>>()
    );
    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(task.state, TaskState::Failed);
    assert_eq!(
        task.error_code.as_deref(),
        Some("RANGE_IGNORED"),
        "expected RANGE_IGNORED, got {:?}: {:?}",
        task.error_code,
        task.error_message
    );
    assert!(
        task.downloaded_bytes > 0,
        "the task must have persisted bytes before the ignored range"
    );
    assert!(
        task.downloaded_bytes < total,
        "an incomplete task must not report the full body as downloaded"
    );

    let part = part_path(dest.path(), "late-ignored.bin");
    assert!(part.exists(), "a failed task must preserve its part file");
    assert!(
        !dest.path().join("late-ignored.bin").exists(),
        "a failed task must not create a final file"
    );

    let part_bytes = std::fs::read(&part).expect("read part file");
    assert_eq!(
        part_bytes.len() as u64,
        total,
        "the preallocated part must keep the full declared size"
    );
    for (offset, (stored, expected)) in part_bytes.iter().zip(body.iter()).enumerate() {
        if *stored != 0 {
            assert_eq!(
                stored, expected,
                "part byte {offset} does not match the body byte at the same offset"
            );
        }
    }

    for segment in engine.store().segments_for_task(&id).expect("segments") {
        let start = segment.start as usize;
        let length = segment.downloaded_bytes as usize;
        if length == 0 {
            continue;
        }
        assert_eq!(
            &part_bytes[start..start + length],
            &body[start..start + length],
            "persisted bytes for segment [{}..{}) must match the body at the same offsets",
            segment.start,
            segment.end
        );
    }
}

#[tokio::test]
async fn dynamic_splitting_creates_segments_keeps_ranges_and_matches_hash() {
    const MIB: usize = 1024 * 1024;
    let total = 32 * MIB;
    let body = pattern_bytes(0x5a, total);
    let hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body: body.clone(),
        range_mode: RangeMode::HonorRange,
        slow_first_half_delay: Some(Duration::from_millis(12)),
        stream_chunk_size: 256 * 1024,
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    let id = engine
        .add_task(make_spec(
            fixture.url("/split-slow-first"),
            "split.bin",
            2,
            Some(hash.clone()),
        ))
        .expect("add task");

    let ((), max_rows) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("engine run timed out")
                .expect("engine run failed");
        },
        async {
            let start = Instant::now();
            let mut max_rows = 0usize;
            loop {
                let task = engine.store().task(&id).expect("load task").expect("task");
                let segments = engine.store().segments_for_task(&id).expect("segments");
                if !segments.is_empty() {
                    assert_segment_ranges(&segments, total as u64);
                    max_rows = max_rows.max(segments.len());
                }
                if matches!(
                    task.state,
                    TaskState::Completed
                        | TaskState::Failed
                        | TaskState::Cancelled
                        | TaskState::Paused
                ) {
                    break;
                }
                if start.elapsed() > WAIT_TIMEOUT {
                    panic!(
                        "timed out observing the split download, state {:?}, rows {max_rows}",
                        task.state
                    );
                }
                sleep(POLL_INTERVAL).await;
            }
            max_rows
        }
    );

    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(
        task.state,
        TaskState::Completed,
        "split download failed with {:?}: {:?}",
        task.error_code,
        task.error_message
    );
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));
    assert!(
        max_rows >= 3,
        "dynamic splitting must create a segment beyond the initial two, observed max {max_rows}"
    );
    assert!(
        max_rows <= 6,
        "split depth must stay bounded, observed {max_rows} segment rows"
    );

    let final_path = dest.path().join("split.bin");
    assert_eq!(
        std::fs::read(&final_path).expect("read final file"),
        body,
        "dynamic splitting must not corrupt the final file"
    );
    assert!(
        !part_path(dest.path(), "split.bin").exists(),
        "no part file may remain after completion"
    );
    assert_segment_ranges(
        &engine.store().segments_for_task(&id).expect("segments"),
        total as u64,
    );
}

#[tokio::test]
async fn transient_server_errors_retry_then_complete_with_bounded_requests() {
    let body = non_periodic_bytes(256 * 1024);
    let hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body: body.clone(),
        fail_first_n: 2,
        fail_status: 500,
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    let id = engine
        .add_task(make_spec(
            fixture.url("/retry-500"),
            "retry-500.bin",
            1,
            Some(hash.clone()),
        ))
        .expect("add task");

    let ((), observed) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("engine run timed out")
                .expect("engine run failed");
        },
        wait_for_terminal(engine.store(), &id, WAIT_TIMEOUT),
    );

    assert_eq!(
        observed,
        TaskState::Completed,
        "two transient 500 responses must be retried, requests={}",
        fixture.requests().len()
    );
    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(task.error_code, None);
    assert_eq!(
        std::fs::read(dest.path().join("retry-500.bin")).expect("read final"),
        body,
        "retried download must produce the fixture bytes exactly once"
    );
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));
    let requests = fixture.requests().len();
    assert!(
        requests <= 12,
        "a small one-segment download must use a bounded request count, observed {requests}"
    );
}

#[tokio::test]
async fn persistent_server_errors_stop_after_a_bounded_threshold() {
    let body = non_periodic_bytes(128 * 1024);
    let fixture = RangeServer::start(FixtureConfig {
        body: body.clone(),
        status_override: Some(500),
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    let id = engine
        .add_task(make_spec(
            fixture.url("/always-500"),
            "always-500.bin",
            1,
            None,
        ))
        .expect("add task");

    let ((), observed) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("engine run timed out")
                .expect("engine run failed");
        },
        wait_for_terminal(engine.store(), &id, WAIT_TIMEOUT),
    );

    assert!(
        matches!(observed, TaskState::Failed | TaskState::Paused),
        "a persistent server failure must stop the task, observed {observed:?}"
    );
    let task = engine.store().task(&id).expect("load task").expect("task");
    assert!(
        matches!(
            task.error_code.as_deref(),
            Some("SERVER_ERROR") | Some("RETRY_EXHAUSTED")
        ),
        "expected a typed retry exhaustion code, got {:?}: {:?}",
        task.error_code,
        task.error_message
    );
    let requests = fixture.requests().len();
    assert!(
        requests <= 20,
        "an always-failing task must stop after a bounded attempt count, observed {requests} requests"
    );
    assert!(
        !dest.path().join("always-500.bin").exists(),
        "an always-failing task must not create a final file"
    );
}

#[tokio::test]
async fn integer_retry_after_is_observed_before_the_retry() {
    let body = non_periodic_bytes(256 * 1024);
    let hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body: body.clone(),
        rate_limit_429: true,
        rate_limit_times: Some(1),
        retry_after: Some("1".to_string()),
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    let id = engine
        .add_task(make_spec(
            fixture.url("/retry-after-1"),
            "retry-after-1.bin",
            1,
            Some(hash.clone()),
        ))
        .expect("add task");

    let started = Instant::now();
    let ((), (observed, elapsed)) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("engine run timed out")
                .expect("engine run failed");
        },
        async {
            let state = wait_for_terminal(engine.store(), &id, WAIT_TIMEOUT).await;
            (state, started.elapsed())
        },
    );

    assert_eq!(
        observed,
        TaskState::Completed,
        "a single 429 with Retry-After must be retried, requests={}",
        fixture.requests().len()
    );
    assert!(
        elapsed >= Duration::from_secs(1),
        "the engine must wait at least the integer Retry-After delay, elapsed {elapsed:?}"
    );
    assert!(
        fixture.requests().len() <= 20,
        "the 429 retry must stay bounded, observed {} requests",
        fixture.requests().len()
    );
    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));
}

#[tokio::test]
async fn http_date_retry_after_falls_back_to_a_short_exponential_delay() {
    // The date is far in the future. Sleeping until it would exceed the test
    // bound, so a quick completion proves the HTTP-date fallback path.
    let body = non_periodic_bytes(256 * 1024);
    let hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body: body.clone(),
        rate_limit_429: true,
        rate_limit_times: Some(1),
        retry_after: Some("Wed, 21 Oct 2099 07:28:00 GMT".to_string()),
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    let id = engine
        .add_task(make_spec(
            fixture.url("/retry-after-date"),
            "retry-after-date.bin",
            1,
            Some(hash.clone()),
        ))
        .expect("add task");

    let started = Instant::now();
    let bound = Duration::from_secs(8);
    let ((), (observed, elapsed)) = tokio::join!(
        async {
            timeout(bound, engine.run())
                .await
                .expect("an HTTP-date Retry-After must not sleep until the date")
                .expect("engine run failed");
        },
        async {
            let state = wait_for_terminal(engine.store(), &id, bound).await;
            (state, started.elapsed())
        },
    );

    assert_eq!(
        observed,
        TaskState::Completed,
        "the HTTP-date 429 retry must still complete, requests={}",
        fixture.requests().len()
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "the HTTP-date value must use the exponential fallback, elapsed {elapsed:?}"
    );
    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));
}

#[tokio::test]
async fn auth_and_not_found_failures_are_not_retried() {
    for (status, expected_code) in [
        (401u16, "AUTH_REQUIRED"),
        (403, "AUTH_REQUIRED"),
        (404, "NOT_FOUND"),
    ] {
        let body = non_periodic_bytes(64 * 1024);
        let fixture = RangeServer::start(FixtureConfig {
            body: body.clone(),
            status_override: Some(status),
            ..Default::default()
        });
        let dest = tempdir().expect("dest");
        let store_dir = tempdir().expect("store");
        let engine = Engine::new(
            open_store(&store_dir),
            dest.path().to_path_buf(),
            EngineOptions { max_concurrent: 1 },
        );
        let filename = format!("status-{status}.bin");

        let id = engine
            .add_task(make_spec(
                fixture.url(&format!("/status-{status}")),
                &filename,
                1,
                None,
            ))
            .expect("add task");

        let ((), observed) = tokio::join!(
            async {
                timeout(WAIT_TIMEOUT, engine.run())
                    .await
                    .expect("engine run timed out")
                    .expect("engine run failed");
            },
            wait_for_terminal(engine.store(), &id, WAIT_TIMEOUT),
        );

        assert_eq!(
            observed,
            TaskState::Failed,
            "status {status} must fail the task"
        );
        let task = engine.store().task(&id).expect("load task").expect("task");
        assert_eq!(
            task.error_code.as_deref(),
            Some(expected_code),
            "status {status} must map to {expected_code}, got {:?}",
            task.error_code
        );
        assert_eq!(
            fixture.requests().len(),
            1,
            "a non-retryable {status} response must be requested exactly once"
        );
        assert!(
            !dest.path().join(&filename).exists(),
            "status {status} must not create a final file"
        );
        let part = part_path(dest.path(), &filename);
        if part.exists() {
            assert_eq!(
                std::fs::metadata(&part).expect("part metadata").len(),
                0,
                "a non-retryable HTTP failure must not write part bytes"
            );
        }
    }
}

#[tokio::test]
async fn invalid_content_range_start_fails_without_writing_part_bytes() {
    let body = non_periodic_bytes(1024 * 1024);
    let fixture = RangeServer::start(FixtureConfig {
        body: body.clone(),
        invalid_content_range_after_probe: Some(1),
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    let filename = "range-mismatch.bin";

    let id = engine
        .add_task(make_spec(fixture.url("/range-mismatch"), filename, 1, None))
        .expect("add task");

    let ((), observed) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("engine run timed out")
                .expect("engine run failed");
        },
        wait_for_terminal(engine.store(), &id, WAIT_TIMEOUT),
    );

    assert_eq!(observed, TaskState::Failed);
    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(
        task.error_code.as_deref(),
        Some("RANGE_MISMATCH"),
        "got {:?}: {:?}",
        task.error_code,
        task.error_message
    );
    let part = part_path(dest.path(), filename);
    assert!(part.exists(), "the part must be preserved for diagnosis");
    let bytes = std::fs::read(&part).expect("read part");
    assert!(
        bytes.iter().all(|byte| *byte == 0),
        "a mismatched Content-Range must not write any body bytes"
    );
    assert!(
        !dest.path().join(filename).exists(),
        "a range mismatch must not create a final file"
    );
}

#[tokio::test]
async fn an_overlong_body_is_truncated_at_the_segment_end() {
    let body = non_periodic_bytes(512 * 1024);
    let hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body: body.clone(),
        overrun_bytes: Some(32 * 1024),
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    let filename = "overlong.bin";

    let id = engine
        .add_task(make_spec(
            fixture.url("/overlong"),
            filename,
            1,
            Some(hash.clone()),
        ))
        .expect("add task");

    let ((), observed) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("engine run timed out")
                .expect("engine run failed");
        },
        wait_for_terminal(engine.store(), &id, WAIT_TIMEOUT),
    );

    assert_eq!(
        observed,
        TaskState::Completed,
        "an overlong body must be truncated, not rejected; requests={}",
        fixture.requests().len()
    );
    let final_path = dest.path().join(filename);
    let final_bytes = std::fs::read(&final_path).expect("read final");
    assert_eq!(
        final_bytes.len(),
        body.len(),
        "the body must not overrun the segment end"
    );
    assert_eq!(final_bytes, body);
    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));
    assert!(!part_path(dest.path(), filename).exists());
}

#[tokio::test]
async fn a_truncated_body_retries_from_the_persisted_offset_and_completes() {
    let body = non_periodic_bytes(1024 * 1024);
    let hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body: body.clone(),
        truncate_first_ranged_after: Some(256 * 1024),
        truncate_ranged_times: 1,
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    let filename = "truncated-once.bin";

    let id = engine
        .add_task(make_spec(
            fixture.url("/truncated-once"),
            filename,
            2,
            Some(hash.clone()),
        ))
        .expect("add task");

    let ((), observed) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("engine run timed out")
                .expect("engine run failed");
        },
        wait_for_terminal(engine.store(), &id, WAIT_TIMEOUT),
    );

    assert_eq!(
        observed,
        TaskState::Completed,
        "a short body must be retried from the persisted offset, requests={}",
        fixture.requests().len()
    );
    let final_path = dest.path().join(filename);
    assert_eq!(std::fs::read(&final_path).expect("read final"), body);
    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));
    assert!(!part_path(dest.path(), filename).exists());
    let requests = fixture.requests().len();
    assert!(
        requests <= 20,
        "truncation retries must stay bounded, observed {requests} requests"
    );
}

#[tokio::test]
async fn persistent_truncation_stops_after_the_retry_bound_and_preserves_the_part() {
    let body = non_periodic_bytes(1024 * 1024);
    let fixture = RangeServer::start(FixtureConfig {
        body: body.clone(),
        truncate_first_ranged_after: Some(64 * 1024),
        truncate_ranged_times: usize::MAX,
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    let filename = "truncated-always.bin";

    let id = engine
        .add_task(make_spec(
            fixture.url("/truncated-always"),
            filename,
            1,
            None,
        ))
        .expect("add task");

    let ((), observed) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("engine run timed out")
                .expect("engine run failed");
        },
        wait_for_terminal(engine.store(), &id, WAIT_TIMEOUT),
    );

    assert_eq!(observed, TaskState::Failed);
    let task = engine.store().task(&id).expect("load task").expect("task");
    assert!(
        matches!(
            task.error_code.as_deref(),
            Some("NETWORK_INTERRUPTED")
                | Some("INCOMPLETE_BODY")
                | Some("RETRY_EXHAUSTED")
                | Some("IO_ERROR")
        ),
        "expected a typed interruption error, got {:?}: {:?}",
        task.error_code,
        task.error_message
    );
    let part = part_path(dest.path(), filename);
    assert!(
        part.exists(),
        "a persistently truncated task must preserve its part"
    );
    assert_eq!(
        std::fs::metadata(&part).expect("part metadata").len(),
        body.len() as u64,
        "the preallocated part must keep its declared size"
    );
    assert!(
        !dest.path().join(filename).exists(),
        "a persistently truncated task must not create a final file"
    );
    let requests = fixture.requests().len();
    assert!(
        requests <= 20,
        "persistent truncation must stop at the retry bound, observed {requests} requests"
    );
}

#[tokio::test]
async fn a_stalled_server_produces_a_timeout_code_and_no_files() {
    let fixture = RangeServer::start(FixtureConfig {
        body: non_periodic_bytes(4096),
        stall_before_response: Some(Duration::from_secs(3)),
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let client = build_client_with_timeouts(Duration::from_millis(200), Duration::from_millis(150))
        .expect("build the short-timeout client");

    let result = timeout(
        Duration::from_secs(2),
        client.get(fixture.url("/stall")).send(),
    )
    .await
    .expect("the request must fail before the outer bound");
    let error = result.expect_err("a stalled server must not produce a response");
    let classified = classify_reqwest_error(&error);

    assert_eq!(
        classified.code(),
        "TIMEOUT",
        "a read timeout must classify as TIMEOUT, got {}: {}",
        classified.code(),
        classified
    );
    assert!(
        classified.is_retryable(),
        "a timeout is a transient failure and must be retryable"
    );
    assert!(
        std::fs::read_dir(dest.path())
            .expect("read dest")
            .next()
            .is_none(),
        "the timeout probe must not create a file or part"
    );
    assert_eq!(
        fixture.requests().len(),
        1,
        "the timeout probe must issue exactly one request"
    );
}

fn make_spec_with_headers(
    url: String,
    filename: &str,
    connections: u32,
    expected_sha256: Option<String>,
    headers: Vec<(String, String)>,
) -> DownloadSpec {
    DownloadSpec {
        url,
        filename: filename.to_string(),
        connections,
        allow_html: false,
        expected_sha256,
        extra_headers: headers,
        destination: None,
    }
}

#[derive(Debug, Clone, Copy)]
struct ProgressSample {
    at: Instant,
    downloaded: u64,
}

async fn sample_progress(store: &Database, id: &str, deadline: Duration) -> Vec<ProgressSample> {
    let start = Instant::now();
    let mut samples = Vec::new();
    loop {
        let task = store.task(id).expect("load task").expect("task exists");
        samples.push(ProgressSample {
            at: Instant::now(),
            downloaded: task.downloaded_bytes,
        });
        if matches!(
            task.state,
            TaskState::Completed | TaskState::Failed | TaskState::Cancelled | TaskState::Paused
        ) {
            return samples;
        }
        if start.elapsed() > deadline {
            panic!(
                "timed out sampling task {id}, state {:?}, downloaded={}",
                task.state, task.downloaded_bytes
            );
        }
        sleep(Duration::from_millis(25)).await;
    }
}

fn downloaded_at(samples: &[ProgressSample], at: Instant) -> u64 {
    samples
        .iter()
        .take_while(|sample| sample.at <= at)
        .last()
        .map(|sample| sample.downloaded)
        .unwrap_or(0)
}

/// Average persisted throughput over a steady window that excludes the
/// startup ramp and the finalization tail.
fn steady_rate(
    samples: &[ProgressSample],
    total: u64,
    startup: Duration,
    finalize: Duration,
) -> f64 {
    let first_byte = samples
        .iter()
        .find(|sample| sample.downloaded > 0)
        .expect("the task must persist at least one byte");
    let done = samples
        .iter()
        .find(|sample| sample.downloaded >= total)
        .expect("the task must reach its declared total");
    let window_start = first_byte.at + startup;
    let window_end = done.at.checked_sub(finalize).unwrap_or(done.at);
    assert!(
        window_end > window_start,
        "the steady window must be non-empty, start={window_start:?}, end={window_end:?}"
    );

    let bytes =
        downloaded_at(samples, window_end).saturating_sub(downloaded_at(samples, window_start));
    bytes as f64 / (window_end - window_start).as_secs_f64()
}

fn sqlite_text_dump(path: &Path) -> String {
    let connection = rusqlite::Connection::open(path).expect("open sqlite for scan");
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .expect("list tables");
    let tables: Vec<String> = statement
        .query_map([], |row| row.get(0))
        .expect("query tables")
        .collect::<rusqlite::Result<Vec<String>>>()
        .expect("collect tables");
    drop(statement);

    let mut dump = String::new();
    for table in tables {
        if table.starts_with("sqlite_") {
            continue;
        }
        let escaped = table.replace('"', "\"\"");
        let mut statement = connection
            .prepare(&format!("SELECT * FROM \"{}\"", escaped))
            .expect("prepare table scan");
        let columns = statement.column_count();
        let mut rows = statement.query([]).expect("query table rows");
        while let Some(row) = rows.next().expect("next table row") {
            for index in 0..columns {
                let value: rusqlite::types::Value =
                    row.get(index).expect("read table cell as a value");
                match value {
                    rusqlite::types::Value::Text(text) => dump.push_str(&text),
                    rusqlite::types::Value::Blob(bytes) => {
                        dump.push_str(&String::from_utf8_lossy(&bytes))
                    }
                    _ => {}
                }
                dump.push('\u{1f}');
            }
        }
    }
    dump
}

fn read_log_files(logger: &Logger) -> String {
    let mut logs = String::new();
    for path in logger.files().expect("list log files") {
        if let Ok(text) = std::fs::read_to_string(&path) {
            logs.push_str(&text);
            logs.push('\n');
        }
    }
    logs
}

fn debug_logger(directory: &Path) -> Arc<Logger> {
    Arc::new(
        Logger::new(
            LogConfig::new(directory.to_path_buf(), 10 * 1024 * 1024, 5)
                .with_level(LogLevel::Debug),
        )
        .expect("create logger"),
    )
}

async fn run_engine_to_terminal(engine: &Engine, store: &Database, id: &str) {
    let ((), state) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("engine run timed out")
                .expect("engine run failed");
        },
        wait_for_terminal(store, id, WAIT_TIMEOUT),
    );
    assert!(
        matches!(
            state,
            TaskState::Completed | TaskState::Failed | TaskState::Cancelled | TaskState::Paused
        ),
        "task {id} did not reach a terminal state, observed {state:?}"
    );
}

#[tokio::test]
async fn per_task_rate_limit_holds_steady_throughput() {
    const BODY_BYTES: usize = 8 * 1024 * 1024;
    let body = pattern_bytes(0x31, BODY_BYTES);
    let hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body,
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    let id = engine
        .add_task(make_spec(
            fixture.url("/rate/per-task.bin"),
            "per-task.bin",
            1,
            Some(hash.clone()),
        ))
        .expect("add task");
    engine.set_task_rate_limit(&id, Some(MIB));

    let ((), samples) = tokio::join!(
        async {
            timeout(Duration::from_secs(30), engine.run())
                .await
                .expect("rate-limited engine run timed out")
                .expect("engine run failed");
        },
        sample_progress(engine.store(), &id, Duration::from_secs(30)),
    );

    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(
        task.state,
        TaskState::Completed,
        "rate-limited download failed with {:?}: {:?}",
        task.error_code,
        task.error_message
    );
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));

    let measured = steady_rate(&samples, BODY_BYTES as u64, RATE_STARTUP, RATE_FINALIZE);
    let expected = MIB as f64;
    assert!(
        (expected * 0.9..=expected * 1.1).contains(&measured),
        "per-task steady throughput {measured:.0} B/s is outside 10% of {expected:.0} B/s"
    );
}

#[tokio::test]
async fn global_rate_limit_applies_to_a_single_task() {
    const BODY_BYTES: usize = 4 * 1024 * 1024;
    let body = pattern_bytes(0x42, BODY_BYTES);
    let hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body,
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    engine.set_global_rate_limit(Some(MIB));

    let id = engine
        .add_task(make_spec(
            fixture.url("/rate/global.bin"),
            "global.bin",
            1,
            Some(hash.clone()),
        ))
        .expect("add task");

    let ((), samples) = tokio::join!(
        async {
            timeout(Duration::from_secs(25), engine.run())
                .await
                .expect("globally rate-limited engine run timed out")
                .expect("engine run failed");
        },
        sample_progress(engine.store(), &id, Duration::from_secs(25)),
    );

    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(
        task.state,
        TaskState::Completed,
        "globally rate-limited download failed with {:?}: {:?}",
        task.error_code,
        task.error_message
    );
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));

    let measured = steady_rate(&samples, BODY_BYTES as u64, RATE_STARTUP, RATE_FINALIZE);
    let expected = MIB as f64;
    assert!(
        (expected * 0.9..=expected * 1.1).contains(&measured),
        "global steady throughput {measured:.0} B/s is outside 10% of {expected:.0} B/s"
    );
}

#[tokio::test]
async fn an_unlimited_task_is_not_throttled() {
    const BODY_BYTES: usize = 8 * 1024 * 1024;
    let body = pattern_bytes(0x53, BODY_BYTES);
    let hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body,
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    let id = engine
        .add_task(make_spec(
            fixture.url("/rate/unlimited.bin"),
            "unlimited.bin",
            1,
            Some(hash.clone()),
        ))
        .expect("add task");

    let started = Instant::now();
    let ((), observed) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("unlimited engine run timed out")
                .expect("engine run failed");
        },
        wait_for_terminal(engine.store(), &id, WAIT_TIMEOUT),
    );
    let elapsed = started.elapsed();

    assert_eq!(observed, TaskState::Completed);
    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));
    assert!(
        elapsed < Duration::from_secs(5),
        "an unlimited local 8 MiB task must not be throttled, elapsed {elapsed:?}"
    );
}

#[tokio::test]
async fn proxy_basic_auth_forwards_and_never_leaks_to_the_origin() {
    const HEADER_VALUE: &str = "oneget-proxy-test-header";
    const PROXY_USER: &str = "oneget-proxy-user";
    const PROXY_PASSWORD: &str = "oneget-proxy-password-secret";
    let body = non_periodic_bytes(1024 * 1024);
    let hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body: body.clone(),
        ..Default::default()
    });
    let proxy = ProxyServer::start(Some(ProxyCredentials {
        username: PROXY_USER.to_string(),
        password: PROXY_PASSWORD.to_string(),
    }));
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let log_dir = tempdir().expect("logs");
    let logger = debug_logger(log_dir.path());
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    engine
        .set_proxy(Some(ProxyConfig {
            url: proxy.url(),
            username: Some(PROXY_USER.to_string()),
            password: Some(PROXY_PASSWORD.to_string()),
        }))
        .expect("configure the authenticated proxy");
    engine.set_logger(Arc::clone(&logger));

    let id = engine
        .add_task(make_spec_with_headers(
            fixture.url("/proxy/payload.bin"),
            "proxy-payload.bin",
            2,
            Some(hash.clone()),
            vec![("X-Proxy-Test".to_string(), HEADER_VALUE.to_string())],
        ))
        .expect("add proxied task");

    run_engine_to_terminal(&engine, engine.store(), &id).await;
    logger.flush().expect("flush logs");

    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(
        task.state,
        TaskState::Completed,
        "proxied download failed with {:?}: {:?}",
        task.error_code,
        task.error_message
    );
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));
    assert_eq!(
        std::fs::read(dest.path().join("proxy-payload.bin")).expect("read final"),
        body
    );

    let expected = proxy
        .expected_authorization()
        .expect("the fixture proxy requires credentials")
        .to_string();
    let requests = proxy.requests();
    assert!(!requests.is_empty(), "the proxy must observe requests");
    for request in &requests {
        assert_eq!(
            request.headers.get("proxy-authorization"),
            Some(&expected),
            "the proxy must observe Proxy-Authorization for {}",
            request.target
        );
        let forwarded = request
            .forwarded_headers
            .as_ref()
            .expect("an authenticated request must be forwarded");
        assert!(
            !forwarded.contains_key("proxy-authorization"),
            "proxy credentials must not be forwarded to the origin"
        );
        assert!(
            !forwarded.contains_key("proxy-connection"),
            "hop-by-hop proxy headers must not be forwarded"
        );
    }

    let origin_requests = fixture.requests();
    assert!(!origin_requests.is_empty());
    assert_eq!(
        requests.len(),
        origin_requests.len(),
        "every client request must reach the origin through the proxy"
    );
    for request in &origin_requests {
        assert!(
            !request.headers.contains_key("proxy-authorization"),
            "the origin must never see proxy credentials"
        );
    }

    let origin_prefix = fixture.url("");
    for request in &requests {
        assert!(
            request.target.starts_with(&origin_prefix),
            "the proxy forwarded to an unexpected destination: {}",
            request.target
        );
    }

    let dump = sqlite_text_dump(&store_dir.path().join("tasks.db"));
    assert!(
        !dump.contains(PROXY_USER),
        "the proxy username reached the store"
    );
    assert!(
        !dump.contains(PROXY_PASSWORD),
        "the proxy password reached the store"
    );
    let logs = read_log_files(&logger);
    assert!(
        !logs.contains(PROXY_USER),
        "the proxy username reached the logs"
    );
    assert!(
        !logs.contains(PROXY_PASSWORD),
        "the proxy password reached the logs"
    );
}

#[tokio::test]
async fn wrong_proxy_password_fails_with_a_typed_proxy_auth_error() {
    const PROXY_USER: &str = "oneget-proxy-user";
    const GOOD_PASSWORD: &str = "oneget-proxy-good-secret";
    const WRONG_PASSWORD: &str = "oneget-proxy-wrong-secret";
    let body = non_periodic_bytes(128 * 1024);
    let fixture = RangeServer::start(FixtureConfig {
        body,
        ..Default::default()
    });
    let proxy = ProxyServer::start(Some(ProxyCredentials {
        username: PROXY_USER.to_string(),
        password: GOOD_PASSWORD.to_string(),
    }));
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let log_dir = tempdir().expect("logs");
    let logger = debug_logger(log_dir.path());
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    engine
        .set_proxy(Some(ProxyConfig {
            url: proxy.url(),
            username: Some(PROXY_USER.to_string()),
            password: Some(WRONG_PASSWORD.to_string()),
        }))
        .expect("configure the proxy with a wrong password");
    engine.set_logger(Arc::clone(&logger));

    let id = engine
        .add_task(make_spec(
            fixture.url("/proxy/wrong-password.bin"),
            "wrong-password.bin",
            1,
            None,
        ))
        .expect("add task");
    run_engine_to_terminal(&engine, engine.store(), &id).await;
    logger.flush().expect("flush logs");

    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(task.state, TaskState::Failed);
    assert_eq!(
        task.error_code.as_deref(),
        Some("PROXY_AUTH_REQUIRED"),
        "wrong proxy credentials must produce a typed proxy auth error, got {:?}: {:?}",
        task.error_code,
        task.error_message
    );
    assert!(
        !dest.path().join("wrong-password.bin").exists(),
        "a proxy authentication failure must not create a final file"
    );

    let requests = proxy.requests();
    assert!(!requests.is_empty(), "the proxy must observe the attempt");
    assert!(
        requests
            .iter()
            .all(|request| request.forwarded_headers.is_none()),
        "a rejected proxy request must not be forwarded to the origin"
    );
    assert!(
        fixture.requests().is_empty(),
        "the origin must not receive a request rejected by the proxy"
    );

    let dump = sqlite_text_dump(&store_dir.path().join("tasks.db"));
    for secret in [PROXY_USER, GOOD_PASSWORD, WRONG_PASSWORD] {
        assert!(
            !dump.contains(secret),
            "proxy credential {secret} reached the store"
        );
    }
    let logs = read_log_files(&logger);
    for secret in [PROXY_USER, GOOD_PASSWORD, WRONG_PASSWORD] {
        assert!(
            !logs.contains(secret),
            "proxy credential {secret} reached the logs"
        );
    }
}

#[tokio::test]
async fn a_proxy_url_with_userinfo_is_rejected_without_persisting_credentials() {
    let proxy = ProxyServer::start(None);
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    let error = engine
        .set_proxy(Some(ProxyConfig {
            url: format!("http://oneget-user:oneget-embedded-secret@{}", proxy.addr()),
            username: None,
            password: None,
        }))
        .expect_err("a proxy URL with userinfo must be rejected");
    assert_eq!(
        error.code(),
        "INVALID_PROXY_URL",
        "expected INVALID_PROXY_URL, got {}: {}",
        error.code(),
        error
    );
    assert!(
        proxy.requests().is_empty(),
        "an invalid proxy configuration must not send a request"
    );
    assert!(
        engine.store().list_tasks().expect("list tasks").is_empty(),
        "an invalid proxy configuration must not create a task"
    );

    let dump = sqlite_text_dump(&store_dir.path().join("tasks.db"));
    assert!(
        !dump.contains("oneget-embedded-secret"),
        "embedded proxy credentials reached the store"
    );
}

#[tokio::test]
async fn cookie_scope_sends_only_matching_host_path_and_unexpired_insecure_cookies() {
    const HOST_ONLY: &str = "oneget-cookie-host-only";
    const PATH_OK: &str = "oneget-cookie-path-ok";
    const OTHER_HOST: &str = "oneget-cookie-other-host";
    const PATH_WRONG: &str = "oneget-cookie-path-wrong";
    const EXPIRED: &str = "oneget-cookie-expired";
    const SECURE: &str = "oneget-cookie-secure";
    let body = non_periodic_bytes(256 * 1024);
    let hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body,
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let log_dir = tempdir().expect("logs");
    let cookie_dir = tempdir().expect("cookies");
    let logger = debug_logger(log_dir.path());
    let cookie_path = write_cookie_file(
        cookie_dir.path(),
        &[
            NetscapeCookie {
                domain: "127.0.0.1".to_string(),
                include_subdomains: false,
                path: "/".to_string(),
                secure: false,
                expires: future_expiry(),
                name: "host_only".to_string(),
                value: HOST_ONLY.to_string(),
            },
            NetscapeCookie {
                domain: "localhost".to_string(),
                include_subdomains: false,
                path: "/".to_string(),
                secure: false,
                expires: future_expiry(),
                name: "other_host".to_string(),
                value: OTHER_HOST.to_string(),
            },
            NetscapeCookie {
                domain: "127.0.0.1".to_string(),
                include_subdomains: false,
                path: "/allowed".to_string(),
                secure: false,
                expires: future_expiry(),
                name: "path_ok".to_string(),
                value: PATH_OK.to_string(),
            },
            NetscapeCookie {
                domain: "127.0.0.1".to_string(),
                include_subdomains: false,
                path: "/blocked".to_string(),
                secure: false,
                expires: future_expiry(),
                name: "path_wrong".to_string(),
                value: PATH_WRONG.to_string(),
            },
            NetscapeCookie {
                domain: "127.0.0.1".to_string(),
                include_subdomains: false,
                path: "/".to_string(),
                secure: false,
                expires: past_expiry(),
                name: "expired".to_string(),
                value: EXPIRED.to_string(),
            },
            NetscapeCookie {
                domain: "127.0.0.1".to_string(),
                include_subdomains: false,
                path: "/".to_string(),
                secure: true,
                expires: future_expiry(),
                name: "secure_only".to_string(),
                value: SECURE.to_string(),
            },
        ],
    );
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    engine.set_cookie_file(Some(cookie_path));
    engine.set_logger(Arc::clone(&logger));

    let id = engine
        .add_task(make_spec(
            fixture.url("/allowed/payload.bin"),
            "cookie-scope.bin",
            1,
            Some(hash.clone()),
        ))
        .expect("add task");
    run_engine_to_terminal(&engine, engine.store(), &id).await;

    // The same listener is reachable as both 127.0.0.1 and localhost, so the
    // second task isolates host matching from any port or redirect difference.
    let localhost_id = engine
        .add_task(make_spec(
            fixture.host_url("localhost", "/allowed/payload-localhost.bin"),
            "cookie-scope-localhost.bin",
            1,
            None,
        ))
        .expect("add localhost task");
    run_engine_to_terminal(&engine, engine.store(), &localhost_id).await;
    logger.flush().expect("flush logs");

    for task_id in [&id, &localhost_id] {
        let task = engine
            .store()
            .task(task_id)
            .expect("load task")
            .expect("task");
        assert_eq!(
            task.state,
            TaskState::Completed,
            "cookie-scoped download failed with {:?}: {:?}",
            task.error_code,
            task.error_message
        );
    }
    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));

    let requests = fixture.requests();
    assert!(!requests.is_empty());
    let ipv4_names = BTreeSet::from(["host_only".to_string(), "path_ok".to_string()]);
    let localhost_names = BTreeSet::from(["other_host".to_string()]);
    let mut saw_ipv4 = false;
    let mut saw_localhost = false;
    for request in &requests {
        let host = request.headers.get("host").cloned().unwrap_or_default();
        let names = cookie_names(&request.headers);
        if host.starts_with("127.0.0.1") {
            saw_ipv4 = true;
            assert_eq!(
                names,
                ipv4_names,
                "request to {} sent cookies {:?}",
                request.path,
                request.headers.get("cookie")
            );
        } else if host.starts_with("localhost") {
            saw_localhost = true;
            assert_eq!(
                names,
                localhost_names,
                "request to {} sent cookies {:?}",
                request.path,
                request.headers.get("cookie")
            );
        } else {
            panic!("unexpected fixture host {host}");
        }
    }
    assert!(saw_ipv4, "the fixture must record the 127.0.0.1 requests");
    assert!(
        saw_localhost,
        "the fixture must record the localhost requests"
    );

    let dump = sqlite_text_dump(&store_dir.path().join("tasks.db"));
    let logs = read_log_files(&logger);
    for secret in [HOST_ONLY, PATH_OK, OTHER_HOST, PATH_WRONG, EXPIRED, SECURE] {
        assert!(
            !dump.contains(secret),
            "cookie value {secret} reached the store"
        );
        assert!(
            !logs.contains(secret),
            "cookie value {secret} reached the logs"
        );
    }
}

#[tokio::test]
async fn a_cross_host_redirect_does_not_forward_the_original_host_cookie() {
    const SOURCE_COOKIE: &str = "oneget-source-cookie-secret";
    const TARGET_COOKIE: &str = "oneget-target-cookie-secret";
    let target_body = non_periodic_bytes(512 * 1024);
    let target_hash = sha256_hex(&target_body);
    let target = RangeServer::start(FixtureConfig {
        body: target_body.clone(),
        ..Default::default()
    });
    let source = RangeServer::start(FixtureConfig {
        body: Vec::new(),
        redirect_location: Some(target.host_url("localhost", "/final/payload.bin")),
        redirect_status: Some(302),
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let log_dir = tempdir().expect("logs");
    let cookie_dir = tempdir().expect("cookies");
    let logger = debug_logger(log_dir.path());
    let cookie_path = write_cookie_file(
        cookie_dir.path(),
        &[
            NetscapeCookie {
                domain: "127.0.0.1".to_string(),
                include_subdomains: false,
                path: "/".to_string(),
                secure: false,
                expires: future_expiry(),
                name: "source_cookie".to_string(),
                value: SOURCE_COOKIE.to_string(),
            },
            NetscapeCookie {
                domain: "localhost".to_string(),
                include_subdomains: false,
                path: "/".to_string(),
                secure: false,
                expires: future_expiry(),
                name: "target_cookie".to_string(),
                value: TARGET_COOKIE.to_string(),
            },
        ],
    );
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    engine.set_cookie_file(Some(cookie_path));
    engine.set_logger(Arc::clone(&logger));

    let id = engine
        .add_task(make_spec(
            source.url("/start/payload.bin"),
            "cross-host.bin",
            1,
            Some(target_hash.clone()),
        ))
        .expect("add task");
    run_engine_to_terminal(&engine, engine.store(), &id).await;
    logger.flush().expect("flush logs");

    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(
        task.state,
        TaskState::Completed,
        "cross-host redirect failed with {:?}: {:?}",
        task.error_code,
        task.error_message
    );
    assert_eq!(task.actual_sha256.as_deref(), Some(target_hash.as_str()));
    assert_eq!(
        std::fs::read(dest.path().join("cross-host.bin")).expect("read final"),
        target_body
    );

    let source_requests = source.requests();
    assert!(!source_requests.is_empty());
    for request in &source_requests {
        assert_eq!(
            cookie_value(&request.headers, "source_cookie").as_deref(),
            Some(SOURCE_COOKIE),
            "the original host must receive its own cookie"
        );
    }

    let target_requests = target.requests();
    assert!(!target_requests.is_empty());
    for request in &target_requests {
        assert!(
            cookie_value(&request.headers, "source_cookie").is_none(),
            "the redirected host must not receive the original host cookie"
        );
        let raw_cookie = request.headers.get("cookie").cloned().unwrap_or_default();
        assert!(
            !raw_cookie.contains(SOURCE_COOKIE),
            "the redirected host cookie header leaked the original value"
        );
    }

    let dump = sqlite_text_dump(&store_dir.path().join("tasks.db"));
    let logs = read_log_files(&logger);
    for secret in [SOURCE_COOKIE, TARGET_COOKIE] {
        assert!(
            !dump.contains(secret),
            "cookie value {secret} reached the store"
        );
        assert!(
            !logs.contains(secret),
            "cookie value {secret} reached the logs"
        );
    }
}

#[tokio::test]
async fn forbidden_and_newline_headers_are_rejected_before_any_request() {
    let fixture = RangeServer::start(FixtureConfig {
        body: non_periodic_bytes(64 * 1024),
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    for &header in FORBIDDEN_HEADERS {
        let error = engine
            .add_task(make_spec_with_headers(
                fixture.url("/header/rejected.bin"),
                "rejected.bin",
                1,
                None,
                vec![(header.to_string(), "oneget-forbidden-value".to_string())],
            ))
            .expect_err("a forbidden header must be rejected");
        assert_eq!(
            error.code(),
            "INVALID_HEADER",
            "header {header} must return INVALID_HEADER, got {}: {}",
            error.code(),
            error
        );
    }

    for header in ["authorization", "cookie", "proxy-authorization", "range"] {
        let error = engine
            .add_task(make_spec_with_headers(
                fixture.url("/header/rejected-lowercase.bin"),
                "rejected-lowercase.bin",
                1,
                None,
                vec![(header.to_string(), "oneget-forbidden-value".to_string())],
            ))
            .expect_err("a lowercase forbidden header must be rejected");
        assert_eq!(
            error.code(),
            "INVALID_HEADER",
            "header {header} must return INVALID_HEADER"
        );
    }

    for value in [
        "oneget\r\nHost: evil.example",
        "oneget\nHost: evil.example",
        "oneget\rHost: evil.example",
    ] {
        let error = engine
            .add_task(make_spec_with_headers(
                fixture.url("/header/newline.bin"),
                "newline.bin",
                1,
                None,
                vec![("X-Custom-Trace".to_string(), value.to_string())],
            ))
            .expect_err("a CR or LF header value must be rejected");
        assert_eq!(
            error.code(),
            "INVALID_HEADER",
            "a newline-bearing value must return INVALID_HEADER"
        );
    }

    let error = engine
        .add_task(make_spec_with_headers(
            fixture.url("/header/injection.bin"),
            "injection.bin",
            1,
            None,
            vec![(
                "X-Custom-Trace\r\nHost: evil.example".to_string(),
                "oneget".to_string(),
            )],
        ))
        .expect_err("a CRLF header name must be rejected");
    assert_eq!(error.code(), "INVALID_HEADER");

    assert!(
        fixture.requests().is_empty(),
        "rejected headers must not produce any request"
    );
    assert!(
        engine.store().list_tasks().expect("list tasks").is_empty(),
        "rejected headers must not create a task"
    );
}

#[tokio::test]
async fn safe_header_reaches_the_fixture_but_is_not_persisted_for_replay() {
    let body = non_periodic_bytes(512 * 1024);
    let hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body: body.clone(),
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let log_dir = tempdir().expect("logs");
    let db_path = store_dir.path().join("tasks.db");
    let logger = debug_logger(log_dir.path());

    let (id, served_requests) = {
        let engine = Engine::new(
            Database::open(&db_path).expect("open store"),
            dest.path().to_path_buf(),
            EngineOptions { max_concurrent: 1 },
        );
        engine.set_logger(Arc::clone(&logger));
        let id = engine
            .add_task(make_spec_with_headers(
                fixture.url("/header/safe.bin"),
                "safe-header.bin",
                1,
                Some(hash.clone()),
                vec![(SAFE_HEADER_NAME.to_string(), SAFE_HEADER_VALUE.to_string())],
            ))
            .expect("add safe-header task");

        run_engine_to_terminal(&engine, engine.store(), &id).await;
        logger.flush().expect("flush logs");

        let task = engine.store().task(&id).expect("load task").expect("task");
        assert_eq!(
            task.state,
            TaskState::Completed,
            "safe-header download failed with {:?}: {:?}",
            task.error_code,
            task.error_message
        );
        assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));

        let requests = fixture.requests();
        assert!(!requests.is_empty());
        for request in &requests {
            assert_eq!(
                request.headers.get("x-custom-trace").map(String::as_str),
                Some(SAFE_HEADER_VALUE),
                "the safe header must reach every fixture request while the task is live"
            );
        }
        (id, requests.len())
    };

    let dump = sqlite_text_dump(&db_path);
    assert!(
        !dump.contains(SAFE_HEADER_VALUE),
        "the SQLite store must not keep a custom header value for recovery"
    );
    // Persisting the safe header name with a redacted or empty value is
    // acceptable so a fresh engine knows which safe headers were configured,
    // but the value must never be replayable. Dropping headers entirely is
    // also privacy-correct, so the name is not required.
    let logs = read_log_files(&logger);
    assert!(
        !logs.contains(SAFE_HEADER_VALUE),
        "a header value must not be written to the log"
    );

    let fresh_engine = Engine::new(
        Database::open(&db_path).expect("reopen store"),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    let tasks = fresh_engine.store().list_tasks().expect("list tasks");
    assert_eq!(tasks.len(), 1, "exactly one safe task must be persisted");
    let fresh_task = &tasks[0];
    assert_eq!(fresh_task.id, id);
    assert!(
        !format!("{fresh_task:?}").contains(SAFE_HEADER_VALUE),
        "a fresh engine must not expose a persisted header value"
    );

    timeout(WAIT_TIMEOUT, fresh_engine.run())
        .await
        .expect("fresh engine run timed out")
        .expect("fresh engine run failed");
    assert_eq!(
        fixture.requests().len(),
        served_requests,
        "a fresh engine must not replay a completed task's custom header"
    );
    let fresh_dump = sqlite_text_dump(&db_path);
    assert!(
        !fresh_dump.contains(SAFE_HEADER_VALUE),
        "reopening the store must not reveal a persisted header value"
    );
}

// ---------------------------------------------------------------------------
// Step 2 behavior slice: proxy resolution, authenticated transport, system
// proxy injection, and credential clearing.
//
// These tests exercise the public boundary only:
// `oneget_lib::platform::proxy` (system source discovery and
// `resolve_proxy`), `oneget_lib::engine::http::{validate_proxy_url,
// ProxyConfig}`, the engine scheduler, the loopback API server, and the
// settings/secret commands. The HTTP fixture proxy cases already present in
// this file cover the manual HTTP proxy behavior; HTTP-407 and SOCKS5 cases
// are extended here.
// ---------------------------------------------------------------------------

#[path = "support/socks5_server.rs"]
mod socks5_server;

use oneget_lib::api::clock::SystemClock;
use oneget_lib::api::desktop::{DesktopBridge, PendingPrefill};
use oneget_lib::api::error::ApiError;
use oneget_lib::api::pairing_store::PairingStore;
use oneget_lib::api::protocol::CaptureItem;
use oneget_lib::api::rate_limit::RateLimitPolicy;
use oneget_lib::api::server::{ApiServerConfig, ApiServerHandle};
use oneget_lib::app_state::AppState;
use oneget_lib::commands::app::{
    proxy_credentials_save, proxy_credentials_status, settings_save, AutostartService,
    NotificationSendResult, NotificationService, NotificationStatus, QuitService,
    SecretRoundTripResult, SecretService, SecretStatus,
};
use oneget_lib::config::{ProxyMode, ProxyType, Settings, SettingsStore};
use oneget_lib::platform::paths::AppPaths;
use oneget_lib::platform::secrets::{MemorySecretStore, SecretError, SecretStore, SecretValue};
use serde_json::{json, Value};
use socks5_server::{Socks5Credentials, Socks5Server};

fn manual_proxy_settings(proxy_type: ProxyType, host: &str, port: u16) -> Settings {
    Settings {
        proxy_mode: ProxyMode::Manual,
        proxy_type,
        proxy_host: Some(host.to_string()),
        proxy_port: Some(port),
        ..Settings::default()
    }
}

#[tokio::test]
async fn socks5_proxy_with_correct_credentials_completes_and_records_the_tunnel() {
    const SOCKS_USER: &str = "oneget-socks-user";
    const SOCKS_PASSWORD: &str = "oneget-socks-password-secret";
    let body = non_periodic_bytes(256 * 1024);
    let hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body: body.clone(),
        ..Default::default()
    });
    let proxy = Socks5Server::start(Some(Socks5Credentials {
        username: SOCKS_USER.to_string(),
        password: SOCKS_PASSWORD.to_string(),
    }));
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    engine
        .set_proxy(Some(ProxyConfig {
            url: proxy.url(),
            username: Some(SOCKS_USER.to_string()),
            password: Some(SOCKS_PASSWORD.to_string()),
        }))
        .expect("configure the authenticated SOCKS5 proxy");

    let id = engine
        .add_task(make_spec(
            fixture.url("/socks5/payload.bin"),
            "socks5-payload.bin",
            2,
            Some(hash.clone()),
        ))
        .expect("add task");

    run_engine_to_terminal(&engine, engine.store(), &id).await;

    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(
        task.state,
        TaskState::Completed,
        "the SOCKS5 download must complete, observed {:?}: {:?}",
        task.error_code,
        task.error_message
    );
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));
    assert_eq!(
        std::fs::read(dest.path().join("socks5-payload.bin")).expect("read final"),
        body,
        "the SOCKS5 download must deliver the fixture bytes"
    );

    let origin_authority = fixture
        .url("")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();
    let tunnels = proxy.tunnels();
    assert!(
        tunnels.iter().any(|tunnel| tunnel.authenticated
            && tunnel.username.as_deref() == Some(SOCKS_USER)
            && tunnel.target.as_deref() == Some(origin_authority.as_str())),
        "the fixture must record an authenticated tunnel to {origin_authority} with the configured username, got {tunnels:?}"
    );
    for request in fixture.requests() {
        assert_eq!(
            request
                .headers
                .keys()
                .filter(|name| name.eq_ignore_ascii_case("proxy-authorization"))
                .count(),
            0,
            "the origin must never see the SOCKS5 credential"
        );
    }
}

#[tokio::test]
async fn socks5_proxy_with_wrong_password_fails_with_a_typed_error_and_no_final_file() {
    const SOCKS_USER: &str = "oneget-socks-user";
    const GOOD_PASSWORD: &str = "oneget-socks-good-secret";
    const WRONG_PASSWORD: &str = "oneget-socks-wrong-secret";
    let body = non_periodic_bytes(128 * 1024);
    let fixture = RangeServer::start(FixtureConfig {
        body,
        ..Default::default()
    });
    let proxy = Socks5Server::start(Some(Socks5Credentials {
        username: SOCKS_USER.to_string(),
        password: GOOD_PASSWORD.to_string(),
    }));
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    engine
        .set_proxy(Some(ProxyConfig {
            url: proxy.url(),
            username: Some(SOCKS_USER.to_string()),
            password: Some(WRONG_PASSWORD.to_string()),
        }))
        .expect("configure the SOCKS5 proxy with a wrong password");

    let id = engine
        .add_task(make_spec(
            fixture.url("/socks5/wrong-password.bin"),
            "socks5-wrong-password.bin",
            1,
            None,
        ))
        .expect("add task");

    run_engine_to_terminal(&engine, engine.store(), &id).await;

    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(
        task.state,
        TaskState::Failed,
        "a rejected SOCKS5 authentication must fail the task, observed {:?}",
        task.state
    );
    let code = task
        .error_code
        .clone()
        .expect("a failed SOCKS5 authentication must carry a typed error code");
    assert!(!code.is_empty(), "the typed error code must not be empty");
    assert!(
        !dest.path().join("socks5-wrong-password.bin").exists(),
        "a rejected SOCKS5 authentication must not create a final file"
    );

    let tunnels = proxy.tunnels();
    assert!(
        tunnels.iter().any(|tunnel| !tunnel.authenticated),
        "the fixture must record the rejected authentication attempt, got {tunnels:?}"
    );
    assert!(
        fixture.requests().is_empty(),
        "the origin must not receive a request rejected by the SOCKS5 fixture"
    );
}

#[test]
fn validate_proxy_url_accepts_socks5_and_still_rejects_embedded_userinfo() {
    let accepted = oneget_lib::engine::http::validate_proxy_url("socks5://127.0.0.1:1080");
    assert!(
        accepted.is_ok(),
        "socks5 proxies must be accepted, got {accepted:?}"
    );
    for input in [
        "socks5://user:pass@127.0.0.1:1080",
        "http://user:pass@127.0.0.1:8080",
    ] {
        let error = oneget_lib::engine::http::validate_proxy_url(input)
            .expect_err("a proxy URL with embedded userinfo must be rejected");
        assert_eq!(
            error.code(),
            "INVALID_PROXY_URL",
            "expected INVALID_PROXY_URL for {input}, got {}",
            error.code()
        );
    }
}

/// Seed data for the fake system proxy source. The fixture rebuilds a fresh
/// `SystemProxyDiscovery` on every call, so the fake never depends on any
/// trait bound of the real discovery enum.
#[derive(Clone)]
enum FakeDiscovery {
    Proxy {
        http: Option<String>,
        https: Option<String>,
    },
    Unavailable {
        reason: String,
    },
}

struct FakeSystemProxy {
    discovery: FakeDiscovery,
}

impl oneget_lib::platform::proxy::SystemProxySource for FakeSystemProxy {
    fn discover(&self) -> oneget_lib::platform::proxy::SystemProxyDiscovery {
        match &self.discovery {
            FakeDiscovery::Proxy { http, https } => {
                oneget_lib::platform::proxy::SystemProxyDiscovery::Proxy {
                    http: http.clone(),
                    https: https.clone(),
                }
            }
            FakeDiscovery::Unavailable { reason } => {
                oneget_lib::platform::proxy::SystemProxyDiscovery::Unavailable {
                    reason: reason.clone(),
                }
            }
        }
    }
}

#[test]
fn direct_mode_resolves_direct_even_when_a_system_proxy_exists_and_credentials_are_present() {
    let settings = Settings::default(); // proxy mode: direct
    let source = FakeSystemProxy {
        discovery: FakeDiscovery::Proxy {
            http: Some("http://127.0.0.1:18080".to_string()),
            https: Some("https://127.0.0.1:18443".to_string()),
        },
    };
    let resolution = oneget_lib::platform::proxy::resolve_proxy(
        &settings,
        Some(("oneget-user", "oneget-pass")),
        &source,
    );
    assert_eq!(
        resolution,
        oneget_lib::platform::proxy::ProxyResolution::Direct,
        "direct mode must resolve to Direct and ignore the system proxy"
    );
}

#[test]
fn manual_mode_resolves_host_port_and_credentials_for_each_protocol() {
    for (proxy_type, scheme) in [
        (ProxyType::Http, "http"),
        (ProxyType::Https, "https"),
        (ProxyType::Socks5, "socks5"),
    ] {
        let settings = manual_proxy_settings(proxy_type, "127.0.0.1", 1080);
        let source = FakeSystemProxy {
            discovery: FakeDiscovery::Unavailable {
                reason: "unused-in-manual-mode".to_string(),
            },
        };
        let resolution = oneget_lib::platform::proxy::resolve_proxy(
            &settings,
            Some(("oneget-proxy-user", "oneget-proxy-pass")),
            &source,
        );
        assert_eq!(
            resolution,
            oneget_lib::platform::proxy::ProxyResolution::Proxy(
                oneget_lib::platform::proxy::ResolvedProxy {
                    url: format!("{scheme}://127.0.0.1:1080"),
                    username: Some("oneget-proxy-user".to_string()),
                    password: Some("oneget-proxy-pass".to_string()),
                }
            ),
            "manual {scheme} mode must resolve host, port, and credentials"
        );
    }
}

#[test]
fn system_mode_prefers_the_https_value_else_the_http_value_and_never_attaches_credentials() {
    let settings = Settings {
        proxy_mode: ProxyMode::System,
        ..Settings::default()
    };

    let http_only = FakeSystemProxy {
        discovery: FakeDiscovery::Proxy {
            http: Some("http://127.0.0.1:18080".to_string()),
            https: None,
        },
    };
    let resolution = oneget_lib::platform::proxy::resolve_proxy(
        &settings,
        Some(("oneget-user", "oneget-pass")),
        &http_only,
    );
    assert_eq!(
        resolution,
        oneget_lib::platform::proxy::ProxyResolution::Proxy(
            oneget_lib::platform::proxy::ResolvedProxy {
                url: "http://127.0.0.1:18080".to_string(),
                username: None,
                password: None,
            }
        ),
        "a system proxy without an HTTPS value must fall back to the HTTP value and carry no credentials"
    );

    let both = FakeSystemProxy {
        discovery: FakeDiscovery::Proxy {
            http: Some("http://127.0.0.1:18080".to_string()),
            https: Some("https://127.0.0.1:18443".to_string()),
        },
    };
    let resolution = oneget_lib::platform::proxy::resolve_proxy(
        &settings,
        Some(("oneget-user", "oneget-pass")),
        &both,
    );
    assert_eq!(
        resolution,
        oneget_lib::platform::proxy::ProxyResolution::Proxy(
            oneget_lib::platform::proxy::ResolvedProxy {
                url: "https://127.0.0.1:18443".to_string(),
                username: None,
                password: None,
            }
        ),
        "a system proxy with an HTTPS value must prefer it and carry no credentials"
    );
}

#[test]
fn system_mode_unavailable_reports_the_reason_and_is_never_direct_or_a_proxy() {
    let settings = Settings {
        proxy_mode: ProxyMode::System,
        ..Settings::default()
    };
    let source = FakeSystemProxy {
        discovery: FakeDiscovery::Unavailable {
            reason: "pac_only".to_string(),
        },
    };
    let resolution = oneget_lib::platform::proxy::resolve_proxy(&settings, None, &source);
    assert_eq!(
        resolution,
        oneget_lib::platform::proxy::ProxyResolution::Unavailable {
            reason: "pac_only".to_string()
        },
        "a PAC-only configuration must report Unavailable with the source reason"
    );
    assert!(
        !matches!(
            resolution,
            oneget_lib::platform::proxy::ProxyResolution::Direct
        ),
        "an unavailable system proxy must never silently resolve Direct"
    );
}

#[tokio::test]
async fn a_resolved_system_proxy_completes_the_download_through_the_fixture() {
    let body = non_periodic_bytes(192 * 1024);
    let hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body: body.clone(),
        ..Default::default()
    });
    let proxy = ProxyServer::start(None);
    let settings = Settings {
        proxy_mode: ProxyMode::System,
        ..Settings::default()
    };
    let source = FakeSystemProxy {
        discovery: FakeDiscovery::Proxy {
            http: Some(proxy.url()),
            https: None,
        },
    };
    let resolution = oneget_lib::platform::proxy::resolve_proxy(&settings, None, &source);
    let resolved = match resolution {
        oneget_lib::platform::proxy::ProxyResolution::Proxy(resolved) => resolved,
        other => panic!("a configured system proxy must resolve to a proxy, got {other:?}"),
    };
    assert_eq!(
        resolved.username, None,
        "a system proxy carries no credentials"
    );
    assert_eq!(
        resolved.password, None,
        "a system proxy carries no credentials"
    );

    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    engine
        .set_proxy(Some(ProxyConfig {
            url: resolved.url,
            username: resolved.username,
            password: resolved.password,
        }))
        .expect("apply the resolved system proxy");

    let id = engine
        .add_task(make_spec(
            fixture.url("/system/payload.bin"),
            "system-payload.bin",
            2,
            Some(hash.clone()),
        ))
        .expect("add task");

    run_engine_to_terminal(&engine, engine.store(), &id).await;

    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(
        task.state,
        TaskState::Completed,
        "a download through the resolved system proxy must complete, observed {:?}: {:?}",
        task.error_code,
        task.error_message
    );
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));

    let origin_prefix = fixture.url("");
    let requests = proxy.requests();
    assert!(
        !requests.is_empty(),
        "the system proxy fixture must observe the download"
    );
    for request in &requests {
        assert!(
            request.target.starts_with(&origin_prefix),
            "the system proxy forwarded to an unexpected destination: {}",
            request.target
        );
    }
}

#[tokio::test]
async fn direct_mode_sends_no_proxy_request() {
    let body = non_periodic_bytes(128 * 1024);
    let hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body,
        ..Default::default()
    });
    let proxy = ProxyServer::start(None);
    let settings = Settings::default(); // proxy mode: direct
    let source = FakeSystemProxy {
        discovery: FakeDiscovery::Proxy {
            http: Some(proxy.url()),
            https: None,
        },
    };
    let resolution = oneget_lib::platform::proxy::resolve_proxy(&settings, None, &source);
    assert_eq!(
        resolution,
        oneget_lib::platform::proxy::ProxyResolution::Direct,
        "direct mode must resolve Direct and must not produce a proxy configuration"
    );

    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    // Direct resolution means no proxy is applied to the engine.
    let id = engine
        .add_task(make_spec(
            fixture.url("/direct/payload.bin"),
            "direct-payload.bin",
            1,
            Some(hash),
        ))
        .expect("add task");

    run_engine_to_terminal(&engine, engine.store(), &id).await;

    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(
        task.state,
        TaskState::Completed,
        "a direct download must complete, observed {:?}",
        task.error_code
    );
    assert!(
        proxy.requests().is_empty(),
        "a direct task must not issue a request through any proxy"
    );
}

// --- Credential clearing through settings_save ---

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
    fn roundtrip(
        &self,
        _secret: &str,
    ) -> Result<SecretRoundTripResult, oneget_lib::error::AppError> {
        Ok(SecretRoundTripResult { success: true })
    }

    fn get_secret(&self, key: &str) -> Result<Option<SecretValue>, oneget_lib::error::AppError> {
        self.inner.get(key).map_err(map_secret_error)
    }

    fn set_secret(
        &self,
        key: &str,
        value: &SecretValue,
    ) -> Result<(), oneget_lib::error::AppError> {
        self.inner.set(key, value).map_err(map_secret_error)
    }

    fn delete_secret(&self, key: &str) -> Result<(), oneget_lib::error::AppError> {
        self.inner.delete(key).map_err(map_secret_error)
    }
}

fn map_secret_error(error: SecretError) -> oneget_lib::error::AppError {
    oneget_lib::error::AppError::new(error.code, error.message)
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

struct StateHarness {
    _temp: tempfile::TempDir,
    state: AppState,
}

fn build_harness() -> StateHarness {
    let temp = tempdir().expect("tempdir");
    let paths = AppPaths::with_root(temp.path().join("app")).expect("app paths");
    let settings_path = paths.data.join("settings.json");
    let settings = SettingsStore::load(&settings_path).expect("settings store");
    let logger = Logger::new(LogConfig::with_defaults(paths.logs.clone())).expect("logger");
    let state = AppState::new(
        paths,
        settings,
        logger,
        std::sync::Arc::new(MemorySecrets::new()),
        std::sync::Arc::new(NoopNotifications),
        std::sync::Arc::new(NoopAutostart),
        std::sync::Arc::new(NoopQuit),
    );
    StateHarness { _temp: temp, state }
}

fn assert_single_boolean_status(status: SecretStatus, configured: bool) {
    assert_eq!(status.configured, configured);
    let serialized = serde_json::to_value(status).expect("status must serialize");
    let object = serialized
        .as_object()
        .expect("status must serialize as an object");
    assert_eq!(
        object.len(),
        1,
        "the secret status must expose exactly one key, got {object:?}"
    );
    assert_eq!(object.get("configured"), Some(&json!(configured)));
}

fn save_proxy_credentials(harness: &StateHarness) {
    let saved = proxy_credentials_save(
        &harness.state,
        "oneget-proxy-user".to_string(),
        "oneget-proxy-pass-secret".to_string(),
    )
    .expect("save proxy credentials");
    assert_single_boolean_status(saved, true);
    let status = proxy_credentials_status(&harness.state).expect("status after save");
    assert_single_boolean_status(status, true);
}

#[test]
fn saving_direct_mode_deletes_the_stored_proxy_credentials() {
    let harness = build_harness();

    assert_single_boolean_status(
        proxy_credentials_status(&harness.state).expect("initial status"),
        false,
    );
    save_proxy_credentials(&harness);

    let settings = Settings {
        proxy_mode: ProxyMode::Direct,
        ..harness.state.settings()
    };
    settings_save(&harness.state, settings).expect("save direct mode");

    assert_single_boolean_status(
        proxy_credentials_status(&harness.state).expect("status after direct save"),
        false,
    );
}

#[test]
fn saving_system_mode_deletes_the_stored_proxy_credentials() {
    let harness = build_harness();

    assert_single_boolean_status(
        proxy_credentials_status(&harness.state).expect("initial status"),
        false,
    );
    save_proxy_credentials(&harness);

    let settings = Settings {
        proxy_mode: ProxyMode::System,
        ..harness.state.settings()
    };
    settings_save(&harness.state, settings).expect("save system mode");

    assert_single_boolean_status(
        proxy_credentials_status(&harness.state).expect("status after system save"),
        false,
    );
}

#[test]
fn saving_manual_mode_preserves_the_stored_proxy_credentials() {
    let harness = build_harness();

    save_proxy_credentials(&harness);

    let settings = manual_proxy_settings(ProxyType::Http, "127.0.0.1", 8080);
    settings_save(&harness.state, settings).expect("save manual mode");

    assert_single_boolean_status(
        proxy_credentials_status(&harness.state).expect("status after manual save"),
        true,
    );
}

// --- Loopback API never uses the configured manual proxy ---

struct FakeBridge;

impl DesktopBridge for FakeBridge {
    fn create_takeover_task(&self, _item: &CaptureItem, _now_ms: i64) -> Result<String, ApiError> {
        Ok("task-fixture".to_string())
    }

    fn queue_prefill(&self, _prefill: PendingPrefill) -> Result<(), ApiError> {
        Ok(())
    }

    fn focus_main_window(&self) {}

    fn log_event(&self, _event: &str, _fields: Value) {}
}

fn raw_loopback_http(port: u16, request: &str) -> String {
    use std::io::{Read, Write};

    let mut stream =
        std::net::TcpStream::connect(("127.0.0.1", port)).expect("connect to the loopback API");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read timeout");
    stream.write_all(request.as_bytes()).expect("write request");
    let mut response = String::new();
    stream.read_to_string(&mut response).expect("read response");
    response
}

#[test]
fn loopback_api_traffic_never_passes_through_the_configured_manual_proxy() {
    let proxy = ProxyServer::start(None);
    let harness = build_harness();
    let proxy_port: u16 = proxy
        .url()
        .rsplit(':')
        .next()
        .expect("proxy port")
        .parse()
        .expect("proxy port number");
    let settings = manual_proxy_settings(ProxyType::Http, "127.0.0.1", proxy_port);
    settings_save(&harness.state, settings).expect("save manual proxy settings");

    let pairing_temp = tempdir().expect("pairing tempdir");
    let store = std::sync::Arc::new(std::sync::Mutex::new(
        PairingStore::open(pairing_temp.path().join("pairing.db")).expect("open pairing store"),
    ));
    let mut handle = ApiServerHandle::start(ApiServerConfig {
        port: 0,
        app_version: "oneget-test".to_string(),
        pairing_store: store,
        bridge: std::sync::Arc::new(FakeBridge),
        clock: std::sync::Arc::new(SystemClock::default()),
        pair_origin_policy: RateLimitPolicy::new(10, 60_000),
        token_policy: RateLimitPolicy::new(10, 60_000),
    })
    .expect("start the loopback API on an ephemeral port");

    let port = handle.port();
    let response = raw_loopback_http(
        port,
        &format!("GET /health HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"),
    );
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "the loopback API must answer /health directly, got: {response}"
    );

    handle.shutdown();
    assert!(
        proxy.requests().is_empty(),
        "loopback API traffic must not pass through the configured manual proxy, observed {:?}",
        proxy.requests()
    );
}

// ---------------------------------------------------------------------------
// Step 3 behavior tests (expected RED): global User-Agent, timeouts, retries.
// ---------------------------------------------------------------------------

#[test]
fn network_options_default_contract_is_exact() {
    let options = NetworkOptions::default();
    assert_eq!(options.user_agent, None);
    assert_eq!(options.connect_timeout, Duration::from_secs(15));
    assert_eq!(options.read_timeout, Duration::from_secs(30));
    assert_eq!(options.retry_attempts, 5);
}

#[tokio::test]
async fn global_user_agent_is_fallback_for_tasks_without_their_own_user_agent() {
    const GLOBAL_UA: &str = "OneGet-Global/1.0";
    const TASK_UA: &str = "OneGet-Task/1.0";

    let body_first = non_periodic_bytes(256 * 1024);
    let hash_first = sha256_hex(&body_first);
    let fixture_first = RangeServer::start(FixtureConfig {
        body: body_first.clone(),
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let body_second = non_periodic_bytes(192 * 1024);
    let hash_second = sha256_hex(&body_second);
    let fixture_second = RangeServer::start(FixtureConfig {
        body: body_second.clone(),
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });

    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    engine.set_network_options(NetworkOptions {
        user_agent: Some(GLOBAL_UA.to_string()),
        ..NetworkOptions::default()
    });

    let first_id = engine
        .add_task(make_spec_with_headers(
            fixture_first.url("/ua/explicit.bin"),
            "ua-explicit.bin",
            1,
            Some(hash_first.clone()),
            vec![("User-Agent".to_string(), TASK_UA.to_string())],
        ))
        .expect("add first task");
    run_engine_to_terminal(&engine, engine.store(), &first_id).await;

    let second_id = engine
        .add_task(make_spec(
            fixture_second.url("/ua/global.bin"),
            "ua-global.bin",
            1,
            Some(hash_second.clone()),
        ))
        .expect("add second task");
    run_engine_to_terminal(&engine, engine.store(), &second_id).await;

    let first_task = engine.store().task(&first_id).expect("load").expect("task");
    assert_eq!(
        first_task.state,
        TaskState::Completed,
        "explicit-UA task failed with {:?}: {:?}",
        first_task.error_code,
        first_task.error_message
    );
    let second_task = engine
        .store()
        .task(&second_id)
        .expect("load")
        .expect("task");
    assert_eq!(
        second_task.state,
        TaskState::Completed,
        "global-UA task failed with {:?}: {:?}",
        second_task.error_code,
        second_task.error_message
    );

    let first_requests = fixture_first.requests();
    assert!(
        !first_requests.is_empty(),
        "the explicit-UA fixture must observe requests"
    );
    for request in &first_requests {
        assert_eq!(
            request.headers.get("user-agent").map(String::as_str),
            Some(TASK_UA),
            "a task with its own User-Agent must send that value, request={request:?}"
        );
    }

    let second_requests = fixture_second.requests();
    assert!(
        !second_requests.is_empty(),
        "the global-UA fixture must observe requests"
    );
    for request in &second_requests {
        assert_eq!(
            request.headers.get("user-agent").map(String::as_str),
            Some(GLOBAL_UA),
            "a task without its own User-Agent must send the global value, request={request:?}"
        );
    }
}

#[tokio::test]
async fn stalled_download_respects_configured_read_timeout_and_fails_typed() {
    let fixture = RangeServer::start(FixtureConfig {
        body: non_periodic_bytes(256 * 1024),
        stall_before_response: Some(Duration::from_secs(3)),
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    engine.set_network_options(NetworkOptions {
        read_timeout: Duration::from_millis(150),
        retry_attempts: 1,
        ..NetworkOptions::default()
    });

    let id = engine
        .add_task(make_spec(
            fixture.url("/timeout/stall.bin"),
            "stall-timeout.bin",
            1,
            None,
        ))
        .expect("add task");

    let started = Instant::now();
    let ((), observed) = tokio::join!(
        async {
            timeout(Duration::from_secs(5), engine.run())
                .await
                .expect("engine run must finish within the bounded wall-clock window")
                .expect("engine run failed");
        },
        wait_for_terminal(engine.store(), &id, Duration::from_secs(5)),
    );
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "a 150ms read timeout must fail well within the bounded window, elapsed {elapsed:?}"
    );
    assert_eq!(
        observed,
        TaskState::Failed,
        "a stalled task must fail, observed {observed:?}"
    );
    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(
        task.error_code.as_deref(),
        Some("TIMEOUT"),
        "expected typed TIMEOUT, got {:?}: {:?}",
        task.error_code,
        task.error_message
    );
    assert!(
        !dest.path().join("stall-timeout.bin").exists(),
        "a timed-out task must not create a final file"
    );
}

#[derive(Clone, Debug)]
struct RetryBoundRequest {
    range: Option<String>,
}

struct RetryBoundServer {
    url: String,
    requests: std::sync::Arc<std::sync::Mutex<Vec<RetryBoundRequest>>>,
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl RetryBoundServer {
    fn start(body: Vec<u8>) -> Self {
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::{Arc, Mutex};

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind retry-bound server");
        let addr = listener.local_addr().expect("addr");
        listener
            .set_nonblocking(true)
            .expect("set nonblocking listener");
        let requests: Arc<Mutex<Vec<RetryBoundRequest>>> = Arc::new(Mutex::new(Vec::new()));
        let requests_clone = Arc::clone(&requests);
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = Arc::clone(&shutdown);
        let body = Arc::new(body);
        let data_failures = Arc::new(AtomicUsize::new(0));

        let join = std::thread::spawn(move || loop {
            if shutdown_clone.load(Ordering::SeqCst) {
                break;
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let body = Arc::clone(&body);
                    let requests = Arc::clone(&requests_clone);
                    let failures = Arc::clone(&data_failures);
                    std::thread::spawn(move || {
                        stream.set_read_timeout(Some(Duration::from_secs(2))).ok();
                        let _ = stream.set_nonblocking(false);
                        let mut buf = Vec::new();
                        let mut tmp = [0u8; 4096];
                        loop {
                            match stream.read(&mut tmp) {
                                Ok(0) => break,
                                Ok(n) => {
                                    buf.extend_from_slice(&tmp[..n]);
                                    if buf.windows(4).any(|w| w == b"\r\n\r\n")
                                        || buf.len() > 64 * 1024
                                    {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }
                        let req_text = String::from_utf8_lossy(&buf);
                        let mut range: Option<String> = None;
                        for line in req_text.split("\r\n").skip(1) {
                            if line.is_empty() {
                                break;
                            }
                            if let Some((k, v)) = line.split_once(':') {
                                if k.trim().eq_ignore_ascii_case("range") {
                                    range = Some(v.trim().to_string());
                                }
                            }
                        }
                        requests
                            .lock()
                            .expect("lock requests")
                            .push(RetryBoundRequest {
                                range: range.clone(),
                            });

                        let total = body.len() as u64;
                        if range.as_deref() == Some("bytes=0-0") {
                            let resp = format!(
                                "HTTP/1.1 206 Partial Content\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes 0-0/{total}\r\nContent-Length: 1\r\nConnection: close\r\n\r\n"
                            );
                            let _ = stream.write_all(resp.as_bytes());
                            if !body.is_empty() {
                                let _ = stream.write_all(&body[0..1]);
                            }
                            return;
                        }

                        let is_data_attempt = range.is_some();
                        if is_data_attempt {
                            let seen = failures.fetch_add(1, Ordering::SeqCst) + 1;
                            if seen <= 2 {
                                let msg = b"temporarily unavailable";
                                let resp = format!(
                                    "HTTP/1.1 503 Service Unavailable\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                    msg.len()
                                );
                                let _ = stream.write_all(resp.as_bytes());
                                let _ = stream.write_all(msg);
                                return;
                            }
                        }

                        // Serve the requested range (or full body) successfully.
                        let (start, end) = match range.as_deref() {
                            Some(r) if r.starts_with("bytes=") => {
                                let spec = r.trim_start_matches("bytes=");
                                if let Some((s, e)) = spec.split_once('-') {
                                    let start: u64 = s.parse().unwrap_or(0);
                                    let end: u64 = if e.is_empty() {
                                        total.saturating_sub(1)
                                    } else {
                                        e.parse().unwrap_or(total.saturating_sub(1))
                                    };
                                    (start, end.min(total.saturating_sub(1)))
                                } else {
                                    (0, total.saturating_sub(1))
                                }
                            }
                            _ => (0, total.saturating_sub(1)),
                        };
                        let start_usize = start as usize;
                        let end_usize = (end as usize).min(body.len().saturating_sub(1));
                        let slice = &body[start_usize..=end_usize];
                        if range.is_some() {
                            let resp = format!(
                                "HTTP/1.1 206 Partial Content\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nContent-Range: bytes {start}-{end}/{total}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                slice.len()
                            );
                            let _ = stream.write_all(resp.as_bytes());
                            let _ = stream.write_all(slice);
                        } else {
                            let resp = format!(
                                "HTTP/1.1 200 OK\r\nAccept-Ranges: bytes\r\nContent-Type: application/octet-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            );
                            let _ = stream.write_all(resp.as_bytes());
                            let _ = stream.write_all(body.as_ref());
                        }
                    });
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => {
                    if shutdown_clone.load(Ordering::SeqCst) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        });

        Self {
            url: format!("http://{}", addr),
            requests,
            shutdown,
            join: Some(join),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.url, path)
    }

    fn requests(&self) -> Vec<RetryBoundRequest> {
        self.requests.lock().expect("lock requests").clone()
    }

    fn data_attempts_after_probe(&self) -> usize {
        self.requests()
            .iter()
            .filter(|r| r.range.as_deref() != Some("bytes=0-0"))
            .count()
    }
}

impl Drop for RetryBoundServer {
    fn drop(&mut self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::SeqCst);
        if let Ok(stream) = std::net::TcpStream::connect(self.url.trim_start_matches("http://")) {
            let _ = stream.shutdown(std::net::Shutdown::Both);
        }
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

#[tokio::test]
async fn retry_attempts_bound_data_requests_and_allow_success_within_budget() {
    let body = non_periodic_bytes(256 * 1024);
    let hash = sha256_hex(&body);

    // Budget = 2: probe succeeds, exactly 2 data attempts (both 503) must fail.
    // Probe is served normally and does not consume the data-attempt budget; the
    // inline server counts non-probe ranged requests as data attempts.
    let fail_server = RetryBoundServer::start(body.clone());
    let fail_dest = tempdir().expect("dest");
    let fail_store_dir = tempdir().expect("store");
    let fail_engine = Engine::new(
        open_store(&fail_store_dir),
        fail_dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    fail_engine.set_network_options(NetworkOptions {
        retry_attempts: 2,
        ..NetworkOptions::default()
    });

    let fail_id = fail_engine
        .add_task(make_spec(
            fail_server.url("/retry-bound/fail.bin"),
            "retry-fail.bin",
            1,
            Some(hash.clone()),
        ))
        .expect("add fail task");
    let (_, fail_state) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, fail_engine.run())
                .await
                .expect("fail engine timed out")
                .expect("fail engine run failed");
        },
        wait_for_terminal(fail_engine.store(), &fail_id, WAIT_TIMEOUT),
    );
    assert_eq!(
        fail_state,
        TaskState::Failed,
        "retry_attempts=2 must fail after bounded 503s"
    );
    assert!(
        !fail_dest.path().join("retry-fail.bin").exists(),
        "a bounded-retry failure must not produce a final file"
    );
    assert_eq!(
        fail_server.data_attempts_after_probe(),
        2,
        "with retry_attempts=2 the server must observe exactly 2 data attempts after the probe, requests={:?}",
        fail_server.requests()
    );

    // Budget = 3: first two data attempts fail with 503, third succeeds.
    let ok_server = RetryBoundServer::start(body.clone());
    let ok_dest = tempdir().expect("dest");
    let ok_store_dir = tempdir().expect("store");
    let ok_engine = Engine::new(
        open_store(&ok_store_dir),
        ok_dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    ok_engine.set_network_options(NetworkOptions {
        retry_attempts: 3,
        ..NetworkOptions::default()
    });

    let ok_id = ok_engine
        .add_task(make_spec(
            ok_server.url("/retry-bound/ok.bin"),
            "retry-ok.bin",
            1,
            Some(hash.clone()),
        ))
        .expect("add ok task");
    let (_, ok_state) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, ok_engine.run())
                .await
                .expect("ok engine timed out")
                .expect("ok engine run failed");
        },
        wait_for_terminal(ok_engine.store(), &ok_id, WAIT_TIMEOUT),
    );
    assert_eq!(
        ok_state,
        TaskState::Completed,
        "retry_attempts=3 must succeed within the configured budget"
    );
    let ok_task = ok_engine
        .store()
        .task(&ok_id)
        .expect("load ok task")
        .expect("ok task exists");
    assert_eq!(ok_task.actual_sha256.as_deref(), Some(hash.as_str()));
    assert_eq!(
        std::fs::read(ok_dest.path().join("retry-ok.bin")).expect("read final"),
        body,
        "successful bounded retry must produce the fixture bytes"
    );
    assert!(
        ok_server.data_attempts_after_probe() >= 3,
        "with retry_attempts=3 the server must observe at least 3 data attempts after the probe, requests={:?}",
        ok_server.requests()
    );
}

#[tokio::test]
async fn forbidden_and_crlf_headers_remain_rejected_and_unpersisted_with_network_options() {
    let fixture = RangeServer::start(FixtureConfig {
        body: non_periodic_bytes(64 * 1024),
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let db_path = store_dir.path().join("tasks.db");
    let engine = Engine::new(
        Database::open(&db_path).expect("open store"),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    engine.set_network_options(NetworkOptions {
        user_agent: Some("OneGet-Global/1.0".to_string()),
        ..NetworkOptions::default()
    });

    for (header, value) in [
        ("Host", "evil.example"),
        ("Authorization", "Bearer secret"),
        ("Cookie", "session=secret"),
        ("Range", "bytes=0-1"),
        ("Proxy-Authorization", "Basic secret"),
        ("Content-Length", "42"),
        ("Connection", "keep-alive"),
        ("Transfer-Encoding", "chunked"),
        ("Upgrade", "h2c"),
        ("TE", "trailers"),
        ("Trailer", "Expires"),
        ("Set-Cookie", "a=b"),
    ] {
        let error = engine
            .add_task(make_spec_with_headers(
                fixture.url("/netopts/forbidden.bin"),
                "forbidden.bin",
                1,
                None,
                vec![(header.to_string(), value.to_string())],
            ))
            .expect_err("forbidden header must be rejected");
        assert_eq!(error.code(), "INVALID_HEADER", "header {header}");
    }

    for (name, value) in [
        ("X-Bad".to_string(), "value\r\nInjected: x".to_string()),
        ("X-Bad".to_string(), "value\nInjected: x".to_string()),
        ("X-Bad\r\nInjected".to_string(), "x".to_string()),
    ] {
        let error = engine
            .add_task(make_spec_with_headers(
                fixture.url("/netopts/crlf.bin"),
                "crlf.bin",
                1,
                None,
                vec![(name, value)],
            ))
            .expect_err("CR/LF must be rejected");
        assert_eq!(error.code(), "INVALID_HEADER");
    }

    assert!(
        fixture.requests().is_empty(),
        "rejected headers must not produce any request"
    );
    assert!(
        engine.store().list_tasks().expect("list tasks").is_empty(),
        "rejected headers must not create a task"
    );
    let dump = sqlite_text_dump(&db_path);
    for marker in [
        "evil.example",
        "Bearer secret",
        "session=secret",
        "Basic secret",
        "Injected: x",
        "OneGet-Global/1.0",
    ] {
        assert!(
            !dump.contains(marker),
            "sensitive or rejected header value {marker:?} must not be persisted"
        );
    }
}

// --- Cookie sources and provenance ---

fn browser_cookie_value(
    name: &str,
    domain: &str,
    path: &str,
    secure: bool,
    expires: u64,
) -> BrowserCookie {
    BrowserCookie {
        name: name.to_string(),
        value: format!("browser-secret-{name}"),
        domain: domain.to_string(),
        path: path.to_string(),
        secure,
        expires,
    }
}

fn add_request(url: String, filename: &str) -> AddDownload {
    AddDownload {
        url,
        filename: filename.to_string(),
        destination: None,
        connections: 1,
        rate_limit: None,
        expected_sha256: None,
        allow_html: false,
        extra_headers: Vec::new(),
        cookies_file: None,
    }
}

async fn wait_service_terminal(service: &EngineService, id: &str, deadline: Duration) -> TaskState {
    let start = Instant::now();
    loop {
        let tasks = service.list().expect("list tasks");
        let task = tasks
            .into_iter()
            .find(|task| task.id == id)
            .expect("task exists");
        if matches!(
            task.state,
            TaskState::Completed | TaskState::Failed | TaskState::Cancelled | TaskState::Paused
        ) {
            return task.state;
        }
        if start.elapsed() > deadline {
            panic!(
                "timed out waiting for service task {id}, state {:?}",
                task.state
            );
        }
        sleep(POLL_INTERVAL).await;
    }
}

fn summary_for<'a>(summaries: &'a [DownloadSummary], id: &str) -> &'a DownloadSummary {
    summaries
        .iter()
        .find(|summary| summary.id == id)
        .expect("summary for task")
}

#[tokio::test]
async fn cookie_file_takes_precedence_and_provenance_reports_source_and_count_without_values() {
    let body = non_periodic_bytes(128 * 1024);
    let _hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body: body.clone(),
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let log_dir = tempdir().expect("logs");
    let cookie_dir = tempdir().expect("cookies");
    let logger = debug_logger(log_dir.path());

    let file_cookie_name = "file_cookie";
    let file_cookie_val = "file-cookie-secret-value";
    let cookie_path = write_cookie_file(
        cookie_dir.path(),
        &[NetscapeCookie {
            domain: "127.0.0.1".to_string(),
            include_subdomains: false,
            path: "/".to_string(),
            secure: false,
            expires: future_expiry(),
            name: file_cookie_name.to_string(),
            value: file_cookie_val.to_string(),
        }],
    );

    let service = EngineService::start(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    )
    .expect("start service");
    service.set_logger(Arc::clone(&logger));

    let browser = vec![
        browser_cookie_value("browser_cookie", "127.0.0.1", "/", false, u64::MAX / 2),
        browser_cookie_value("other_host", "other.example", "/", false, u64::MAX / 2),
    ];
    let id = service
        .add_with_cookies(
            add_request(
                fixture.url("/cookies/file-precedence.bin"),
                "file-precedence.bin",
            ),
            CookieSource::File(cookie_path),
        )
        .expect("add file-cookie task");
    let state = wait_service_terminal(&service, &id, WAIT_TIMEOUT).await;
    assert_eq!(state, TaskState::Completed);
    logger.flush().expect("flush logs");

    let requests = fixture.requests();
    assert!(!requests.is_empty());
    for request in &requests {
        let names = cookie_names(&request.headers);
        assert!(names.contains(file_cookie_name));
        assert!(!names.contains("browser_cookie"));
        assert!(!names.contains("other_host"));
    }

    // A browser-cookie task must send only matching in-scope cookies.
    let fixture_browser = RangeServer::start(FixtureConfig {
        body: body.clone(),
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let browser_task = service
        .add_with_cookies(
            add_request(
                fixture_browser.url("/allowed/browser.bin"),
                "browser-only.bin",
            ),
            CookieSource::Browser(vec![
                browser_cookie_value("ok_cookie", "127.0.0.1", "/allowed", false, u64::MAX / 2),
                browser_cookie_value("wrong_path", "127.0.0.1", "/blocked", false, u64::MAX / 2),
                browser_cookie_value("wrong_host", "localhost", "/", false, u64::MAX / 2),
                browser_cookie_value("expired", "127.0.0.1", "/", false, 1),
                browser_cookie_value("secure_only", "127.0.0.1", "/", true, u64::MAX / 2),
            ]),
        )
        .expect("add browser-cookie task");
    let state = wait_service_terminal(&service, &browser_task, WAIT_TIMEOUT).await;
    assert_eq!(state, TaskState::Completed);

    let browser_requests = fixture_browser.requests();
    assert!(!browser_requests.is_empty());
    for request in &browser_requests {
        let names = cookie_names(&request.headers);
        assert_eq!(names, BTreeSet::from(["ok_cookie".to_string()]));
    }

    let summaries = service.list_summaries().expect("list summaries");
    let file_summary = summary_for(&summaries, &id);
    assert_eq!(file_summary.cookie_source.as_deref(), Some("file"));
    assert_eq!(file_summary.cookie_count, 1);
    let browser_summary = summary_for(&summaries, &browser_task);
    assert_eq!(browser_summary.cookie_source.as_deref(), Some("browser"));
    assert_eq!(browser_summary.cookie_count, 1);

    let dump = sqlite_text_dump(&store_dir.path().join("tasks.db"));
    let logs = read_log_files(&logger);
    let file_json = serde_json::to_string(file_summary).expect("serialize summary");
    let browser_json = serde_json::to_string(browser_summary).expect("serialize summary");
    for secret in [
        file_cookie_val,
        "browser-secret-browser_cookie",
        "browser-secret-ok_cookie",
        "browser-secret-wrong_path",
        "browser-secret-wrong_host",
        "browser-secret-expired",
        "browser-secret-secure_only",
        "browser-secret-other_host",
    ] {
        assert!(!dump.contains(secret));
        assert!(!logs.contains(secret));
        assert!(!file_json.contains(secret));
        assert!(!browser_json.contains(secret));
    }

    drop(service);
    let _ = browser;
}

#[tokio::test]
async fn missing_or_unreadable_cookie_file_fails_and_preserves_part() {
    let body = non_periodic_bytes(256 * 1024);
    let fixture = RangeServer::start(FixtureConfig {
        body,
        range_mode: RangeMode::HonorRange,
        stream_delay: Some(Duration::from_millis(10)),
        stream_chunk_size: 16 * 1024,
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let missing = store_dir.path().join("missing-cookies.txt");

    let service = EngineService::start(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    )
    .expect("start service");
    let id = service
        .add_with_cookies(
            add_request(fixture.url("/cookies/missing.bin"), "missing-cookie.bin"),
            CookieSource::File(missing),
        )
        .expect("add task");

    let state = wait_service_terminal(&service, &id, WAIT_TIMEOUT).await;
    assert!(
        matches!(state, TaskState::Failed | TaskState::Paused),
        "missing cookie file must fail the task, got {state:?}"
    );
    let tasks = service.list().expect("list tasks");
    let task = tasks.into_iter().find(|task| task.id == id).expect("task");
    assert_eq!(task.error_code.as_deref(), Some("COOKIE_FILE_ERROR"));

    let part = part_path(dest.path(), "missing-cookie.bin");
    assert!(part.exists(), "part file must be preserved");
    assert!(
        !dest.path().join("missing-cookie.bin").exists(),
        "no final file may be created"
    );
}

#[tokio::test]
async fn captured_cookie_task_after_restart_sends_no_browser_cookies_and_fails_auth_preserving_part(
) {
    let body = non_periodic_bytes(256 * 1024);
    let fixture = RangeServer::start(FixtureConfig {
        body,
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let log_dir = tempdir().expect("logs");
    let db_path = store_dir.path().join("tasks.db");

    let browser_secret = "browser-restart-secret";
    let cookies = vec![BrowserCookie {
        name: "session".to_string(),
        value: browser_secret.to_string(),
        domain: "127.0.0.1".to_string(),
        path: "/".to_string(),
        secure: false,
        expires: u64::MAX / 2,
    }];

    {
        let service = EngineService::start(
            Database::open(&db_path).expect("open store"),
            dest.path().to_path_buf(),
            EngineOptions { max_concurrent: 1 },
        )
        .expect("start service");
        let id = service
            .add_with_cookies(
                add_request(fixture.url("/restart/ok.bin"), "restart-cookie.bin"),
                CookieSource::Browser(cookies.clone()),
            )
            .expect("add browser-cookie task");
        let state = wait_service_terminal(&service, &id, WAIT_TIMEOUT).await;
        assert_eq!(state, TaskState::Completed);
        service.shutdown();
    }

    // Second phase: require cookie on restart; task should re-request without browser cookies.
    let auth_fixture = RangeServer::start(FixtureConfig {
        body: non_periodic_bytes(256 * 1024),
        range_mode: RangeMode::HonorRange,
        status_override: Some(401),
        ..Default::default()
    });
    let logger = debug_logger(log_dir.path());
    let service = EngineService::start(
        Database::open(&db_path).expect("reopen store"),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    )
    .expect("restart service");
    service.set_logger(Arc::clone(&logger));

    let _id = service
        .add_with_cookies(
            add_request(auth_fixture.url("/restart/auth.bin"), "restart-auth.bin"),
            CookieSource::Browser(cookies),
        )
        .expect("add browser-cookie task for restart");
    // Simulate resume in a new process context: drop and recreate without re-supplying cookies.
    service.shutdown();
    drop(service);

    // Create the task record in a state that will be resumed? easier: start new service and retry/resume after failure path.
    let service = EngineService::start(
        Database::open(&db_path).expect("reopen store again"),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    )
    .expect("restart service again");
    service.set_logger(Arc::clone(&logger));
    let tasks = service.list().expect("list tasks");
    let restart_task = tasks
        .into_iter()
        .find(|task| task.filename == "restart-auth.bin")
        .expect("restart-auth task");
    if matches!(restart_task.state, TaskState::Paused | TaskState::Failed) {
        let _ = service.resume(&restart_task.id);
        let _ = service.retry(&restart_task.id);
    }
    let state = wait_service_terminal(&service, &restart_task.id, WAIT_TIMEOUT).await;
    assert_eq!(
        state,
        TaskState::Failed,
        "resume without browser cookies must fail auth"
    );
    let tasks = service.list().expect("list tasks");
    let task = tasks
        .into_iter()
        .find(|task| task.id == restart_task.id)
        .expect("task");
    assert_eq!(task.error_code.as_deref(), Some("AUTH_REQUIRED"));

    for request in auth_fixture.requests() {
        let names = cookie_names(&request.headers);
        assert!(
            names.is_empty(),
            "no browser cookies may be sent after restart"
        );
    }

    assert!(
        part_path(dest.path(), "restart-auth.bin").exists(),
        "part must be preserved"
    );

    logger.flush().expect("flush logs");
    let dump = sqlite_text_dump(&db_path);
    let logs = read_log_files(&logger);
    let summaries = service.list_summaries().expect("list summaries");
    let summary = summary_for(&summaries, &restart_task.id);
    let summary_json = serde_json::to_string(summary).expect("serialize summary");
    assert!(!dump.contains(browser_secret));
    assert!(!logs.contains(browser_secret));
    assert!(!summary_json.contains(browser_secret));
}

#[tokio::test]
async fn request_level_absolute_cookies_file_is_used_by_the_task() {
    let body = non_periodic_bytes(128 * 1024);
    let fixture = RangeServer::start(FixtureConfig {
        body,
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let cookie_dir = tempdir().expect("cookies");
    let cookie_name = "request_cookie";
    let cookie_secret = "request-cookie-value";
    let cookie_path = write_cookie_file(
        cookie_dir.path(),
        &[NetscapeCookie {
            domain: "127.0.0.1".to_string(),
            include_subdomains: false,
            path: "/".to_string(),
            secure: false,
            expires: future_expiry(),
            name: cookie_name.to_string(),
            value: cookie_secret.to_string(),
        }],
    );

    let service = EngineService::start(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    )
    .expect("start service");

    let mut request = add_request(
        fixture.url("/cookies/request-level.bin"),
        "request-level.bin",
    );
    request.cookies_file = Some(cookie_path);

    let id = service
        .add(request)
        .expect("add task with request-level cookies file");
    let state = wait_service_terminal(&service, &id, WAIT_TIMEOUT).await;
    assert_eq!(state, TaskState::Completed);

    let requests = fixture.requests();
    assert!(!requests.is_empty(), "fixture must receive requests");
    for request in &requests {
        let names = cookie_names(&request.headers);
        assert!(
            names.contains(cookie_name),
            "request-level cookies file must be sent, got {names:?}"
        );
        assert_eq!(
            cookie_value(&request.headers, cookie_name).as_deref(),
            Some(cookie_secret)
        );
    }
}

#[tokio::test]
async fn request_level_relative_cookies_file_is_rejected_before_task_creation() {
    let body = non_periodic_bytes(64 * 1024);
    let fixture = RangeServer::start(FixtureConfig {
        body,
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");

    let service = EngineService::start(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    )
    .expect("start service");

    let mut request = add_request(
        fixture.url("/cookies/relative-rejected.bin"),
        "relative-rejected.bin",
    );
    request.cookies_file = Some(PathBuf::from("relative/cookies.txt"));

    let error = service
        .add(request)
        .expect_err("relative cookies file must be rejected");
    assert_eq!(error.code(), "INVALID_COOKIES_FILE");

    let tasks = service.list().expect("list tasks");
    assert!(
        tasks.is_empty(),
        "no task may be created for a rejected cookies file path"
    );
    assert!(
        fixture.requests().is_empty(),
        "a rejected cookies file path must not produce any request"
    );
}

// ---------------------------------------------------------------------------
// Review repair RED tests: AC-028 / AC-029 and E3
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ac_028_running_task_applies_global_rate_limit_without_restart() {
    // Long, slowly streamed body so the limit can be changed while the task is
    // actively downloading. The post-change steady window must reflect the new
    // limit without restarting the task.
    const BODY_BYTES: usize = 12 * 1024 * 1024;
    let body = pattern_bytes(0x61, BODY_BYTES);
    let hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body,
        range_mode: RangeMode::HonorRange,
        slow_first_half_delay: Some(Duration::from_millis(8)),
        stream_chunk_size: 64 * 1024,
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    let id = engine
        .add_task(make_spec(
            fixture.url("/rate/ac028-running-global.bin"),
            "ac028-running-global.bin",
            1,
            Some(hash.clone()),
        ))
        .expect("add task");

    let new_limit = MIB;
    let mut change_at: Option<Instant> = None;
    let mut samples: Vec<ProgressSample> = Vec::new();

    let (run_result, _) = tokio::join!(
        async {
            timeout(Duration::from_secs(60), engine.run())
                .await
                .expect("engine run timed out")
        },
        async {
            // Let it make progress without a throttling limit, then lower the
            // global limit while the task is still running.
            let started = Instant::now();
            loop {
                let task = engine.store().task(&id).expect("load task").expect("task");
                if task.downloaded_bytes >= 2 * MIB || task.state == TaskState::Completed {
                    break;
                }
                if started.elapsed() > WAIT_TIMEOUT {
                    panic!(
                        "timed out waiting for initial progress, state {:?}, downloaded={}",
                        task.state, task.downloaded_bytes
                    );
                }
                sleep(POLL_INTERVAL).await;
            }
            let task_before_change = engine.store().task(&id).expect("load task").expect("task");
            assert_eq!(
                task_before_change.state,
                TaskState::Downloading,
                "the task must still be running when the global limit changes"
            );

            change_at = Some(Instant::now());
            engine.set_global_rate_limit(Some(new_limit));

            // Sample until terminal so the post-change steady window is observed.
            let start = Instant::now();
            loop {
                let task = engine.store().task(&id).expect("load task").expect("task");
                samples.push(ProgressSample {
                    at: Instant::now(),
                    downloaded: task.downloaded_bytes,
                });
                if matches!(
                    task.state,
                    TaskState::Completed
                        | TaskState::Failed
                        | TaskState::Cancelled
                        | TaskState::Paused
                ) {
                    break;
                }
                if start.elapsed() > Duration::from_secs(60) {
                    panic!(
                        "timed out sampling after limit change, state {:?}, downloaded={}",
                        task.state, task.downloaded_bytes
                    );
                }
                sleep(Duration::from_millis(25)).await;
            }
        }
    );
    run_result.expect("engine run failed");

    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(
        task.state,
        TaskState::Completed,
        "limited running download failed with {:?}: {:?}",
        task.error_code,
        task.error_message
    );
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));

    let change_at = change_at.expect("limit change timestamp recorded");
    // Measure throughput over a stable post-change window (drop a couple of
    // boundary samples to avoid changeover/startup noise) so the result reflects
    // the limit applied to the still-running task without a restart.
    let post: Vec<ProgressSample> = samples
        .into_iter()
        .filter(|sample| sample.at >= change_at)
        .collect();
    assert!(
        post.len() >= 6,
        "expected enough post-change samples to measure throughput, got {}",
        post.len()
    );
    let trim = 2usize.min(post.len() / 3);
    let window = &post[trim..post.len().saturating_sub(trim)];
    assert!(
        window.len() >= 2,
        "post-change measurement window must contain at least two samples"
    );
    let first = &window[0];
    let last = window.last().expect("window has samples");
    let dt = last.at.duration_since(first.at).as_secs_f64();
    assert!(dt > 0.0, "post-change measurement window must span time");
    let bytes = last.downloaded.saturating_sub(first.downloaded) as f64;
    let measured = bytes / dt;
    let expected = new_limit as f64;
    assert!(
        (expected * 0.9..=expected * 1.1).contains(&measured),
        "post-change steady throughput {measured:.0} B/s is outside 10% of {expected:.0} B/s (task must apply the lower global limit mid-download without restart)"
    );
}

#[tokio::test]
async fn ac_029_per_task_and_global_limits_use_the_stricter_combination() {
    const BODY_BYTES: usize = 8 * 1024 * 1024;
    let body = pattern_bytes(0x62, BODY_BYTES);
    let hash = sha256_hex(&body);
    let fixture = RangeServer::start(FixtureConfig {
        body,
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    let per_task = 2 * MIB;
    let global = MIB;
    let stricter = global.min(per_task);
    engine.set_global_rate_limit(Some(global));

    let id = engine
        .add_task(make_spec(
            fixture.url("/rate/ac029-combined.bin"),
            "ac029-combined.bin",
            2,
            Some(hash.clone()),
        ))
        .expect("add task");
    engine.set_task_rate_limit(&id, Some(per_task));

    let ((), samples) = tokio::join!(
        async {
            timeout(Duration::from_secs(40), engine.run())
                .await
                .expect("combined-rate engine run timed out")
                .expect("engine run failed");
        },
        sample_progress(engine.store(), &id, Duration::from_secs(40)),
    );

    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(
        task.state,
        TaskState::Completed,
        "combined-rate download failed with {:?}: {:?}",
        task.error_code,
        task.error_message
    );
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));

    let measured = steady_rate(&samples, BODY_BYTES as u64, RATE_STARTUP, RATE_FINALIZE);
    let expected = stricter as f64;
    assert!(
        (expected * 0.9..=expected * 1.1).contains(&measured),
        "combined steady throughput {measured:.0} B/s must respect the stricter limit {expected:.0} B/s within 10%"
    );
}

use oneget_lib::commands::app::hf_token_save as app_hf_token_save;
use oneget_lib::commands::downloads::downloads_add as app_downloads_add;
use oneget_lib::commands::downloads::AddDownloadRequest;

#[tokio::test]
async fn e3_secret_reapplication_applies_hf_token_and_proxy_credentials_to_running_engine() {
    // Hugging Face token: saved through the command must be applied so a gated
    // HF download started afterwards carries Authorization, and clearing removes it.
    let body = non_periodic_bytes(128 * 1024);
    let hash = sha256_hex(&body);
    let cdn_base = "http://cdn-lfs.huggingface.co/repos/fixture/e3.bin";
    let hf = HfFixtureServer::start(HfFixtureConfig {
        resolver_behavior: ResolverBehavior::RedirectToFreshCdn(cdn_base.to_string()),
        cdn_body: body.clone(),
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store = Database::open(dest.path().join("tasks.db")).expect("open store");
    let engine = Arc::new(
        EngineService::start(
            store,
            dest.path().to_path_buf(),
            EngineOptions { max_concurrent: 1 },
        )
        .expect("start engine"),
    );
    engine
        .set_proxy(Some(ProxyConfig {
            url: hf.proxy_url(),
            username: None,
            password: None,
        }))
        .expect("route HF fixture through proxy");

    // Build an AppState harness whose engine is the one we control.
    let temp = tempdir().expect("tempdir");
    let paths = AppPaths::with_root(temp.path().join("app")).expect("app paths");
    let settings_path = paths.data.join("settings.json");
    let settings_store = SettingsStore::load(&settings_path).expect("settings store");
    let logger = Logger::new(LogConfig::with_defaults(paths.logs.clone())).expect("logger");
    let secrets: Arc<dyn SecretService> = Arc::new(MemorySecrets::new());
    let state = AppState::new(
        paths,
        settings_store,
        logger,
        secrets,
        Arc::new(NoopNotifications),
        Arc::new(NoopAutostart),
        Arc::new(NoopQuit),
    )
    .with_engine(Arc::clone(&engine));

    let saved_token =
        app_hf_token_save(&state, "hf_e3_secret_token".to_string()).expect("save hf token");
    assert_single_boolean_status(saved_token, true);

    let id = app_downloads_add(
        &state,
        AddDownloadRequest {
            url: "http://huggingface.co/gpt2/resolve/main/e3.bin".to_string(),
            filename: "e3-gated.bin".to_string(),
            destination: None,
            connections: 1,
            rate_limit: None,
            expected_sha256: Some(hash.clone()),
            allow_html: false,
            extra_headers: Vec::new(),
            cookies_file: None,
            cookie_handle: None,
        },
    )
    .expect("add gated HF task");

    let terminal = wait_service_terminal(&engine, &id, Duration::from_secs(30)).await;
    assert_eq!(
        terminal,
        TaskState::Completed,
        "gated HF task must complete once the saved token is applied"
    );
    let resolver = hf.resolver_requests();
    assert!(
        !resolver.is_empty(),
        "resolver must be contacted for the HF download"
    );
    assert!(
        resolver
            .iter()
            .all(|req| req.authorization.as_deref() == Some("Bearer hf_e3_secret_token")),
        "every resolver request must carry the saved token after hf_token_save, got {:?}",
        resolver
            .iter()
            .map(|req| req.authorization.clone())
            .collect::<Vec<_>>()
    );

    oneget_lib::commands::app::hf_token_clear(&state).expect("clear hf token");

    // Start a second gated fixture/task and assert no Authorization is sent.
    let body2 = non_periodic_bytes(64 * 1024);
    let hash2 = sha256_hex(&body2);
    let cdn_base2 = "http://cdn-lfs.huggingface.co/repos/fixture/e3b.bin";
    let hf2 = HfFixtureServer::start(HfFixtureConfig {
        resolver_behavior: ResolverBehavior::RedirectToFreshCdn(cdn_base2.to_string()),
        cdn_body: body2.clone(),
        ..Default::default()
    });
    engine
        .set_proxy(Some(ProxyConfig {
            url: hf2.proxy_url(),
            username: None,
            password: None,
        }))
        .expect("route second HF fixture through proxy");

    let id2 = app_downloads_add(
        &state,
        AddDownloadRequest {
            url: "http://huggingface.co/gpt2/resolve/main/e3b.bin".to_string(),
            filename: "e3b-gated.bin".to_string(),
            destination: None,
            connections: 1,
            rate_limit: None,
            expected_sha256: Some(hash2),
            allow_html: false,
            extra_headers: Vec::new(),
            cookies_file: None,
            cookie_handle: None,
        },
    )
    .expect("add second gated HF task");
    let _ = wait_service_terminal(&engine, &id2, Duration::from_secs(30)).await;
    let resolver2 = hf2.resolver_requests();
    assert!(!resolver2.is_empty(), "second resolver must be contacted");
    assert!(
        resolver2.iter().all(|req| req.authorization.is_none()),
        "clearing the HF token must remove Authorization from later requests, got {:?}",
        resolver2
            .iter()
            .map(|req| req.authorization.clone())
            .collect::<Vec<_>>()
    );

    // Proxy credentials: save while manual mode is saved, then a subsequent
    // authenticated proxy download must succeed without another settings save.
    const PROXY_USER: &str = "e3-proxy-user";
    const PROXY_PASS: &str = "e3-proxy-pass-secret";
    let proxy_body = non_periodic_bytes(128 * 1024);
    let proxy_hash = sha256_hex(&proxy_body);
    let fixture = RangeServer::start(FixtureConfig {
        body: proxy_body.clone(),
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let proxy = ProxyServer::start(Some(ProxyCredentials {
        username: PROXY_USER.to_string(),
        password: PROXY_PASS.to_string(),
    }));

    let proxy_port: u16 = proxy
        .url()
        .rsplit(':')
        .next()
        .expect("proxy port")
        .parse()
        .expect("proxy port number");
    let manual = manual_proxy_settings(ProxyType::Http, "127.0.0.1", proxy_port);
    settings_save(&state, manual).expect("save manual proxy settings");
    let saved_proxy =
        proxy_credentials_save(&state, PROXY_USER.to_string(), PROXY_PASS.to_string())
            .expect("save proxy credentials");
    assert_single_boolean_status(saved_proxy, true);

    let proxy_task = app_downloads_add(
        &state,
        AddDownloadRequest {
            url: fixture.url("/e3/proxy.bin"),
            filename: "e3-proxy.bin".to_string(),
            destination: None,
            connections: 1,
            rate_limit: None,
            expected_sha256: Some(proxy_hash.clone()),
            allow_html: false,
            extra_headers: Vec::new(),
            cookies_file: None,
            cookie_handle: None,
        },
    )
    .expect("add authenticated proxy task");
    let proxy_state = wait_service_terminal(&engine, &proxy_task, Duration::from_secs(30)).await;
    assert_eq!(
        proxy_state,
        TaskState::Completed,
        "saving proxy credentials must make a subsequent manual-proxy download succeed"
    );
    assert!(
        !proxy.requests().is_empty(),
        "the authenticated proxy must observe the download request"
    );
}
