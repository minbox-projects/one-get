//! Behavior tests for cooperative dynamic splitting and the safe Range
//! fallback. Every fixture is local and deterministic.

mod support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use oneget_lib::engine::http::build_client_with_timeouts;
use oneget_lib::engine::retry::classify_reqwest_error;
use oneget_lib::engine::scheduler::{Engine, EngineOptions};
use oneget_lib::engine::state::TaskState;
use oneget_lib::engine::worker::DownloadSpec;
use oneget_lib::store::{Database, Segment};
use sha2::{Digest, Sha256};
use support::fixtures::pattern_bytes;
use support::range_server::{FixtureConfig, RangeMode, RangeServer};
use tempfile::tempdir;
use tokio::time::{sleep, timeout, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(15);
const WAIT_TIMEOUT: Duration = Duration::from_secs(20);

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
