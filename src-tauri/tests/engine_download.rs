//! Step 3 RED: behavior tests for segment download, worker pool, and atomic finalization.
//!
//! Intended public interface under test:
//! - `oneget_lib::engine::worker::{download_once, finalize_task, DownloadSpec, DownloadOutcome, FinalizeOutcome}`
//! - Authorized store additions: `Database::persist_progress`, `Database::record_actual_hash`,
//!   `Database::record_discovered_metadata`.
//!
//! Expected RED: build failure because planner/part_file/worker modules and new store/platform APIs
//! do not yet exist.

mod support;

#[path = "support/faults.rs"]
mod faults;

use std::path::{Path, PathBuf};
use std::time::Duration;

use oneget_lib::engine::error::EngineError;
use oneget_lib::engine::part_file::PartFile;
use oneget_lib::engine::scheduler::{Engine, EngineOptions};
use oneget_lib::engine::state::{SegmentState, TaskState};
use oneget_lib::engine::worker::{
    download_once, finalize_task, DownloadOutcome, DownloadSpec, FinalizeOutcome,
};
use oneget_lib::platform::files::CapDir;
use oneget_lib::store::types::{SegmentInput, SegmentProgressInput};
use oneget_lib::store::Database;
use sha2::{Digest, Sha256};
use support::fixtures::pattern_bytes;
use support::range_server::{FixtureConfig, RangeMode, RangeServer};
use tempfile::tempdir;
use tokio::time::{sleep, timeout, Instant};

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    digest_to_hex(hasher.finalize())
}

fn digest_to_hex(digest: impl AsRef<[u8]>) -> String {
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
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

fn make_spec(url: String, filename: &str, connections: u32) -> DownloadSpec {
    make_spec_with(url, filename, connections, false, None)
}

fn make_spec_with(
    url: String,
    filename: &str,
    connections: u32,
    allow_html: bool,
    expected_sha256: Option<String>,
) -> DownloadSpec {
    DownloadSpec {
        url,
        filename: filename.to_string(),
        connections,
        allow_html,
        expected_sha256,
        extra_headers: Vec::new(),
        destination: None,
    }
}

const POLL_INTERVAL: Duration = Duration::from_millis(15);
const WAIT_TIMEOUT: Duration = Duration::from_secs(20);

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

async fn run_engine_to_terminal(engine: &Engine, id: &str) -> TaskState {
    let ((), observed) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("engine run timed out")
                .expect("engine run failed");
        },
        wait_for_terminal(engine.store(), id, WAIT_TIMEOUT),
    );
    observed
}

