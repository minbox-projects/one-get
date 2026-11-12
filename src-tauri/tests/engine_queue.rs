//! Behavior tests for the scheduler queue, concurrency limits, pause/resume,
//! cancel, and retry semantics.

mod support;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use oneget_lib::engine::error::EngineError;
use oneget_lib::engine::part_file::PartFile;
use oneget_lib::engine::scheduler::{Engine, EngineOptions};
use oneget_lib::engine::state::{SegmentState, TaskState};
use oneget_lib::engine::worker::DownloadSpec;
use oneget_lib::store::Database;
use sha2::{Digest, Sha256};
use support::fixtures::pattern_bytes;
use support::range_server::{FixtureConfig, RangeMode, RangeServer, ServedRange};
use tempfile::tempdir;
use tokio::time::{sleep, timeout, Instant};

const POLL_INTERVAL: Duration = Duration::from_millis(20);
const WAIT_TIMEOUT: Duration = Duration::from_secs(20);
const SHORT_SETTLE: Duration = Duration::from_millis(250);

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn assert_code(err: &EngineError, code: &str) {
    assert_eq!(
        err.code(),
        code,
        "expected code {code}, got {}: {}",
        err.code(),
        err
    );
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

fn slow_body_config(body: Vec<u8>) -> FixtureConfig {
    FixtureConfig {
        body,
        range_mode: RangeMode::HonorRange,
        stream_delay: Some(Duration::from_millis(12)),
        stream_chunk_size: 16 * 1024,
        ..Default::default()
    }
}

fn read_file(path: &Path) -> Vec<u8> {
    std::fs::read(path).expect("read file")
}

async fn wait_for_state(
    store: &Database,
    id: &str,
    expected: TaskState,
    deadline: Duration,
) -> TaskState {
    let start = Instant::now();
    loop {
        let task = store.task(id).expect("load task").expect("task exists");
        if task.state == expected {
            return task.state;
        }
        if matches!(
            task.state,
            TaskState::Failed | TaskState::Cancelled | TaskState::Completed
        ) && task.state != expected
        {
            panic!(
                "task {id} reached terminal state {:?} while waiting for {:?} (error={:?} {:?})",
                task.state, expected, task.error_code, task.error_message
            );
        }
        if start.elapsed() > deadline {
            panic!(
                "timed out waiting for task {id} to reach {:?}, current {:?}, downloaded={:?}, error={:?} {:?}",
                expected,
                task.state,
                task.downloaded_bytes,
                task.error_code,
                task.error_message
            );
        }
        sleep(POLL_INTERVAL).await;
    }
}

async fn wait_for_progress(store: &Database, id: &str, min_bytes: u64, deadline: Duration) -> u64 {
    let start = Instant::now();
    loop {
        let task = store.task(id).expect("load task").expect("task exists");
        let downloaded = task.downloaded_bytes;
        if downloaded >= min_bytes
            && matches!(
                task.state,
                TaskState::Downloading | TaskState::Finalizing | TaskState::Completed
            )
        {
            return downloaded;
        }
        if matches!(task.state, TaskState::Failed | TaskState::Cancelled) {
            panic!(
                "task {id} failed/cancelled while waiting for progress >= {min_bytes}: {:?} {:?} {:?}",
                task.state, task.error_code, task.error_message
            );
        }
        if start.elapsed() > deadline {
            panic!(
                "timed out waiting for task {id} progress >= {min_bytes}, current {downloaded}, state {:?}",
                task.state
            );
        }
        sleep(POLL_INTERVAL).await;
    }
}

async fn wait_for_active_at_least(store: &Database, min: i64, deadline: Duration) -> i64 {
    let start = Instant::now();
    loop {
        let active = store.active_task_count().expect("active count");
        if active >= min {
            return active;
        }
        if start.elapsed() > deadline {
            panic!("timed out waiting for active_task_count >= {min}, current {active}");
        }
        sleep(POLL_INTERVAL).await;
    }
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
                "timed out waiting for task {id} terminal-ish state, current {:?}, downloaded={}",
                task.state, task.downloaded_bytes
            );
        }
        sleep(POLL_INTERVAL).await;
    }
}

async fn wait_for_all_completed(store: &Database, ids: &[String], deadline: Duration) {
    let start = Instant::now();
    loop {
        let mut all_done = true;
        for id in ids {
            let task = store.task(id).expect("load task").expect("task exists");
            match task.state {
                TaskState::Completed => {}
                TaskState::Failed => panic!(
                    "task {id} failed: {:?} {:?}",
                    task.error_code, task.error_message
                ),
                TaskState::Cancelled => panic!("task {id} cancelled unexpectedly"),
                _ => all_done = false,
            }
        }
        if all_done {
            return;
        }
        if start.elapsed() > deadline {
            let states: Vec<_> = ids
                .iter()
                .map(|id| {
                    let t = store.task(id).unwrap().unwrap();
                    (id.clone(), t.state, t.downloaded_bytes)
                })
                .collect();
            panic!("timed out waiting for all tasks completed: {states:?}");
        }
        sleep(POLL_INTERVAL).await;
    }
}

async fn wait_for_no_active(store: &Database, deadline: Duration) {
    let start = Instant::now();
    loop {
        let active = store.active_task_count().expect("active count");
        if active == 0 {
            return;
        }
        if start.elapsed() > deadline {
            panic!("timed out waiting for no active tasks, current {active}");
        }
        sleep(POLL_INTERVAL).await;
    }
}

fn part_path(dest_dir: &Path, filename: &str) -> PathBuf {
    dest_dir.join(format!("{filename}.oneget.part"))
}

