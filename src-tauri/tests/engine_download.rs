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

use std::path::{Path, PathBuf};

use oneget_lib::engine::error::EngineError;
use oneget_lib::engine::part_file::PartFile;
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
    DownloadSpec {
        url,
        filename: filename.to_string(),
        connections,
        allow_html: false,
        expected_sha256: None,
        extra_headers: Vec::new(),
    }
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
