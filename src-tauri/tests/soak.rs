//! Short CI smoke and ignored long-running HTTP-only soak for the download
//! engine.
//!
//! Every iteration uses fresh temporary directories, local ephemeral-port
//! fixtures, and deterministic fault modes, so no public network is touched and
//! disk and memory stay bounded across a multi-hour run.

mod support;

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Once;
use std::time::Duration;

use oneget_lib::engine::scheduler::{Engine, EngineOptions};
use oneget_lib::engine::state::TaskState;
use oneget_lib::engine::worker::DownloadSpec;
use oneget_lib::store::Database;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use support::fixtures::pattern_bytes;
use support::range_server::{FixtureConfig, RangeMode, RangeServer};
use tempfile::tempdir;
use tokio::time::{sleep, timeout, Instant};

/// The documented default soak length: 24 hours.
const DEFAULT_SOAK_SECONDS: u64 = 86_400;
/// Bound for one iteration, so a stuck run fails instead of hanging CI.
const ITERATION_TIMEOUT: Duration = Duration::from_secs(90);
/// How often task state is re-read while waiting for a control transition.
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Progress threshold that proves a slow task is actually downloading.
const PAUSE_PROGRESS_BYTES: u64 = 128 * 1024;

#[derive(Default)]
struct IterationSummary {
    tasks: usize,
    completed: usize,
    cancelled: usize,
    failed: usize,
    retried: usize,
    bytes: u64,
    orphan_parts: usize,
    unhandled_panics: usize,
}

#[derive(Default)]
struct SoakTotals {
    iterations: u64,
    tasks: u64,
    completed: u64,
    cancelled: u64,
    failed: u64,
    retried: u64,
    bytes: u64,
    orphan_parts: u64,
    unhandled_panics: u64,
    integrity_ok: bool,
}

impl SoakTotals {
    fn add(&mut self, summary: IterationSummary) {
        self.iterations += 1;
        self.tasks += summary.tasks as u64;
        self.completed += summary.completed as u64;
        self.cancelled += summary.cancelled as u64;
        self.failed += summary.failed as u64;
        self.retried += summary.retried as u64;
        self.bytes += summary.bytes;
        self.orphan_parts += summary.orphan_parts as u64;
        self.unhandled_panics += summary.unhandled_panics as u64;
        // A summary only returns after its integrity check succeeded.
        self.integrity_ok = true;
    }
}

static PANIC_COUNT: AtomicUsize = AtomicUsize::new(0);
static PANIC_HOOK: Once = Once::new();

/// Count every panic raised in the process, including detached fixture threads,
/// while preserving the default hook output.
fn panic_count() -> usize {
    PANIC_HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            PANIC_COUNT.fetch_add(1, Ordering::SeqCst);
            previous(info);
        }));
    });
    PANIC_COUNT.load(Ordering::SeqCst)
}

fn soak_duration() -> Duration {
    match std::env::var("ONEGET_SOAK_SECONDS") {
        Ok(raw) => {
            let seconds: u64 = raw
                .trim()
                .parse()
                .expect("ONEGET_SOAK_SECONDS must be whole seconds");
            Duration::from_secs(seconds)
        }
        Err(_) => Duration::from_secs(DEFAULT_SOAK_SECONDS),
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn make_spec(
    url: String,
    filename: &str,
    connections: u32,
    expected_sha256: String,
) -> DownloadSpec {
    DownloadSpec {
        url,
        filename: filename.to_string(),
        connections,
        allow_html: false,
        expected_sha256: Some(expected_sha256),
        extra_headers: Vec::new(),
    }
}

fn part_path(dest: &Path, filename: &str) -> std::path::PathBuf {
    dest.join(format!("{filename}.oneget.part"))
}

fn integrity_check(database: &Path) -> bool {
    let connection = Connection::open(database).expect("open database for integrity check");
    connection
        .busy_timeout(Duration::from_secs(5))
        .expect("set integrity check busy timeout");
    let result: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .expect("run PRAGMA integrity_check");
    result == "ok"
}

async fn wait_for_state(
    store: &Database,
    id: &str,
    expected: TaskState,
    deadline: Instant,
) -> TaskState {
    loop {
        let task = store.task(id).expect("load task").expect("task exists");
        if task.state == expected {
            return task.state;
        }
        if matches!(
            task.state,
            TaskState::Completed | TaskState::Failed | TaskState::Cancelled
        ) {
            panic!(
                "task {id} reached {:?} while waiting for {:?} (error={:?} {:?})",
                task.state, expected, task.error_code, task.error_message
            );
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for task {id} to reach {:?}, current {:?}",
                expected, task.state
            );
        }
        sleep(POLL_INTERVAL).await;
    }
}

async fn wait_for_download_progress(
    store: &Database,
    id: &str,
    minimum_bytes: u64,
    deadline: Instant,
) -> u64 {
    loop {
        let task = store.task(id).expect("load task").expect("task exists");
        if task.downloaded_bytes >= minimum_bytes {
            return task.downloaded_bytes;
        }
        match task.state {
            TaskState::Connecting | TaskState::Downloading => {}
            TaskState::Completed => {
                panic!("task {id} completed before reaching {minimum_bytes} downloaded bytes")
            }
            other => panic!(
                "task {id} reached {other:?} before reaching {minimum_bytes} downloaded bytes"
            ),
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for task {id} to reach {minimum_bytes} downloaded bytes, current {}",
                task.downloaded_bytes
            );
        }
        sleep(POLL_INTERVAL).await;
    }
}