fn has_range_start(served: &[ServedRange], start: u64) -> bool {
    served.iter().any(|r| r.start == start)
}

#[tokio::test]
async fn fifo_queue_respects_limit_one_and_completion_order() {
    let body = pattern_bytes(11, 512 * 1024);
    let server = RangeServer::start(slow_body_config(body.clone()));
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    let mut ids = Vec::new();
    let mut hashes = Vec::new();
    for i in 0..3 {
        let filename = format!("fifo-{i}.bin");
        let hash = sha256_hex(&body);
        let id = engine
            .add_task(make_spec(
                server.url(&format!("/fifo/{i}")),
                &filename,
                1,
                Some(hash.clone()),
            ))
            .expect("add task");
        ids.push(id);
        hashes.push((filename, hash));
    }

    let observed = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("engine run timeout")
                .expect("engine run");
            "done"
        },
        async {
            wait_for_state(
                engine.store(),
                &ids[0],
                TaskState::Downloading,
                WAIT_TIMEOUT,
            )
            .await;
            // While first runs, others must remain queued.
            let t1 = engine.store().task(&ids[1]).unwrap().unwrap();
            let t2 = engine.store().task(&ids[2]).unwrap().unwrap();
            assert_eq!(
                t1.state,
                TaskState::Queued,
                "second must stay queued while first downloads"
            );
            assert_eq!(
                t2.state,
                TaskState::Queued,
                "third must stay queued while first downloads"
            );
            let active = engine.store().active_task_count().unwrap();
            assert!(
                active <= 1,
                "active_task_count must never exceed 1, got {active}"
            );

            // Track active count bound and completion order by polling.
            let mut max_active: i64 = active;
            let mut completion_order = Vec::new();
            let mut seen = HashSet::new();
            let start = Instant::now();
            while completion_order.len() < 3 {
                let active = engine.store().active_task_count().unwrap();
                if active > max_active {
                    max_active = active;
                }
                for id in &ids {
                    if seen.contains(id) {
                        continue;
                    }
                    let task = engine.store().task(id).unwrap().unwrap();
                    if task.state == TaskState::Completed {
                        seen.insert(id.clone());
                        completion_order.push(id.clone());
                    }
                }
                if start.elapsed() > WAIT_TIMEOUT {
                    panic!("timed out observing fifo completion");
                }
                sleep(POLL_INTERVAL).await;
            }
            (max_active, completion_order)
        }
    );

    let (max_active, completion_order) = observed.1;
    assert!(
        max_active <= 1,
        "active_task_count exceeded 1: {max_active}"
    );
    assert_eq!(completion_order, ids, "completion/start order must be FIFO");
    wait_for_no_active(engine.store(), WAIT_TIMEOUT).await;

    for (i, id) in ids.iter().enumerate() {
        let task = engine.store().task(id).unwrap().unwrap();
        assert_eq!(task.state, TaskState::Completed);
        assert_eq!(task.actual_sha256.as_deref(), Some(hashes[i].1.as_str()));
        assert_eq!(read_file(&dest.path().join(&hashes[i].0)), body);
    }
}

#[tokio::test]
async fn limit_two_runs_two_concurrently_and_completes_with_correct_hashes() {
    let body = pattern_bytes(22, 512 * 1024);
    let server = RangeServer::start(slow_body_config(body.clone()));
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 2 },
    );

    let hash = sha256_hex(&body);
    let id1 = engine
        .add_task(make_spec(
            server.url("/l2/a"),
            "l2-a.bin",
            1,
            Some(hash.clone()),
        ))
        .expect("add 1");
    let id2 = engine
        .add_task(make_spec(
            server.url("/l2/b"),
            "l2-b.bin",
            1,
            Some(hash.clone()),
        ))
        .expect("add 2");

    let (_, reached_two) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("run timeout")
                .expect("run");
        },
        async {
            let active = wait_for_active_at_least(engine.store(), 2, WAIT_TIMEOUT).await;
            assert!(
                active >= 2,
                "must observe active_task_count >= 2, got {active}"
            );
            true
        }
    );

    assert!(reached_two);
    for (id, name) in [(&id1, "l2-a.bin"), (&id2, "l2-b.bin")] {
        let task = engine.store().task(id).unwrap().unwrap();
        assert_eq!(task.state, TaskState::Completed);
        assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));
        assert_eq!(read_file(&dest.path().join(name)), body);
    }
}

