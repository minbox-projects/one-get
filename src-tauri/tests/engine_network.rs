//! Behavior tests for cooperative dynamic splitting and the safe Range
//! fallback. Every fixture is local and deterministic.

mod support;

use std::path::{Path, PathBuf};
use std::time::Duration;

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