/// Run one full soak iteration and verify every invariant it promises.
async fn run_iteration(iteration: u64) -> IterationSummary {
    let panics_before = panic_count();
    let dest = tempdir().expect("destination temp dir");
    let store_dir = tempdir().expect("store temp dir");
    let database_path = store_dir.path().join("tasks.db");
    let store = Database::open(&database_path).expect("open task store");
    let engine = Engine::new(
        store,
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 8 },
    );

    let main_body = pattern_bytes(101, 4 * 1024 * 1024);
    let slow_body = pattern_bytes(102, 2 * 1024 * 1024);
    let truncated_body = pattern_bytes(103, 2 * 1024 * 1024);
    let flaky_body = pattern_bytes(104, 2 * 1024 * 1024);
    let rate_limited_body = pattern_bytes(105, 2 * 1024 * 1024);
    let retry_body = pattern_bytes(106, 1024 * 1024);

    let main_server = RangeServer::start(FixtureConfig {
        body: main_body.clone(),
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let slow_server = RangeServer::start(FixtureConfig {
        body: slow_body.clone(),
        range_mode: RangeMode::HonorRange,
        stream_delay: Some(Duration::from_millis(10)),
        stream_chunk_size: 16 * 1024,
        ..Default::default()
    });
    let truncated_server = RangeServer::start(FixtureConfig {
        body: truncated_body.clone(),
        truncate_first_ranged_after: Some(256 * 1024),
        truncate_ranged_times: 1,
        ..Default::default()
    });
    let flaky_server = RangeServer::start(FixtureConfig {
        body: flaky_body.clone(),
        fail_first_n: 2,
        fail_status: 503,
        ..Default::default()
    });
    let rate_limited_server = RangeServer::start(FixtureConfig {
        body: rate_limited_body.clone(),
        rate_limit_429: true,
        rate_limit_times: Some(2),
        retry_after: Some("0".to_string()),
        ..Default::default()
    });
    let retry_server = RangeServer::start(FixtureConfig {
        body: retry_body.clone(),
        ..Default::default()
    });
    // Fail the retry task non-retryably until the control flow retries it.
    retry_server.set_runtime_status_override(Some(404));

    let prefix = format!("soak-{iteration}");
    let main_hash = sha256_hex(&main_body);
    let slow_hash = sha256_hex(&slow_body);
    let truncated_hash = sha256_hex(&truncated_body);
    let flaky_hash = sha256_hex(&flaky_body);
    let rate_limited_hash = sha256_hex(&rate_limited_body);
    let retry_hash = sha256_hex(&retry_body);

    let add =
        |server_path: &str, filename: &str, connections: u32, hash: &str, server: &RangeServer| {
            engine
                .add_task(make_spec(
                    server.url(server_path),
                    filename,
                    connections,
                    hash.to_string(),
                ))
                .expect("add soak task")
        };

    let _main_a = add(
        "/main-a",
        &format!("{prefix}-main-a.bin"),
        4,
        &main_hash,
        &main_server,
    );
    let _main_b = add(
        "/main-b",
        &format!("{prefix}-main-b.bin"),
        4,
        &main_hash,
        &main_server,
    );
    let _truncated = add(
        "/truncated-once",
        &format!("{prefix}-truncated.bin"),
        2,
        &truncated_hash,
        &truncated_server,
    );
    let _flaky = add(
        "/transient-5xx",
        &format!("{prefix}-flaky.bin"),
        2,
        &flaky_hash,
        &flaky_server,
    );
    let _rate_limited = add(
        "/rate-limited",
        &format!("{prefix}-rate-limited.bin"),
        2,
        &rate_limited_hash,
        &rate_limited_server,
    );
    let paused = add(
        "/pause-resume",
        &format!("{prefix}-pause-resume.bin"),
        2,
        &slow_hash,
        &slow_server,
    );
    let cancelled = add(
        "/cancel",
        &format!("{prefix}-cancel.bin"),
        2,
        &slow_hash,
        &slow_server,
    );
    let retried = add(
        "/retry",
        &format!("{prefix}-retry.bin"),
        1,
        &retry_hash,
        &retry_server,
    );

    let deadline = Instant::now() + ITERATION_TIMEOUT;

    let run_engine = async {
        timeout(ITERATION_TIMEOUT, engine.run())
            .await
            .expect("soak iteration timed out")
            .expect("engine run failed");
    };
    let drive_controls = async {
        wait_for_download_progress(engine.store(), &cancelled, PAUSE_PROGRESS_BYTES, deadline)
            .await;
        engine
            .cancel(&cancelled)
            .await
            .expect("cancel an in-flight task");
        wait_for_state(engine.store(), &cancelled, TaskState::Cancelled, deadline).await;

        wait_for_download_progress(engine.store(), &paused, PAUSE_PROGRESS_BYTES, deadline).await;
        engine
            .pause(&paused)
            .await
            .expect("pause an in-flight task");
        wait_for_state(engine.store(), &paused, TaskState::Paused, deadline).await;
        engine.resume(&paused).await.expect("resume a paused task");

        wait_for_state(engine.store(), &retried, TaskState::Failed, deadline).await;
        retry_server.set_runtime_status_override(None);
        engine.retry(&retried).await.expect("retry a failed task");
    };

    tokio::join!(run_engine, drive_controls);
    // Engine::run returns once no queued or active task remains.

    let tasks = engine.store().list_tasks().expect("list soak tasks");
    let mut summary = IterationSummary {
        tasks: tasks.len(),
        retried: 1,
        ..Default::default()
    };

    for task in &tasks {
        match task.state {
            TaskState::Completed => {
                summary.completed += 1;
                let expected = task
                    .expected_sha256
                    .as_deref()
                    .expect("completed soak task carries an expected hash");
                assert_eq!(
                    task.actual_sha256.as_deref(),
                    Some(expected),
                    "stored hash mismatch for {}",
                    task.filename
                );
                let final_path = dest.path().join(&task.filename);
                let bytes = std::fs::read(&final_path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", task.filename));
                assert_eq!(
                    sha256_hex(&bytes),
                    expected,
                    "final SHA-256 mismatch for {}",
                    task.filename
                );
                summary.bytes += bytes.len() as u64;
                assert!(
                    !part_path(dest.path(), &task.filename).exists(),
                    "completed task {} left a part file",
                    task.filename
                );
            }
            TaskState::Cancelled => {
                summary.cancelled += 1;
                assert!(
                    !part_path(dest.path(), &task.filename).exists(),
                    "cancelled task {} left a part file",
                    task.filename
                );
                assert!(
                    !dest.path().join(&task.filename).exists(),
                    "cancelled task {} produced a final file",
                    task.filename
                );
            }
            TaskState::Failed => {
                panic!(
                    "task {} ended failed: {:?} {:?}",
                    task.filename, task.error_code, task.error_message
                );
            }
            other => panic!("task {} ended in unexpected state {other:?}", task.filename),
        }
    }

    let orphans = std::fs::read_dir(dest.path())
        .expect("scan destination")
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .ends_with(".oneget.part")
        })
        .count();
    assert_eq!(orphans, 0, "orphan part files remain in the destination");
    summary.orphan_parts = orphans;

    assert_eq!(summary.tasks, 8, "soak iteration must add eight tasks");
    assert_eq!(
        summary.completed, 7,
        "soak iteration must finish seven tasks"
    );
    assert_eq!(summary.cancelled, 1, "soak iteration must cancel one task");
    summary.unhandled_panics = panic_count().saturating_sub(panics_before);
    assert_eq!(
        summary.unhandled_panics, 0,
        "a thread panicked during the soak iteration"
    );
    assert!(
        integrity_check(&database_path),
        "PRAGMA integrity_check did not return ok"
    );

    summary
}