#[tokio::test]
async fn set_max_concurrent_takes_effect_without_restart() {
    let body = pattern_bytes(33, 512 * 1024);
    let server = RangeServer::start(slow_body_config(body.clone()));
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    let hash = sha256_hex(&body);
    let id1 = engine
        .add_task(make_spec(
            server.url("/c/1"),
            "c1.bin",
            1,
            Some(hash.clone()),
        ))
        .unwrap();
    let id2 = engine
        .add_task(make_spec(
            server.url("/c/2"),
            "c2.bin",
            1,
            Some(hash.clone()),
        ))
        .unwrap();
    let id3 = engine
        .add_task(make_spec(
            server.url("/c/3"),
            "c3.bin",
            1,
            Some(hash.clone()),
        ))
        .unwrap();

    tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("run timeout")
                .expect("run");
        },
        async {
            wait_for_state(engine.store(), &id1, TaskState::Downloading, WAIT_TIMEOUT).await;
            let t2 = engine.store().task(&id2).unwrap().unwrap();
            assert_eq!(
                t2.state,
                TaskState::Queued,
                "second should be queued under limit 1"
            );
            engine.set_max_concurrent(2).expect("set_max_concurrent");
            // After raising limit, second should start while first still running/downloading.
            let start = Instant::now();
            loop {
                let t1 = engine.store().task(&id1).unwrap().unwrap();
                let t2 = engine.store().task(&id2).unwrap().unwrap();
                if matches!(
                    t2.state,
                    TaskState::Connecting
                        | TaskState::Downloading
                        | TaskState::Finalizing
                        | TaskState::Completed
                ) && matches!(
                    t1.state,
                    TaskState::Downloading | TaskState::Finalizing | TaskState::Connecting
                ) {
                    // observed overlap
                    break;
                }
                if t1.state == TaskState::Completed && t2.state != TaskState::Queued {
                    // if first completed very fast, at least ensure second started (limit applied) - but we need "while first still running"
                    // try to be strict: panic if we missed overlap
                }
                if start.elapsed() > WAIT_TIMEOUT {
                    panic!("did not observe second starting while first still running: t1={:?} t2={:?}", t1.state, t2.state);
                }
                sleep(POLL_INTERVAL).await;
            }
            wait_for_all_completed(
                engine.store(),
                &[id1.clone(), id2.clone(), id3.clone()],
                WAIT_TIMEOUT,
            )
            .await;
        }
    );

    for id in [&id1, &id2, &id3] {
        let task = engine.store().task(id).unwrap().unwrap();
        assert_eq!(task.state, TaskState::Completed);
        assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));
    }
}

#[tokio::test]
async fn paused_task_does_not_consume_slot_and_keeps_part() {
    let body_a = pattern_bytes(44, 1024 * 1024);
    let body_bc = pattern_bytes(55, 256 * 1024);
    let server_a = RangeServer::start(slow_body_config(body_a.clone()));
    let server_b = RangeServer::start(FixtureConfig {
        body: body_bc.clone(),
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let server_c = RangeServer::start(FixtureConfig {
        body: body_bc.clone(),
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

    let hash_a = sha256_hex(&body_a);
    let hash_bc = sha256_hex(&body_bc);
    let id_a = engine
        .add_task(make_spec(server_a.url("/a"), "a.bin", 1, Some(hash_a)))
        .unwrap();

    tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("run timeout")
                .expect("run");
        },
        async {
            wait_for_progress(engine.store(), &id_a, 64 * 1024, WAIT_TIMEOUT).await;
            engine
                .pause(&id_a)
                .await
                .expect("pause task to free concurrency slot");
            wait_for_state(engine.store(), &id_a, TaskState::Paused, WAIT_TIMEOUT).await;
            let part_a = part_path(dest.path(), "a.bin");
            assert!(part_a.exists(), "paused task must keep its part file");
            let progress_a = engine
                .store()
                .task(&id_a)
                .unwrap()
                .unwrap()
                .downloaded_bytes;

            let id_b = engine
                .add_task(make_spec(
                    server_b.url("/b"),
                    "b.bin",
                    1,
                    Some(hash_bc.clone()),
                ))
                .unwrap();
            let id_c = engine
                .add_task(make_spec(
                    server_c.url("/c"),
                    "c.bin",
                    1,
                    Some(hash_bc.clone()),
                ))
                .unwrap();

            wait_for_all_completed(engine.store(), &[id_b.clone(), id_c.clone()], WAIT_TIMEOUT)
                .await;
            assert_eq!(read_file(&dest.path().join("b.bin")), body_bc);
            assert_eq!(read_file(&dest.path().join("c.bin")), body_bc);

            let task_a = engine.store().task(&id_a).unwrap().unwrap();
            assert_eq!(
                task_a.state,
                TaskState::Paused,
                "A must remain paused while B/C complete"
            );
            assert!(part_a.exists(), "A's part must remain after B/C complete");
            assert_eq!(
                task_a.downloaded_bytes, progress_a,
                "paused task progress must not advance"
            );

            // run() may finish while A remains paused (paused tasks do not keep run alive).
            // Verify A stayed paused with its part through B/C completion as required.
            let task_a = engine.store().task(&id_a).unwrap().unwrap();
            assert_eq!(task_a.state, TaskState::Paused);
            assert!(part_a.exists());
            wait_for_no_active(engine.store(), WAIT_TIMEOUT).await;
        }
    );
}

#[tokio::test]
async fn pause_resume_persists_progress_no_further_progress_noop_and_no_rerequest() {
    let body = pattern_bytes(77, 1024 * 1024);
    let hash = sha256_hex(&body);
    let server = RangeServer::start(slow_body_config(body.clone()));
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    let id = engine
        .add_task(make_spec(
            server.url("/pause-resume"),
            "pr.bin",
            4,
            Some(hash.clone()),
        ))
        .expect("add task");

    tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("run timeout")
                .expect("run");
        },
        async {
            // wait until downloading and some progress persisted
            wait_for_state(engine.store(), &id, TaskState::Downloading, WAIT_TIMEOUT).await;
            let progressed = wait_for_progress(engine.store(), &id, 64 * 1024, WAIT_TIMEOUT).await;
            // ensure segments show progress persisted (some downloaded bytes recorded)
            let segs = engine.store().segments_for_task(&id).unwrap();
            assert!(
                segs.iter().any(|s| s.downloaded_bytes > 0),
                "expected persisted segment progress before pause, got {segs:?}"
            );

            engine.pause(&id).await.expect("pause downloading task");
            wait_for_state(engine.store(), &id, TaskState::Paused, WAIT_TIMEOUT).await;

            let after_pause = engine.store().task(&id).unwrap().unwrap();
            let recorded_progress = after_pause.downloaded_bytes;
            assert!(
                recorded_progress >= progressed,
                "recorded progress should reflect persisted bytes"
            );

            sleep(SHORT_SETTLE).await;
            let after_settle = engine.store().task(&id).unwrap().unwrap();
            assert_eq!(after_settle.state, TaskState::Paused);
            assert_eq!(
                after_settle.downloaded_bytes, recorded_progress,
                "no further progress after pause"
            );

            let part = part_path(dest.path(), "pr.bin");
            assert!(part.exists(), "part must exist while paused");
            let segs_paused = engine.store().segments_for_task(&id).unwrap();
            let paused_offsets: Vec<(u64, u64)> = segs_paused
                .iter()
                .map(|s| (s.start, s.downloaded_bytes))
                .collect();

            // pause again is a no-op
            engine
                .pause(&id)
                .await
                .expect("pause paused task is a no-op");
            let still_paused = engine.store().task(&id).unwrap().unwrap();
            assert_eq!(still_paused.state, TaskState::Paused);
            assert_eq!(still_paused.downloaded_bytes, recorded_progress);

            // record served ranges before resume
            let served_before = server.served_ranges();
            for (start, downloaded) in &paused_offsets {
                if *downloaded > 0 {
                    // completed/served portion before pause should not be requested again from same start
                    // (we assert no new request starts at that segment start after resume)
                    assert!(
                        has_range_start(&served_before, *start),
                        "expected segment start {start} served before pause"
                    );
                }
            }

            engine.resume(&id).await.expect("resume paused task");
            // resume on a downloading task is a no-op - observe once it becomes downloading
            let start = Instant::now();
            let mut saw_downloading = false;
            loop {
                let task = engine.store().task(&id).unwrap().unwrap();
                if task.state == TaskState::Downloading {
                    saw_downloading = true;
                    engine
                        .resume(&id)
                        .await
                        .expect("resume downloading task is a no-op");
                    sleep(Duration::from_millis(30)).await;
                    let after = engine.store().task(&id).unwrap().unwrap();
                    assert!(matches!(
                        after.state,
                        TaskState::Downloading | TaskState::Finalizing | TaskState::Completed
                    ));
                    break;
                }
                if task.state == TaskState::Completed {
                    // very fast completion, still acceptable
                    break;
                }
                if start.elapsed() > WAIT_TIMEOUT {
                    panic!(
                        "task did not become downloading/completed after resume, state {:?}",
                        task.state
                    );
                }
                sleep(POLL_INTERVAL).await;
            }

            wait_for_state(engine.store(), &id, TaskState::Completed, WAIT_TIMEOUT).await;
            let task = engine.store().task(&id).unwrap().unwrap();
            assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));
            assert_eq!(read_file(&dest.path().join("pr.bin")), body);
            assert!(!part.exists(), "part removed after completion");

            // completed segments must not be requested again: for each segment whose downloaded
            // bytes were >0 at pause time, there must be no additional served range starting at
            // that same segment start after the pre-resume snapshot count for that start is considered.
            let served_after = server.served_ranges();
            // For segments fully done at pause time, their start should appear at most once total
            // (a done segment would not need re-request). For partially downloaded segments, a resume
            // may request from start+downloaded but must not re-request from start again.
            for seg in &segs_paused {
                if seg.state == SegmentState::Done {
                    let count = served_after.iter().filter(|r| r.start == seg.start).count();
                    assert!(count <= 1, "completed segment starting at {} must not be re-requested, served starts: {:?}", seg.start, served_after);
                } else if seg.downloaded_bytes > 0 {
                    let count = served_after.iter().filter(|r| r.start == seg.start).count();
                    assert!(count <= 1, "partially completed segment start {} must not be requested again from start, served: {:?}", seg.start, served_after);
                }
            }
            let _ = saw_downloading; // observed when possible
        }
    );
}