fn read_file(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn count_range_requests(server: &RangeServer) -> usize {
    server
        .requests()
        .iter()
        .filter(|r| r.range.is_some())
        .count()
}

#[tokio::test]
async fn download_once_8mib_8_connections_completes_with_matching_hash_progress_and_cleanup() {
    let body = pattern_bytes(42, 8 * 1024 * 1024);
    let expected_hash = sha256_hex(&body);
    let server = RangeServer::start(FixtureConfig {
        body: body.clone(),
        range_mode: RangeMode::HonorRange,
        content_type: Some("application/octet-stream".to_string()),
        ..Default::default()
    });
    let url = server.url("/file-8m.bin");

    let dest_root = tempdir().expect("dest tempdir");
    let store_root = tempdir().expect("store tempdir");
    let store = open_store(&store_root);

    let filename = "file-8m.bin".to_string();
    let task_id = store.create_task(&url, &filename).expect("create task");
    let spec = make_spec(url.clone(), &filename, 8);

    let outcome: DownloadOutcome = download_once(&store, &task_id, dest_root.path(), &spec)
        .await
        .expect("download_once should complete");

    assert_eq!(
        outcome.actual_sha256, expected_hash,
        "outcome actual_sha256 must match fixture bytes"
    );
    assert_eq!(
        outcome.total_bytes,
        body.len() as u64,
        "total_bytes must match"
    );
    assert_eq!(
        read_file(&outcome.final_path),
        body,
        "final file bytes must match fixture"
    );
    assert!(
        outcome.final_path.starts_with(dest_root.path()),
        "final path must be under destination directory"
    );

    let range_count = count_range_requests(&server);
    assert!(
        range_count >= 8,
        "at least 8 requests must carry a Range header, got {range_count}"
    );

    let part_path = dest_root.path().join(PartFile::part_name(&filename));
    assert!(
        !part_path.exists(),
        "part file must not exist after successful completion: {}",
        part_path.display()
    );

    let task = store
        .task(&task_id)
        .expect("load task")
        .expect("task exists");
    assert_eq!(task.state, TaskState::Completed, "task must be completed");
    assert_eq!(
        task.actual_sha256.as_deref(),
        Some(expected_hash.as_str()),
        "stored actual_sha256 must match"
    );
}

#[tokio::test]
async fn engine_task_uses_its_persisted_destination_for_part_and_final_files() {
    let body = pattern_bytes(73, 2 * 1024 * 1024);
    let server = RangeServer::start(FixtureConfig {
        body: body.clone(),
        range_mode: RangeMode::HonorRange,
        stream_delay: Some(Duration::from_millis(8)),
        stream_chunk_size: 16 * 1024,
        ..Default::default()
    });
    let default_dest = tempdir().expect("default destination");
    let custom_dest = tempdir().expect("custom destination");
    let store_dir = tempdir().expect("store");
    let engine = Engine::new(
        open_store(&store_dir),
        default_dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );

    let filename = "custom-destination.bin";
    let mut spec = make_spec(server.url("/custom-destination"), filename, 2);
    spec.destination = Some(custom_dest.path().to_path_buf());
    let task_id = engine
        .add_task(spec)
        .expect("add task with a custom destination");

    let task = engine
        .store()
        .task(&task_id)
        .expect("load task")
        .expect("task exists");
    let stored_destination = task
        .destination
        .as_deref()
        .expect("the resolved destination must be persisted");
    assert_eq!(
        std::fs::canonicalize(stored_destination).expect("canonical stored destination"),
        std::fs::canonicalize(custom_dest.path()).expect("canonical custom destination"),
        "the persisted destination must resolve to the requested directory"
    );

    let part_name = PartFile::part_name(filename);
    let ((), ()) = tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("engine run timed out")
                .expect("engine run failed");
        },
        async {
            let deadline = Instant::now() + WAIT_TIMEOUT;
            loop {
                if custom_dest.path().join(&part_name).exists() {
                    assert!(
                        !default_dest.path().join(&part_name).exists(),
                        "the part file must not be written in the engine default destination"
                    );
                    break;
                }
                let state = engine
                    .store()
                    .task(&task_id)
                    .expect("load task")
                    .expect("task exists")
                    .state;
                assert!(
                    !matches!(
                        state,
                        TaskState::Completed | TaskState::Failed | TaskState::Cancelled
                    ),
                    "task reached {state:?} before its part file appeared in the custom destination"
                );
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for the part file in the custom destination"
                );
                sleep(POLL_INTERVAL).await;
            }
        }
    );

    let finished = engine
        .store()
        .task(&task_id)
        .expect("load task")
        .expect("task exists");
    assert_eq!(
        finished.state,
        TaskState::Completed,
        "the task must complete"
    );

    let final_path = custom_dest.path().join(filename);
    assert!(
        final_path.exists(),
        "the final file must be written in the custom destination"
    );
    assert_eq!(
        read_file(&final_path),
        body,
        "the final file bytes must match the fixture"
    );
    assert!(
        !default_dest.path().join(filename).exists(),
        "no final file may be written in the engine default destination"
    );
    assert!(
        !custom_dest.path().join(&part_name).exists(),
        "the part file must be removed after completion"
    );
}