fn print_summary(mode: &str, totals: &SoakTotals) {
    let integrity = if totals.integrity_ok { "ok" } else { "not-run" };
    eprintln!(
        "soak summary: mode={mode} iterations={} tasks={} completed={} cancelled={} failed={} retried={} bytes={} integrity={integrity} orphan_parts={} unhandled_panics={}",
        totals.iterations,
        totals.tasks,
        totals.completed,
        totals.cancelled,
        totals.failed,
        totals.retried,
        totals.bytes,
        totals.orphan_parts,
        totals.unhandled_panics,
    );
}

#[tokio::test]
async fn short_smoke_exercises_one_deterministic_soak_iteration() {
    let summary = run_iteration(0).await;
    let mut totals = SoakTotals::default();
    totals.add(summary);
    print_summary("smoke", &totals);
}

#[tokio::test]
#[ignore = "long-running HTTP-only soak; use scripts/soak-test.sh or ONEGET_SOAK_SECONDS"]
async fn http_only_soak_repeats_the_task_lifecycle() {
    let duration = soak_duration();
    let started = Instant::now();
    let mut totals = SoakTotals::default();
    let mut iteration = 0u64;

    loop {
        let summary = run_iteration(iteration).await;
        totals.add(summary);
        iteration += 1;
        // A zero or very short duration still runs exactly one iteration.
        if started.elapsed() >= duration {
            break;
        }
    }

    print_summary("soak", &totals);
}