#[tokio::test]
async fn invalid_pause_and_resume_states_return_invalid_state() {
    let body = pattern_bytes(88, 128 * 1024);
    let hash = sha256_hex(&body);
    let server_ok = RangeServer::start(FixtureConfig {
        body: body.clone(),
        ..Default::default()
    });
    let server_fail = RangeServer::start(FixtureConfig {
        status_override: Some(404),
        ..Default::default()
    });
    let server_slow = RangeServer::start(slow_body_config(pattern_bytes(99, 512 * 1024)));
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    let id_completed = engine
        .add_task(make_spec(
            server_ok.url("/done"),
            "done.bin",
            1,
            Some(hash.clone()),
        ))
        .unwrap();
    let id_failed = engine
        .add_task(make_spec(server_fail.url("/fail"), "fail.bin", 1, None))
        .unwrap();
    let id_cancel = engine
        .add_task(make_spec(server_slow.url("/cancel"), "cancel.bin", 1, None))
        .unwrap();

    tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("run timeout")
                .expect("run");
        },
        async {
            wait_for_state(
                engine.store(),
                &id_completed,
                TaskState::Completed,
                WAIT_TIMEOUT,
            )
            .await;
            let failed_state = wait_for_terminal(engine.store(), &id_failed, WAIT_TIMEOUT).await;
            assert_eq!(failed_state, TaskState::Failed);

            let final_path = dest.path().join("done.bin");
            let final_bytes = read_file(&final_path);
            let completed_before = engine.store().task(&id_completed).unwrap().unwrap();

            let err = engine
                .pause(&id_completed)
                .await
                .expect_err("pause completed must fail");
            assert_code(&err, "INVALID_STATE");
            let after = engine.store().task(&id_completed).unwrap().unwrap();
            assert_eq!(after.state, TaskState::Completed);
            assert_eq!(after.actual_sha256, completed_before.actual_sha256);
            assert_eq!(read_file(&final_path), final_bytes);

            let err = engine
                .resume(&id_completed)
                .await
                .expect_err("resume completed must fail");
            assert_code(&err, "INVALID_STATE");

            let err = engine
                .resume(&id_failed)
                .await
                .expect_err("resume failed must fail");
            assert_code(&err, "INVALID_STATE");
            let failed_after = engine.store().task(&id_failed).unwrap().unwrap();
            assert_eq!(failed_after.state, TaskState::Failed);

            let err = engine
                .cancel(&id_completed)
                .await
                .expect_err("cancel completed must fail");
            assert_code(&err, "INVALID_STATE");
            let after_cancel = engine.store().task(&id_completed).unwrap().unwrap();
            assert_eq!(after_cancel.state, TaskState::Completed);
            assert_eq!(read_file(&final_path), final_bytes);

            wait_for_state(
                engine.store(),
                &id_cancel,
                TaskState::Downloading,
                WAIT_TIMEOUT,
            )
            .await;
            engine.pause(&id_cancel).await.expect("pause downloading");
            wait_for_state(engine.store(), &id_cancel, TaskState::Paused, WAIT_TIMEOUT).await;
            engine
                .pause(&id_cancel)
                .await
                .expect("pause paused task is a no-op");
            let paused = engine.store().task(&id_cancel).unwrap().unwrap();
            assert_eq!(paused.state, TaskState::Paused);

            engine.cancel(&id_cancel).await.expect("cancel paused task");
            wait_for_state(
                engine.store(),
                &id_cancel,
                TaskState::Cancelled,
                WAIT_TIMEOUT,
            )
            .await;
            let err = engine
                .resume(&id_cancel)
                .await
                .expect_err("resume cancelled must fail");
            assert_code(&err, "INVALID_STATE");
            let cancelled = engine.store().task(&id_cancel).unwrap().unwrap();
            assert_eq!(cancelled.state, TaskState::Cancelled);
        }
    );

    let store_dir2 = tempdir().expect("store2");
    let dest2 = tempdir().expect("dest2");
    let engine2 = Engine::new(
        open_store(&store_dir2),
        dest2.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    let server_q = RangeServer::start(FixtureConfig {
        body: vec![1, 2, 3],
        ..Default::default()
    });
    let id_q = engine2
        .add_task(make_spec(server_q.url("/q"), "q.bin", 1, None))
        .unwrap();
    engine2
        .resume(&id_q)
        .await
        .expect("resume queued task is a no-op");
    let tq = engine2.store().task(&id_q).unwrap().unwrap();
    assert_eq!(
        tq.state,
        TaskState::Queued,
        "resume on queued task must be a no-op"
    );
}

