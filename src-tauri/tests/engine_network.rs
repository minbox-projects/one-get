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

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use cookies::{
    cookie_names, cookie_value, future_expiry, past_expiry, write_cookie_file, NetscapeCookie,
};
use oneget_lib::engine::http::{build_client_with_timeouts, ProxyConfig};
use oneget_lib::engine::retry::classify_reqwest_error;
use oneget_lib::engine::scheduler::{Engine, EngineOptions};
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