#[tokio::test]
async fn download_once_destination_collision_preserves_old_file_and_writes_new_path() {
    let body = pattern_bytes(7, 256 * 1024);
    let expected_hash = sha256_hex(&body);
    let server = RangeServer::start(FixtureConfig {
        body: body.clone(),
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let url = server.url("/collision.bin");

    let dest_root = tempdir().expect("dest tempdir");
    let store_root = tempdir().expect("store tempdir");
    let store = open_store(&store_root);

    let filename = "file.bin".to_string();
    let existing_path = dest_root.path().join(&filename);
    let old_bytes = b"distinct-old-content-that-must-remain".to_vec();
    std::fs::write(&existing_path, &old_bytes).expect("pre-create existing destination file");

    let task_id = store.create_task(&url, &filename).expect("create task");
    let spec = make_spec(url, &filename, 4);

    let outcome = download_once(&store, &task_id, dest_root.path(), &spec)
        .await
        .expect("download_once should complete despite destination collision");

    assert_ne!(
        outcome.final_path, existing_path,
        "download must complete to a different path when destination collides"
    );
    assert_eq!(
        std::fs::read(&existing_path).expect("read old file"),
        old_bytes,
        "pre-existing destination file must remain unchanged"
    );
    assert_eq!(
        read_file(&outcome.final_path),
        body,
        "new final file must contain downloaded bytes"
    );
    assert_eq!(
        outcome.actual_sha256, expected_hash,
        "new file hash must match downloaded bytes"
    );
}

#[tokio::test]
async fn download_once_zero_byte_fixture_completes_with_empty_final_and_no_part() {
    let server = RangeServer::start(FixtureConfig {
        body: Vec::new(),
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let url = server.url("/empty.bin");

    let dest_root = tempdir().expect("dest tempdir");
    let store_root = tempdir().expect("store tempdir");
    let store = open_store(&store_root);

    let filename = "empty.bin".to_string();
    let task_id = store.create_task(&url, &filename).expect("create task");
    let spec = make_spec(url, &filename, 4);

    let outcome = download_once(&store, &task_id, dest_root.path(), &spec)
        .await
        .expect("zero-byte download must complete");

    let expected_hash = sha256_hex(&[]);
    assert_eq!(
        outcome.actual_sha256, expected_hash,
        "empty file SHA-256 must match"
    );
    assert_eq!(outcome.total_bytes, 0);
    assert!(outcome.final_path.exists(), "final empty file must exist");
    assert_eq!(
        read_file(&outcome.final_path),
        Vec::<u8>::new(),
        "final file must be empty"
    );

    let part_path = dest_root.path().join(PartFile::part_name(&filename));
    assert!(
        !part_path.exists(),
        "no part file should remain for zero-byte completion"
    );

    let task = store
        .task(&task_id)
        .expect("load task")
        .expect("task exists");
    assert_eq!(task.state, TaskState::Completed);
}

async fn prepare_finalizing_task_with_complete_part(
    store: &Database,
    dest: &Path,
    task_id: &str,
    filename: &str,
    bytes: &[u8],
) -> PathBuf {
    let cap = CapDir::open(dest).expect("open dest capdir");
    let part_name = PartFile::part_name(filename);
    cap.write(&part_name, bytes)
        .await
        .expect("write complete part bytes");
    store
        .record_part_path(task_id, &part_name)
        .expect("record part path");

    // Move task into finalizing state: queued -> connecting -> downloading -> finalizing
    store
        .transition_task(task_id, TaskState::Queued, TaskState::Connecting)
        .expect("queued->connecting");
    store
        .transition_task(task_id, TaskState::Connecting, TaskState::Downloading)
        .expect("connecting->downloading");
    store
        .transition_task(task_id, TaskState::Downloading, TaskState::Finalizing)
        .expect("downloading->finalizing");

    // Persist segments done and discovered metadata for recovery/finalization context.
    let total = bytes.len() as u64;
    let segments = vec![SegmentInput {
        index: 0,
        start: 0,
        end: total.max(1), // represent full range; for empty, keep a coherent range marker if needed
        downloaded: total,
        state: SegmentState::Done,
    }];
    // For empty files, replace_segments may have specific expectations; still attempt setup.
    if total > 0 {
        store
            .replace_segments(task_id, &segments)
            .expect("replace segments as done");
        let progress = vec![SegmentProgressInput {
            segment_id: 0,
            downloaded_bytes: total,
        }];
        store
            .persist_progress(task_id, &progress, total)
            .expect("persist progress for complete segments");
    } else {
        // Keep progress/discovered metadata recorded for zero-byte finalization setup.
        store
            .persist_progress(task_id, &[], 0)
            .expect("persist zero-byte progress");
    }
    store
        .record_discovered_metadata(task_id, total, None, None)
        .expect("record discovered metadata");

    cap.root().join(&part_name)
}

#[tokio::test]
async fn finalize_task_is_idempotent_and_does_not_issue_additional_requests() {
    let body = pattern_bytes(21, 128 * 1024);
    let expected_hash = sha256_hex(&body);
    let server = RangeServer::start(FixtureConfig {
        body: body.clone(),
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let _url = server.url("/unused.bin"); // server used only to assert request count stays stable

    let dest_root = tempdir().expect("dest tempdir");
    let store_root = tempdir().expect("store tempdir");
    let store = open_store(&store_root);
    let filename = "resume.bin".to_string();
    let task_id = store
        .create_task("http://127.0.0.1/unused.bin", &filename)
        .expect("create task");

    prepare_finalizing_task_with_complete_part(
        &store,
        dest_root.path(),
        &task_id,
        &filename,
        &body,
    )
    .await;
    let requests_before = server.requests().len();

    let out1: FinalizeOutcome = finalize_task(&store, &task_id, dest_root.path(), None)
        .expect("first finalize_task must succeed");
    let out2: FinalizeOutcome = finalize_task(&store, &task_id, dest_root.path(), None)
        .expect("second finalize_task must be idempotent");

    assert_eq!(
        out1.final_path, out2.final_path,
        "idempotent calls must return same final path"
    );
    assert!(out1.final_path.exists(), "final file must exist");
    assert_eq!(
        read_file(&out1.final_path),
        body,
        "final bytes must match part contents"
    );

    let part_path = dest_root.path().join(PartFile::part_name(&filename));
    assert!(
        !part_path.exists(),
        "part file must be gone after finalization"
    );

    // Exactly one final file at outcome path (and no extra sibling expected for this case).
    let final_name = out1
        .final_path
        .file_name()
        .and_then(|n| n.to_str())
        .expect("final filename");
    let finals: Vec<_> = std::fs::read_dir(dest_root.path())
        .expect("read dest dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy() == final_name)
        .collect();
    assert_eq!(
        finals.len(),
        1,
        "exactly one final file must exist at outcome path"
    );

    let task = store
        .task(&task_id)
        .expect("load task")
        .expect("task exists");
    assert_eq!(task.state, TaskState::Completed);
    assert_eq!(
        task.actual_sha256.as_deref(),
        Some(expected_hash.as_str()),
        "stored actual hash must match finalized bytes"
    );
    assert_eq!(
        server.requests().len(),
        requests_before,
        "finalize_task must not issue fixture requests"
    );
}

#[tokio::test]
async fn finalize_task_recovers_crash_before_rename_and_after_rename() {
    // Crash before rename: part present, final absent, finalizing task -> completes.
    let before_body = pattern_bytes(31, 64 * 1024);
    let before_hash = sha256_hex(&before_body);
    let dest_before = tempdir().expect("dest tempdir before");
    let store_before_root = tempdir().expect("store tempdir before");
    let store_before = open_store(&store_before_root);
    let filename = "recover.bin".to_string();
    let task_before = store_before
        .create_task("http://127.0.0.1/before.bin", &filename)
        .expect("create task");
    prepare_finalizing_task_with_complete_part(
        &store_before,
        dest_before.path(),
        &task_before,
        &filename,
        &before_body,
    )
    .await;

    let out_before = finalize_task(&store_before, &task_before, dest_before.path(), None)
        .expect("crash-before-rename recovery must complete");
    assert!(
        out_before.final_path.exists(),
        "final must exist after recovery"
    );
    assert_eq!(read_file(&out_before.final_path), before_body);
    assert!(
        !dest_before
            .path()
            .join(PartFile::part_name(&filename))
            .exists(),
        "part must be gone after recovery"
    );
    let task = store_before
        .task(&task_before)
        .expect("load task")
        .expect("task exists");
    assert_eq!(task.state, TaskState::Completed);
    assert_eq!(task.actual_sha256.as_deref(), Some(before_hash.as_str()));

    // Crash after rename: final present, part absent, task finalizing with recorded total
    // -> finalize_task completes and stores the recomputed hash.
    let after_body = pattern_bytes(32, 48 * 1024);
    let after_hash = sha256_hex(&after_body);
    let dest_after = tempdir().expect("dest tempdir after");
    let store_after_root = tempdir().expect("store tempdir after");
    let store_after = open_store(&store_after_root);
    let filename_after = "after.bin".to_string();
    let task_after = store_after
        .create_task("http://127.0.0.1/after.bin", &filename_after)
        .expect("create task");
    let cap = CapDir::open(dest_after.path()).expect("open capdir");
    cap.write(&filename_after, &after_body)
        .await
        .expect("write final file (post-rename crash state)");
    // No part file present.
    store_after
        .record_part_path(&task_after, &PartFile::part_name(&filename_after))
        .expect("record part path");
    store_after
        .transition_task(&task_after, TaskState::Queued, TaskState::Connecting)
        .expect("queued->connecting");
    store_after
        .transition_task(&task_after, TaskState::Connecting, TaskState::Downloading)
        .expect("connecting->downloading");
    store_after
        .transition_task(&task_after, TaskState::Downloading, TaskState::Finalizing)
        .expect("downloading->finalizing");
    store_after
        .record_discovered_metadata(&task_after, after_body.len() as u64, None, None)
        .expect("record discovered metadata with total");

    let out_after = finalize_task(&store_after, &task_after, dest_after.path(), None)
        .expect("crash-after-rename recovery must complete");
    assert_eq!(
        out_after.final_path,
        dest_after.path().join(&filename_after)
    );
    assert_eq!(read_file(&out_after.final_path), after_body);
    let task = store_after
        .task(&task_after)
        .expect("load task")
        .expect("task exists");
    assert_eq!(task.state, TaskState::Completed);
    assert_eq!(
        task.actual_sha256.as_deref(),
        Some(after_hash.as_str()),
        "stored hash must be recomputed from final file"
    );
}

#[tokio::test]
async fn finalize_task_reports_missing_part_and_checksum_mismatch_outcomes() {
    // Missing part and final -> PART_MISSING.
    let dest_missing = tempdir().expect("dest tempdir missing");
    let store_missing_root = tempdir().expect("store tempdir missing");
    let store_missing = open_store(&store_missing_root);
    let filename = "missing.bin".to_string();
    let task_missing = store_missing
        .create_task("http://127.0.0.1/missing.bin", &filename)
        .expect("create task");
    store_missing
        .record_part_path(&task_missing, &PartFile::part_name(&filename))
        .expect("record part path");
    store_missing
        .transition_task(&task_missing, TaskState::Queued, TaskState::Connecting)
        .expect("queued->connecting");
    store_missing
        .transition_task(&task_missing, TaskState::Connecting, TaskState::Downloading)
        .expect("connecting->downloading");
    store_missing
        .transition_task(&task_missing, TaskState::Downloading, TaskState::Finalizing)
        .expect("downloading->finalizing");
    store_missing
        .record_discovered_metadata(&task_missing, 1024, None, None)
        .expect("record discovered metadata");

    let err = finalize_task(&store_missing, &task_missing, dest_missing.path(), None)
        .expect_err("missing part and final must fail");
    assert_code(&err, "PART_MISSING");

    // Expected hash mismatch -> CHECKSUM_MISMATCH, part preserved, no final file, task failed.
    let mismatch_body = pattern_bytes(55, 32 * 1024);
    let dest_mismatch = tempdir().expect("dest tempdir mismatch");
    let store_mismatch_root = tempdir().expect("store tempdir mismatch");
    let store_mismatch = open_store(&store_mismatch_root);
    let mismatch_filename = "mismatch.bin".to_string();
    let task_mismatch = store_mismatch
        .create_task("http://127.0.0.1/mismatch.bin", &mismatch_filename)
        .expect("create task");
    let part_path = prepare_finalizing_task_with_complete_part(
        &store_mismatch,
        dest_mismatch.path(),
        &task_mismatch,
        &mismatch_filename,
        &mismatch_body,
    )
    .await;
    assert!(
        part_path.exists(),
        "part must exist before mismatch finalize"
    );

    let wrong_expected = "0".repeat(64);
    let err = finalize_task(
        &store_mismatch,
        &task_mismatch,
        dest_mismatch.path(),
        Some(&wrong_expected),
    )
    .expect_err("hash mismatch must fail");
    assert_code(&err, "CHECKSUM_MISMATCH");

    assert!(
        part_path.exists(),
        "part file must be preserved on checksum mismatch"
    );
    let final_path = dest_mismatch.path().join(&mismatch_filename);
    assert!(
        !final_path.exists(),
        "no final file must be created on checksum mismatch"
    );
    let task = store_mismatch
        .task(&task_mismatch)
        .expect("load task")
        .expect("task exists");
    assert_eq!(
        task.state,
        TaskState::Failed,
        "task must be failed on checksum mismatch"
    );
}

#[tokio::test]
async fn disk_full_pauses_with_progress_and_resumes_after_the_fault_clears() {
    let body = pattern_bytes(77, 512 * 1024);
    let hash = sha256_hex(&body);
    let server = RangeServer::start(FixtureConfig {
        body: body.clone(),
        stream_delay: Some(Duration::from_millis(2)),
        stream_chunk_size: 64 * 1024,
        ..Default::default()
    });
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let store = open_store(&store_dir);
    let engine = Engine::new(
        store,
        dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    let filename = "disk-full.bin";
    let part_name = PartFile::part_name(filename);
    let spec = make_spec(server.url("/disk-full"), filename, 1);
    let id = engine.add_task(spec).expect("add task");

    let fault = faults::DiskFullFault::install(&part_name, 256 * 1024);
    let observed = run_engine_to_terminal(&engine, &id).await;

    assert_eq!(
        observed,
        TaskState::Paused,
        "an injected ENOSPC-equivalent write failure must pause the task"
    );
    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(
        task.error_code.as_deref(),
        Some("DISK_FULL"),
        "expected DISK_FULL, got {:?}: {:?}",
        task.error_code,
        task.error_message
    );
    assert!(
        task.downloaded_bytes > 0,
        "disk-full pause must preserve the bytes and segment progress already written"
    );
    let part = dest.path().join(&part_name);
    assert!(part.exists(), "disk-full pause must preserve the part file");
    assert_eq!(
        std::fs::metadata(&part).expect("part metadata").len(),
        body.len() as u64,
        "the preallocated part must keep its declared size"
    );
    assert!(
        !dest.path().join(filename).exists(),
        "disk-full pause must not create a final file"
    );
    let segments = engine.store().segments_for_task(&id).expect("segments");
    assert!(
        segments.iter().any(|segment| segment.downloaded_bytes > 0),
        "at least one segment must retain its persisted progress"
    );

    drop(fault);
    engine
        .resume(&id)
        .await
        .expect("resume after clearing the fault");
    let observed = run_engine_to_terminal(&engine, &id).await;

    assert_eq!(
        observed,
        TaskState::Completed,
        "the task must complete after the write fault clears"
    );
    let task = engine.store().task(&id).expect("load task").expect("task");
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));
    assert_eq!(
        std::fs::read(dest.path().join(filename)).expect("read final"),
        body,
        "resumed bytes must not be corrupted or duplicated"
    );
    assert!(!dest.path().join(&part_name).exists());
}

#[tokio::test]
async fn html_is_rejected_without_allowance_and_downloaded_with_allowance() {
    let html = b"<!doctype html><html><body>not a binary payload</body></html>".to_vec();
    let hash = sha256_hex(&html);

    // Denied through download_once: the probe rejects the page before any part exists.
    let denied_server = RangeServer::start(FixtureConfig {
        body: html.clone(),
        content_type: Some("text/html; charset=utf-8".to_string()),
        ..Default::default()
    });
    let denied_dest = tempdir().expect("denied dest");
    let denied_store_dir = tempdir().expect("denied store");
    let denied_store = open_store(&denied_store_dir);
    let denied_url = denied_server.url("/page.html");
    let denied_name = "page.html";
    let denied_id = denied_store
        .create_task(&denied_url, denied_name)
        .expect("create denied task");
    let denied_spec = make_spec_with(denied_url, denied_name, 1, false, None);

    let err = download_once(&denied_store, &denied_id, denied_dest.path(), &denied_spec)
        .await
        .expect_err("HTML must be rejected without explicit allowance");
    assert_code(&err, "HTML_RESPONSE");
    assert!(
        !denied_dest.path().join(denied_name).exists(),
        "a rejected HTML page must not create a final file"
    );
    assert!(
        !denied_dest
            .path()
            .join(PartFile::part_name(denied_name))
            .exists(),
        "a rejected HTML page must not create a part file"
    );

    // Denied through the engine: the task records the typed failure.
    let engine_server = RangeServer::start(FixtureConfig {
        body: html.clone(),
        content_type: Some("text/html".to_string()),
        ..Default::default()
    });
    let engine_dest = tempdir().expect("engine dest");
    let engine_store_dir = tempdir().expect("engine store");
    let engine_store = open_store(&engine_store_dir);
    let engine = Engine::new(
        engine_store,
        engine_dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    let engine_name = "page-engine.html";
    let engine_id = engine
        .add_task(make_spec_with(
            engine_server.url("/page-engine.html"),
            engine_name,
            1,
            false,
            None,
        ))
        .expect("add engine task");
    let observed = run_engine_to_terminal(&engine, &engine_id).await;
    assert_eq!(observed, TaskState::Failed);
    let task = engine
        .store()
        .task(&engine_id)
        .expect("load task")
        .expect("task");
    assert_eq!(
        task.error_code.as_deref(),
        Some("HTML_RESPONSE"),
        "expected HTML_RESPONSE, got {:?}: {:?}",
        task.error_code,
        task.error_message
    );
    assert!(
        !engine_dest.path().join(engine_name).exists(),
        "the engine must not finalize a rejected HTML response"
    );

    // Allowed: the HTML bytes are stored as an ordinary file and hashed.
    let allowed_server = RangeServer::start(FixtureConfig {
        body: html.clone(),
        content_type: Some("text/html; charset=utf-8".to_string()),
        ..Default::default()
    });
    let allowed_dest = tempdir().expect("allowed dest");
    let allowed_store_dir = tempdir().expect("allowed store");
    let allowed_store = open_store(&allowed_store_dir);
    let allowed_url = allowed_server.url("/allowed.html");
    let allowed_name = "allowed.html";
    let allowed_id = allowed_store
        .create_task(&allowed_url, allowed_name)
        .expect("create allowed task");
    let allowed_spec = make_spec_with(allowed_url, allowed_name, 1, true, Some(hash.clone()));

    let outcome = download_once(
        &allowed_store,
        &allowed_id,
        allowed_dest.path(),
        &allowed_spec,
    )
    .await
    .expect("allowed HTML must download as an ordinary file");
    assert_eq!(outcome.actual_sha256, hash);
    assert_eq!(
        read_file(&outcome.final_path),
        html,
        "allowed HTML bytes must be preserved exactly"
    );
    let task = allowed_store
        .task(&allowed_id)
        .expect("load task")
        .expect("task");
    assert_eq!(task.state, TaskState::Completed);
    assert_eq!(task.actual_sha256.as_deref(), Some(hash.as_str()));
}

#[tokio::test]
async fn checksum_mismatch_fails_without_renaming_for_download_once_and_engine_paths() {
    let body = pattern_bytes(91, 256 * 1024);
    let wrong_expected = "0".repeat(64);
    let server = RangeServer::start(FixtureConfig {
        body: body.clone(),
        ..Default::default()
    });

    // download_once path.
    let once_dest = tempdir().expect("once dest");
    let once_store_dir = tempdir().expect("once store");
    let once_store = open_store(&once_store_dir);
    let once_name = "checksum-once.bin";
    let once_url = server.url("/checksum-once");
    let once_id = once_store
        .create_task(&once_url, once_name)
        .expect("create once task");
    let once_spec = make_spec_with(once_url, once_name, 1, false, Some(wrong_expected.clone()));

    let err = download_once(&once_store, &once_id, once_dest.path(), &once_spec)
        .await
        .expect_err("a wrong expected SHA-256 must fail");
    assert_code(&err, "CHECKSUM_MISMATCH");
    let once_part = once_dest.path().join(PartFile::part_name(once_name));
    assert!(
        once_part.exists(),
        "checksum mismatch must preserve the part file"
    );
    assert_eq!(
        std::fs::metadata(&once_part).expect("part metadata").len(),
        body.len() as u64
    );
    assert!(
        !once_dest.path().join(once_name).exists(),
        "checksum mismatch must not create a final file"
    );

    // Engine path.
    let engine_dest = tempdir().expect("engine dest");
    let engine_store_dir = tempdir().expect("engine store");
    let engine_store = open_store(&engine_store_dir);
    let engine = Engine::new(
        engine_store,
        engine_dest.path().to_path_buf(),
        EngineOptions { max_concurrent: 1 },
    );
    let engine_name = "checksum-engine.bin";
    let engine_id = engine
        .add_task(make_spec_with(
            server.url("/checksum-engine"),
            engine_name,
            1,
            false,
            Some(wrong_expected),
        ))
        .expect("add engine task");
    let observed = run_engine_to_terminal(&engine, &engine_id).await;

    assert_eq!(observed, TaskState::Failed);
    let task = engine
        .store()
        .task(&engine_id)
        .expect("load task")
        .expect("task");
    assert_eq!(
        task.error_code.as_deref(),
        Some("CHECKSUM_MISMATCH"),
        "expected CHECKSUM_MISMATCH, got {:?}: {:?}",
        task.error_code,
        task.error_message
    );
    let engine_part = engine_dest.path().join(PartFile::part_name(engine_name));
    assert!(
        engine_part.exists(),
        "the engine path must preserve the part on checksum mismatch"
    );
    assert_eq!(
        std::fs::metadata(&engine_part)
            .expect("part metadata")
            .len(),
        body.len() as u64
    );
    assert!(
        !engine_dest.path().join(engine_name).exists(),
        "the engine path must not create a final file on checksum mismatch"
    );
}