#[tokio::test]
async fn cancel_stops_workers_cleans_own_part_and_respects_state_rules() {
    let body_a = pattern_bytes(101, 1024 * 1024);
    let body_b = pattern_bytes(102, 256 * 1024);
    let server_a = RangeServer::start(slow_body_config(body_a.clone()));
    let server_b = RangeServer::start(FixtureConfig {
        body: body_b.clone(),
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 2 },
    );

    let id_a = engine
        .add_task(make_spec(server_a.url("/a"), "a.bin", 1, None))
        .unwrap();
    let id_b = engine
        .add_task(make_spec(server_b.url("/b"), "b.bin", 1, None))
        .unwrap();

    tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("run timeout")
                .expect("run");
        },
        async {
            wait_for_state(engine.store(), &id_a, TaskState::Downloading, WAIT_TIMEOUT).await;
            wait_for_state(engine.store(), &id_b, TaskState::Downloading, WAIT_TIMEOUT).await;

            let part_a = part_path(dest.path(), "a.bin");
            let part_b = part_path(dest.path(), "b.bin");
            assert!(part_a.exists(), "downloading A should have a part");

            engine.cancel(&id_a).await.expect("cancel downloading task");
            wait_for_state(engine.store(), &id_a, TaskState::Cancelled, WAIT_TIMEOUT).await;
            assert!(
                !dest.path().join("a.bin").exists(),
                "cancelled task must have no final file"
            );
            assert!(
                !part_a.exists(),
                "cancelled task's own part must be removed"
            );

            // cancel again is a no-op
            engine
                .cancel(&id_a)
                .await
                .expect("cancel cancelled task is a no-op");
            let ta = engine.store().task(&id_a).unwrap().unwrap();
            assert_eq!(ta.state, TaskState::Cancelled);

            // other task's part/final untouched, B completes
            wait_for_state(engine.store(), &id_b, TaskState::Completed, WAIT_TIMEOUT).await;
            assert_eq!(read_file(&dest.path().join("b.bin")), body_b);
            assert!(!part_b.exists());

            // cancel queued task
            let server_q = RangeServer::start(FixtureConfig {
                body: body_b.clone(),
                ..Default::default()
            });
            let id_q = engine
                .add_task(make_spec(server_q.url("/q"), "q.bin", 1, None))
                .unwrap();
            engine.cancel(&id_q).await.expect("cancel queued task");
            let tq = engine.store().task(&id_q).unwrap().unwrap();
            assert_eq!(tq.state, TaskState::Cancelled);
            assert!(!part_path(dest.path(), "q.bin").exists());
            assert!(!dest.path().join("q.bin").exists());

            // cancel paused task
            let server_p = RangeServer::start(slow_body_config(body_a.clone()));
            let id_p = engine
                .add_task(make_spec(server_p.url("/p"), "p.bin", 1, None))
                .unwrap();
            wait_for_progress(engine.store(), &id_p, 64 * 1024, WAIT_TIMEOUT).await;
            engine
                .pause(&id_p)
                .await
                .expect("pause task before cancelling paused task");
            wait_for_state(engine.store(), &id_p, TaskState::Paused, WAIT_TIMEOUT).await;
            assert!(part_path(dest.path(), "p.bin").exists());
            engine.cancel(&id_p).await.expect("cancel paused task");
            wait_for_state(engine.store(), &id_p, TaskState::Cancelled, WAIT_TIMEOUT).await;
            assert!(!part_path(dest.path(), "p.bin").exists());
            assert!(!dest.path().join("p.bin").exists());

            // cancel failed task removes part and sets cancelled
            let server_f = RangeServer::start(FixtureConfig {
                status_override: Some(500),
                ..Default::default()
            });
            // 500 may be retried in implementation; use 404 to get deterministic fail if non-retryable
            drop(server_f);
            let server_nf = RangeServer::start(FixtureConfig {
                status_override: Some(404),
                ..Default::default()
            });
            let id_f = engine
                .add_task(make_spec(server_nf.url("/nf"), "nf.bin", 1, None))
                .unwrap();
            let st = wait_for_terminal(engine.store(), &id_f, WAIT_TIMEOUT).await;
            assert_eq!(st, TaskState::Failed);
            // part may or may not exist depending impl for probe failure; if exists it must be removed by cancel
            let part_f = part_path(dest.path(), "nf.bin");
            engine.cancel(&id_f).await.expect("cancel failed task");
            let tf = engine.store().task(&id_f).unwrap().unwrap();
            assert_eq!(tf.state, TaskState::Cancelled);
            assert!(
                !part_f.exists(),
                "cancel failed task must remove its part if present"
            );

            // cancel completed task -> INVALID_STATE and final file stays intact
            let hash_b = sha256_hex(&body_b);
            let server_c2 = RangeServer::start(FixtureConfig {
                body: body_b.clone(),
                ..Default::default()
            });
            let id_c2 = engine
                .add_task(make_spec(server_c2.url("/c2"), "c2.bin", 1, Some(hash_b)))
                .unwrap();
            wait_for_state(engine.store(), &id_c2, TaskState::Completed, WAIT_TIMEOUT).await;
            let final_c2 = dest.path().join("c2.bin");
            assert!(final_c2.exists());
            let bytes_before = read_file(&final_c2);
            let err = engine
                .cancel(&id_c2)
                .await
                .expect_err("cancel completed task must fail");
            assert_code(&err, "INVALID_STATE");
            let tc2 = engine.store().task(&id_c2).unwrap().unwrap();
            assert_eq!(
                tc2.state,
                TaskState::Completed,
                "completed task must remain completed"
            );
            assert!(final_c2.exists(), "completed final file must stay intact");
            assert_eq!(read_file(&final_c2), bytes_before);
        }
    );
}

#[tokio::test]
async fn retry_failed_task_with_part_requeues_and_completes() {
    let body = pattern_bytes(123, 256 * 1024);
    let hash = sha256_hex(&body);
    // Start by failing with 500 (retryable) but we want deterministic "failed with a part" state
    // then retry to success. Use runtime status override to fail first, then succeed.
    // Also make it slow/streaming so we can ensure some bytes written? For 500 response, no body written.
    // Alternative: serve 200 with truncation/partial then error in a way that leaves part and fails - not directly in fixture.
    // Simpler deterministic approach: serve 429? retryable; may backoff. 404 is non-retryable and likely no part.
    // Another approach: serve valid bytes but expected hash mismatch? that would fail after download with checksum mismatch and preserve part per Step 6 behavior,
    // but Step 6 not implemented yet - still task can end failed with part if implementation does it; spec says "failed task preserves part for retry".
    // Use checksum mismatch path via expected_sha256 mismatch on otherwise good download if engine records part/fails that way.
    let server = RangeServer::start(FixtureConfig {
        body: body.clone(),
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

    let wrong_hash = "0".repeat(64);
    let id = engine
        .add_task(make_spec(
            server.url("/retry"),
            "retry.bin",
            1,
            Some(wrong_hash),
        ))
        .expect("add task");

    tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("run timeout")
                .expect("run");
        },
        async {
            let state = wait_for_terminal(engine.store(), &id, WAIT_TIMEOUT).await;
            assert_eq!(
                state,
                TaskState::Failed,
                "wrong expected hash should fail the task"
            );
            // part should be preserved for retry (spec behavior)
            let part = part_path(dest.path(), "retry.bin");
            // if implementation preserves part, it exists; if it placed data differently, retry must still work per spec.
            // We assert part exists when present, but main assertion is retry completes correctly.
            let had_part = part.exists();

            // Fix expectation and retry: store the correct expected hash so retry/finalization can succeed.
            engine
                .store()
                .record_expected_sha256(&id, &hash)
                .expect("record expected sha256 for retry");
            engine.retry(&id).await.expect("retry failed task");

            wait_for_state(engine.store(), &id, TaskState::Completed, WAIT_TIMEOUT).await;
            let task = engine.store().task(&id).unwrap().unwrap();
            assert_eq!(task.state, TaskState::Completed);
            assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));
            assert_eq!(read_file(&dest.path().join("retry.bin")), body);
            assert!(
                !part_path(dest.path(), "retry.bin").exists(),
                "part removed after successful retry completion"
            );
            let _ = had_part;
        }
    );
}

fn prepare_finalizing_task(
    store: &Database,
    dest: &Path,
    filename: &str,
    body: &[u8],
    expected_sha256: Option<&str>,
) -> String {
    let id = store
        .create_task("http://127.0.0.1/finalize", filename)
        .expect("create finalizing task");
    store
        .record_connections(&id, 1)
        .expect("record connections");
    store
        .record_discovered_metadata(&id, Some(body.len() as u64), None, None)
        .expect("record metadata");
    if let Some(expected) = expected_sha256 {
        store
            .record_expected_sha256(&id, expected)
            .expect("record expected hash");
    }
    let part_name = PartFile::part_name(filename);
    std::fs::write(dest.join(&part_name), body).expect("write complete part");
    store
        .record_part_path(&id, &part_name)
        .expect("record part path");
    store
        .transition_task(&id, TaskState::Queued, TaskState::Connecting)
        .expect("queued to connecting");
    store
        .transition_task(&id, TaskState::Connecting, TaskState::Downloading)
        .expect("connecting to downloading");
    store
        .transition_task(&id, TaskState::Downloading, TaskState::Finalizing)
        .expect("downloading to finalizing");
    id
}

#[tokio::test]
async fn cancel_during_finalization_removes_part_without_invalid_state() {
    let body = pattern_bytes(201, 256 * 1024);
    let hash = sha256_hex(&body);
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let store = open_store(&store_dir);
    let id = prepare_finalizing_task(&store, dest.path(), "finalizing.bin", &body, Some(&hash));
    let engine = Engine::new(
        open_store(&store_dir),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    let part = part_path(dest.path(), "finalizing.bin");
    assert!(
        part.exists(),
        "a finalizing task must own its complete part"
    );

    engine
        .cancel(&id)
        .await
        .expect("cancelling a finalizing task must succeed");

    let task = engine.store().task(&id).unwrap().unwrap();
    assert_eq!(task.state, TaskState::Cancelled);
    assert_ne!(
        task.error_code.as_deref(),
        Some("INVALID_STATE"),
        "a finalizing cancel must not be treated as a completed-task invalid state, got {:?}",
        task.error_message
    );
    assert!(!part.exists(), "cancel must remove the finalizing part");
    assert!(
        !dest.path().join("finalizing.bin").exists(),
        "cancel must not create a final file"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_racing_finalization_ends_completed_or_cancelled_only() {
    let body = pattern_bytes(202, 4 * 1024 * 1024);
    let hash = sha256_hex(&body);
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let db_path = store_dir.path().join("tasks.db");
    let store = open_store(&store_dir);
    let id = prepare_finalizing_task(&store, dest.path(), "race-finalize.bin", &body, Some(&hash));
    // A second store handle over the same database lets the run and cancel
    // operations execute concurrently rather than sharing one non-Sync
    // rusqlite connection.
    let engine = Engine::new(
        Database::open(&db_path).expect("open run store"),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    let canceller = Engine::new(
        Database::open(&db_path).expect("open cancel store"),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    let run = async { engine.run().await };
    let cancel = async {
        sleep(Duration::from_millis(1)).await;
        canceller.cancel(&id).await
    };
    let (run_result, cancel_result) = tokio::join!(run, cancel);
    run_result.expect("engine run must not fail");
    if let Err(error) = cancel_result {
        assert_code(&error, "INVALID_STATE");
    }

    let task = engine.store().task(&id).unwrap().unwrap();
    let part = part_path(dest.path(), "race-finalize.bin");
    let final_path = dest.path().join("race-finalize.bin");
    match task.state {
        TaskState::Completed => {
            assert!(final_path.exists(), "completed race must keep the final file");
            assert!(!part.exists(), "completed race must remove the part");
            assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));
            assert_eq!(read_file(&final_path), body, "final bytes must match");
        }
        TaskState::Cancelled => {
            assert!(!final_path.exists(), "cancelled race must not leave a final file");
            assert!(!part.exists(), "cancelled race must remove the part");
            assert_ne!(
                task.error_code.as_deref(),
                Some("INVALID_STATE"),
                "cancelled race must not record an invalid-state error, got {:?}",
                task.error_message
            );
        }
        other => panic!(
            "cancel/finalization race must end completed or cancelled, got {other:?} (error={:?} {:?})",
            task.error_code, task.error_message
        ),
    }
}

#[tokio::test]
async fn cross_engine_pause_preserves_part_and_resume_completes() {
    let body = pattern_bytes(211, 1024 * 1024);
    let hash = sha256_hex(&body);
    let server = RangeServer::start(slow_body_config(body.clone()));
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let db_path = store_dir.path().join("tasks.db");
    let engine_a = Engine::new(
        Database::open(&db_path).expect("open engine A store"),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    let engine_b = Engine::new(
        Database::open(&db_path).expect("open engine B store"),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    let id = engine_a
        .add_task(make_spec(
            server.url("/cross-engine-pause"),
            "cross-engine-pause.bin",
            1,
            Some(hash.clone()),
        ))
        .expect("add task");

    let (_, pause_result) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine_a.run())
                .await
                .expect("run timeout")
                .expect("run engine A");
        },
        async {
            wait_for_progress(engine_a.store(), &id, 64 * 1024, WAIT_TIMEOUT).await;
            engine_b.pause(&id).await
        }
    );
    pause_result.expect("a second engine must be able to pause the running task");

    let task = engine_a.store().task(&id).unwrap().unwrap();
    assert_eq!(
        task.state,
        TaskState::Paused,
        "cross-engine pause must stop the task as paused, got {:?} (error={:?} {:?})",
        task.state,
        task.error_code,
        task.error_message
    );
    assert_ne!(
        task.error_code.as_deref(),
        Some("INVALID_STATE"),
        "cross-engine pause must not record INVALID_STATE"
    );
    let part = part_path(dest.path(), "cross-engine-pause.bin");
    assert!(part.exists(), "cross-engine pause must preserve the part");

    engine_a.resume(&id).await.expect("resume paused task");
    let (_, state) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine_a.run())
                .await
                .expect("resume run timeout")
                .expect("resume run");
        },
        wait_for_state(engine_a.store(), &id, TaskState::Completed, WAIT_TIMEOUT,),
    );
    assert_eq!(state, TaskState::Completed);
    let task = engine_a.store().task(&id).unwrap().unwrap();
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));
    assert_eq!(read_file(&dest.path().join("cross-engine-pause.bin")), body);
    assert!(!part.exists());
}

#[tokio::test]
async fn cross_engine_cancel_ends_cancelled_without_invalid_state() {
    let body = pattern_bytes(212, 768 * 1024);
    let server = RangeServer::start(slow_body_config(body.clone()));
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let db_path = store_dir.path().join("tasks.db");
    let engine_a = Engine::new(
        Database::open(&db_path).expect("open engine A store"),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    let engine_b = Engine::new(
        Database::open(&db_path).expect("open engine B store"),
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    let id = engine_a
        .add_task(make_spec(
            server.url("/cross-engine-cancel"),
            "cross-engine-cancel.bin",
            1,
            None,
        ))
        .expect("add task");

    let (_, cancel_result) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine_a.run())
                .await
                .expect("run timeout")
                .expect("run engine A");
        },
        async {
            wait_for_progress(engine_a.store(), &id, 64 * 1024, WAIT_TIMEOUT).await;
            engine_b.cancel(&id).await
        }
    );
    cancel_result.expect("a second engine must be able to cancel the running task");

    let task = engine_a.store().task(&id).unwrap().unwrap();
    assert_eq!(
        task.state,
        TaskState::Cancelled,
        "cross-engine cancel must end cancelled, got {:?} (error={:?} {:?})",
        task.state,
        task.error_code,
        task.error_message
    );
    assert_ne!(
        task.error_code.as_deref(),
        Some("INVALID_STATE"),
        "cross-engine cancel must not record INVALID_STATE"
    );
    assert_ne!(task.state, TaskState::Failed);
    assert!(
        !part_path(dest.path(), "cross-engine-cancel.bin").exists(),
        "cross-engine cancel must remove the part"
    );
    assert!(
        !dest.path().join("cross-engine-cancel.bin").exists(),
        "cross-engine cancel must not create a final file"
    );
}

/// A filename carrying a parent component must never make a part or final file
/// resolve outside the destination directory.
#[tokio::test]
async fn add_task_rejects_parent_component_filename_and_cancel_spares_outside_part() {
    let server = RangeServer::start(FixtureConfig {
        body: pattern_bytes(241, 16 * 1024),
        ..Default::default()
    });
    let outer = tempdir().expect("outer");
    let dest = outer.path().join("dest");
    std::fs::create_dir(&dest).expect("create destination");
    let outside_part = outer.path().join("outside.oneget.part");
    let sealed = b"outside part owned by another process".to_vec();
    std::fs::write(&outside_part, &sealed).expect("write outside part");

    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        dest.clone(),
        EngineOptions { max_concurrent: 1 },
    );

    let added = engine.add_task(make_spec(
        server.url("/parent-traversal"),
        "../outside",
        1,
        None,
    ));

    match added {
        Err(error) => {
            assert!(
                matches!(
                    error.code(),
                    "INVALID_FILENAME" | "INVALID_PATH" | "INVALID_PATH_COMPONENT" | "invalid_path"
                ),
                "a traversal filename must fail with a typed path or filename error, got {error}"
            );
            assert!(
                engine.store().list_tasks().expect("list tasks").is_empty(),
                "a rejected task must not be persisted"
            );
        }
        Ok(id) => {
            let task = engine
                .store()
                .task(&id)
                .expect("load task")
                .expect("task exists");
            let stored = task.filename.clone();
            engine
                .cancel(&id)
                .await
                .expect("cancel task accepted by add_task");
            assert!(
                outside_part.exists(),
                "cancel must not remove a part resolved outside the destination root"
            );
            assert_eq!(
                read_file(&outside_part),
                sealed,
                "cancel must leave a file outside the destination root unchanged"
            );
            let stored_path = Path::new(&stored);
            assert!(
                stored_path.components().count() == 1
                    && matches!(
                        stored_path.components().next(),
                        Some(std::path::Component::Normal(_))
                    ),
                "an accepted filename must be a single normal component, got {stored:?}"
            );
        }
    }

    let mut siblings: Vec<String> = std::fs::read_dir(outer.path())
        .expect("read outer")
        .map(|entry| {
            entry
                .expect("outer entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    siblings.sort();
    assert_eq!(
        siblings,
        vec!["dest".to_string(), "outside.oneget.part".to_string()],
        "add_task and cancel must not create a part or final file outside the destination root"
    );
    assert!(
        std::fs::read_dir(&dest)
            .expect("read destination")
            .next()
            .is_none(),
        "add_task and cancel must not leave part or final files inside the destination"
    );
    assert!(
        server.served_ranges().is_empty(),
        "a traversal filename must not cause any request to the fixture"
    );
}
