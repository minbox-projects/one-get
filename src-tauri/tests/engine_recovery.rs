//! Behavior tests for startup recovery, resume metadata validation, and the
//! crash-recovery harness.
//!
//! Expected RED: build fails because `Engine::recover`/`RecoveryReport` and the
//! `engine_harness` binary (`CARGO_BIN_EXE_engine_harness`) are not implemented.

mod support;

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use oneget_lib::engine::error::EngineError;
use oneget_lib::engine::part_file::PartFile;
use oneget_lib::engine::scheduler::{Engine, EngineOptions};
use oneget_lib::engine::state::{SegmentState, TaskState};
use oneget_lib::platform::files::CapDir;
use oneget_lib::store::types::{SegmentInput, SegmentProgressInput};
use oneget_lib::store::Database;
use sha2::{Digest, Sha256};
use support::fixtures::pattern_bytes;
use support::range_server::{FixtureConfig, RangeMode, RangeServer, ServedRange};
use tempfile::tempdir;
use tokio::time::{sleep, timeout};

const POLL_INTERVAL: Duration = Duration::from_millis(20);
const WAIT_TIMEOUT: Duration = Duration::from_secs(30);
const SETTLE: Duration = Duration::from_millis(300);
const ONE_MIB: u64 = 1024 * 1024;

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

fn open_store_dir(dir: &Path) -> Database {
    Database::open(dir.join("tasks.db")).expect("open store")
}

fn open_store(root: &tempfile::TempDir) -> Database {
    open_store_dir(root.path())
}

fn read_file(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn overlap_bytes(range: &ServedRange, start: u64, end_exclusive: u64) -> u64 {
    if end_exclusive <= start {
        return 0;
    }
    let high_exclusive = range.end.saturating_add(1);
    let lo = range.start.max(start);
    let hi = high_exclusive.min(end_exclusive);
    hi.saturating_sub(lo)
}

fn bytes_reserved_below_high_water(
    served: &[ServedRange],
    segments: &[(u64, u64)], // (start, pre_downloaded)
) -> Vec<u64> {
    segments
        .iter()
        .map(|(start, pre)| {
            let high = start.saturating_add(*pre);
            served
                .iter()
                .map(|r| overlap_bytes(r, *start, high))
                .sum::<u64>()
        })
        .collect()
}

fn make_engine(store: Database, dest: &Path) -> Engine {
    Engine::new(
        store,
        dest.to_path_buf(),
        EngineOptions { max_concurrent: 4 },
    )
}

async fn wait_for_terminal_async(store: &Database, id: &str, deadline: Duration) -> TaskState {
    let start = std::time::Instant::now();
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
                "timed out waiting for terminal state for {id}, current {:?}, error={:?} {:?}",
                task.state, task.error_code, task.error_message
            );
        }
        sleep(POLL_INTERVAL).await;
    }
}

#[allow(clippy::too_many_arguments)]
async fn prepare_downloading_task_with_partial_segments(
    store: &Database,
    dest: &Path,
    task_id: &str,
    filename: &str,
    body: &[u8],
    etag: Option<&str>,
    last_modified: Option<&str>,
    connections: u32,
    segment_count: u32,
    bytes_per_segment: u64,
) -> Vec<(u64, u64)> {
    assert!(segment_count >= 1);
    let total = body.len() as u64;
    let seg_len = total / segment_count as u64;

    let cap = CapDir::open(dest).expect("open capdir");
    let part_name = PartFile::part_name(filename);
    let part = PartFile::create(&cap, filename, Some(total)).expect("create part");
    store
        .record_part_path(task_id, &part_name)
        .expect("record part path");
    store
        .record_connections(task_id, connections)
        .expect("record connections");
    store
        .record_discovered_metadata(task_id, Some(total), etag, last_modified)
        .expect("record metadata");

    store
        .transition_task(task_id, TaskState::Queued, TaskState::Connecting)
        .expect("queued->connecting");
    store
        .transition_task(task_id, TaskState::Connecting, TaskState::Downloading)
        .expect("connecting->downloading");

    let mut inputs = Vec::new();
    let mut progress = Vec::new();
    let mut high_waters = Vec::new();
    let mut downloaded_total = 0u64;
    for i in 0..segment_count {
        let start = i as u64 * seg_len;
        let end = if i + 1 == segment_count {
            total
        } else {
            (i as u64 + 1) * seg_len
        };
        let seg_size = end - start;
        let have = bytes_per_segment.min(seg_size);
        if have > 0 {
            part.write_at(start, &body[start as usize..(start + have) as usize])
                .expect("write partial segment");
        }
        inputs.push(SegmentInput {
            index: i,
            start,
            end,
            downloaded: have,
            state: if have == seg_size {
                SegmentState::Done
            } else if have > 0 {
                SegmentState::Active
            } else {
                SegmentState::Pending
            },
        });
        progress.push(SegmentProgressInput {
            segment_id: i,
            downloaded_bytes: have,
        });
        high_waters.push((start, have));
        downloaded_total += have;
    }
    store
        .replace_segments(task_id, &inputs)
        .expect("replace segments");
    store
        .persist_progress(task_id, &progress, downloaded_total)
        .expect("persist progress");
    high_waters
}

#[tokio::test]
async fn interrupted_downloading_recovery_resumes_and_bounds_redownload() {
    let body = pattern_bytes(41, 2 * 1024 * 1024);
    let expected_hash = sha256_hex(&body);
    let server = RangeServer::start(FixtureConfig {
        body: body.clone(),
        range_mode: RangeMode::HonorRange,
        etag: Some("\"etag-v1\"".to_string()),
        last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".to_string()),
        ..Default::default()
    });
    let url = server.url("/recover-interrupted.bin");

    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let store = open_store(&store_dir);
    let filename = "recover-interrupted.bin".to_string();
    let task_id = store.create_task(&url, &filename).expect("create task");

    // 4 segments, first ~300 KiB of each segment already persisted in part file.
    let pre_high_waters = prepare_downloading_task_with_partial_segments(
        &store,
        dest.path(),
        &task_id,
        &filename,
        &body,
        Some("\"etag-v1\""),
        Some("Wed, 21 Oct 2015 07:28:00 GMT"),
        4,
        4,
        300 * 1024,
    )
    .await;

    let served_before: Vec<ServedRange> = server.served_ranges();
    let _ = served_before;

    let engine = make_engine(open_store_dir(store_dir.path()), dest.path());
    engine
        .recover()
        .expect("recover interrupted downloading task");

    // Drive completion if recovery requeues/resumes work.
    let _ = timeout(WAIT_TIMEOUT, engine.run()).await;

    let store_after = open_store_dir(store_dir.path());
    let state = wait_for_terminal_async(&store_after, &task_id, WAIT_TIMEOUT).await;
    assert_eq!(
        state,
        TaskState::Completed,
        "interrupted task should complete"
    );
    let task = store_after.task(&task_id).unwrap().unwrap();
    assert_eq!(task.actual_sha256.as_deref(), Some(expected_hash.as_str()));
    let final_path = dest.path().join(&filename);
    assert!(final_path.exists(), "final file must exist");
    assert_eq!(
        read_file(&final_path),
        body,
        "final hash/bytes must match fixture"
    );

    let served_after = server.served_ranges();
    let per_segment = bytes_reserved_below_high_water(&served_after, &pre_high_waters);
    for (i, bytes) in per_segment.iter().enumerate() {
        assert!(
            *bytes <= ONE_MIB,
            "segment {i} re-downloaded {bytes} bytes below pre-recovery high-water mark (limit 1 MiB)"
        );
    }
}

async fn prepare_paused_task(
    store: &Database,
    dest: &Path,
    task_id: &str,
    filename: &str,
    body: &[u8],
) {
    let cap = CapDir::open(dest).expect("open capdir");
    let part_name = PartFile::part_name(filename);
    cap.write(&part_name, &body[..4096])
        .await
        .expect("write part");
    store.record_part_path(task_id, &part_name).unwrap();
    store
        .record_discovered_metadata(task_id, Some(body.len() as u64), None, None)
        .unwrap();
    store
        .transition_task(task_id, TaskState::Queued, TaskState::Connecting)
        .unwrap();
    store
        .transition_task(task_id, TaskState::Connecting, TaskState::Downloading)
        .unwrap();
    store
        .transition_task(task_id, TaskState::Downloading, TaskState::Paused)
        .unwrap();
    let segs = vec![SegmentInput {
        index: 0,
        start: 0,
        end: body.len() as u64,
        downloaded: 4096,
        state: SegmentState::Active,
    }];
    store.replace_segments(task_id, &segs).unwrap();
    store
        .persist_paused_progress(
            task_id,
            &[SegmentProgressInput {
                segment_id: 0,
                downloaded_bytes: 4096,
            }],
            4096,
        )
        .unwrap();
}

async fn prepare_failed_task_with_part(
    store: &Database,
    dest: &Path,
    task_id: &str,
    filename: &str,
    body: &[u8],
) {
    let cap = CapDir::open(dest).expect("open capdir");
    let part_name = PartFile::part_name(filename);
    cap.write(&part_name, &body[..2048])
        .await
        .expect("write part");
    store.record_part_path(task_id, &part_name).unwrap();
    store
        .transition_task(task_id, TaskState::Queued, TaskState::Connecting)
        .unwrap();
    store
        .transition_task(task_id, TaskState::Connecting, TaskState::Downloading)
        .unwrap();
    store
        .transition_task(task_id, TaskState::Downloading, TaskState::Failed)
        .unwrap();
    store
        .update_task_error(task_id, "NETWORK_ERROR", "simulated failure")
        .unwrap();
}

async fn prepare_cancelled_task_with_leftover_part(
    store: &Database,
    dest: &Path,
    task_id: &str,
    filename: &str,
) {
    let cap = CapDir::open(dest).expect("open capdir");
    let part_name = PartFile::part_name(filename);
    cap.write(&part_name, b"leftover")
        .await
        .expect("write leftover part");
    store.record_part_path(task_id, &part_name).unwrap();
    store
        .transition_task(task_id, TaskState::Queued, TaskState::Cancelled)
        .unwrap();
}

#[tokio::test]
async fn recovery_preserves_paused_and_failed_and_cleans_cancelled_and_completed() {
    let body = pattern_bytes(7, 64 * 1024);
    let hash = sha256_hex(&body);

    // Paused stays paused
    let dest_paused = tempdir().expect("dest paused");
    let store_paused = tempdir().expect("store paused");
    let db_paused = open_store(&store_paused);
    let id_paused = db_paused
        .create_task("http://127.0.0.1/p", "p.bin")
        .unwrap();
    prepare_paused_task(&db_paused, dest_paused.path(), &id_paused, "p.bin", &body).await;
    let engine = make_engine(open_store_dir(store_paused.path()), dest_paused.path());
    engine.recover().expect("recover paused");
    let t = open_store_dir(store_paused.path())
        .task(&id_paused)
        .unwrap()
        .unwrap();
    assert_eq!(t.state, TaskState::Paused, "paused must stay paused");
    assert!(
        dest_paused
            .path()
            .join(PartFile::part_name("p.bin"))
            .exists(),
        "paused part must be preserved"
    );

    // Failed preserves its part
    let dest_failed = tempdir().expect("dest failed");
    let store_failed = tempdir().expect("store failed");
    let db_failed = open_store(&store_failed);
    let id_failed = db_failed
        .create_task("http://127.0.0.1/f", "f.bin")
        .unwrap();
    prepare_failed_task_with_part(&db_failed, dest_failed.path(), &id_failed, "f.bin", &body).await;
    let engine = make_engine(open_store_dir(store_failed.path()), dest_failed.path());
    engine.recover().expect("recover failed");
    let t = open_store_dir(store_failed.path())
        .task(&id_failed)
        .unwrap()
        .unwrap();
    assert_eq!(t.state, TaskState::Failed, "failed must stay failed");
    assert!(
        dest_failed
            .path()
            .join(PartFile::part_name("f.bin"))
            .exists(),
        "failed part must be preserved"
    );

    // Cancelled removes leftover part
    let dest_cancel = tempdir().expect("dest cancel");
    let store_cancel = tempdir().expect("store cancel");
    let db_cancel = open_store(&store_cancel);
    let id_cancel = db_cancel
        .create_task("http://127.0.0.1/c", "c.bin")
        .unwrap();
    prepare_cancelled_task_with_leftover_part(&db_cancel, dest_cancel.path(), &id_cancel, "c.bin")
        .await;
    let engine = make_engine(open_store_dir(store_cancel.path()), dest_cancel.path());
    engine.recover().expect("recover cancelled");
    let t = open_store_dir(store_cancel.path())
        .task(&id_cancel)
        .unwrap()
        .unwrap();
    assert_eq!(t.state, TaskState::Cancelled);
    assert!(
        !dest_cancel
            .path()
            .join(PartFile::part_name("c.bin"))
            .exists(),
        "cancelled leftover part must be removed"
    );

    // Completed final file untouched
    let dest_done = tempdir().expect("dest done");
    let store_done = tempdir().expect("store done");
    let db_done = open_store(&store_done);
    let id_done = db_done.create_task("http://127.0.0.1/d", "d.bin").unwrap();
    std::fs::write(dest_done.path().join("d.bin"), &body).unwrap();
    db_done
        .transition_task(&id_done, TaskState::Queued, TaskState::Connecting)
        .unwrap();
    db_done
        .transition_task(&id_done, TaskState::Connecting, TaskState::Downloading)
        .unwrap();
    db_done
        .transition_task(&id_done, TaskState::Downloading, TaskState::Finalizing)
        .unwrap();
    db_done
        .record_discovered_metadata(&id_done, Some(body.len() as u64), None, None)
        .unwrap();
    db_done.record_actual_hash(&id_done, &hash).unwrap();
    db_done.complete_task(&id_done, Some(&hash)).unwrap();
    let engine = make_engine(open_store_dir(store_done.path()), dest_done.path());
    engine.recover().expect("recover completed");
    let t = open_store_dir(store_done.path())
        .task(&id_done)
        .unwrap()
        .unwrap();
    assert_eq!(t.state, TaskState::Completed);
    assert_eq!(read_file(&dest_done.path().join("d.bin")), body);
    let _ = id_done;
}

fn transition_to_finalizing(store: &Database, task_id: &str) {
    store
        .transition_task(task_id, TaskState::Queued, TaskState::Connecting)
        .unwrap();
    store
        .transition_task(task_id, TaskState::Connecting, TaskState::Downloading)
        .unwrap();
    store
        .transition_task(task_id, TaskState::Downloading, TaskState::Finalizing)
        .unwrap();
}

#[tokio::test]
async fn finalizing_recovery_cases_are_handled_idempotently_and_safely() {
    // final-only with stored matching hash -> completed without re-download
    let body = pattern_bytes(11, 32 * 1024);
    let hash = sha256_hex(&body);
    let dest = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let db = open_store(&store_dir);
    let id = db.create_task("http://127.0.0.1/f1", "f1.bin").unwrap();
    std::fs::write(dest.path().join("f1.bin"), &body).unwrap();
    transition_to_finalizing(&db, &id);
    db.record_discovered_metadata(&id, Some(body.len() as u64), None, None)
        .unwrap();
    db.record_part_path(&id, &PartFile::part_name("f1.bin"))
        .unwrap();
    db.record_actual_hash(&id, &hash).unwrap();
    let engine = make_engine(open_store_dir(store_dir.path()), dest.path());
    engine.recover().expect("recover final-only stored hash");
    let t = open_store_dir(store_dir.path()).task(&id).unwrap().unwrap();
    assert_eq!(t.state, TaskState::Completed);
    assert_eq!(t.actual_sha256.as_deref(), Some(hash.as_str()));
    assert_eq!(read_file(&dest.path().join("f1.bin")), body);

    // final-only no stored hash, size==total and expected hash match -> recompute and complete
    let body = pattern_bytes(12, 24 * 1024);
    let hash = sha256_hex(&body);
    let dest = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let db = open_store(&store_dir);
    let id = db.create_task("http://127.0.0.1/f2", "f2.bin").unwrap();
    std::fs::write(dest.path().join("f2.bin"), &body).unwrap();
    transition_to_finalizing(&db, &id);
    db.record_discovered_metadata(&id, Some(body.len() as u64), None, None)
        .unwrap();
    db.record_part_path(&id, &PartFile::part_name("f2.bin"))
        .unwrap();
    db.record_expected_sha256(&id, &hash).unwrap();
    let engine = make_engine(open_store_dir(store_dir.path()), dest.path());
    engine
        .recover()
        .expect("recover final-only recompute match");
    let t = open_store_dir(store_dir.path()).task(&id).unwrap().unwrap();
    assert_eq!(t.state, TaskState::Completed);
    assert_eq!(t.actual_sha256.as_deref(), Some(hash.as_str()));

    // final-only no stored hash, expected-hash mismatch -> CHECKSUM_MISMATCH preserving final
    let body = pattern_bytes(13, 24 * 1024);
    let wrong = "0".repeat(64);
    let dest = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let db = open_store(&store_dir);
    let id = db.create_task("http://127.0.0.1/f3", "f3.bin").unwrap();
    std::fs::write(dest.path().join("f3.bin"), &body).unwrap();
    transition_to_finalizing(&db, &id);
    db.record_discovered_metadata(&id, Some(body.len() as u64), None, None)
        .unwrap();
    db.record_part_path(&id, &PartFile::part_name("f3.bin"))
        .unwrap();
    db.record_expected_sha256(&id, &wrong).unwrap();
    let engine = make_engine(open_store_dir(store_dir.path()), dest.path());
    let res = engine.recover();
    // recovery may return error or mark task failed; either way task must be failed with CHECKSUM_MISMATCH and final preserved
    if let Err(e) = res {
        assert_eq!(e.code(), "CHECKSUM_MISMATCH");
    }
    let t = open_store_dir(store_dir.path()).task(&id).unwrap().unwrap();
    assert_eq!(t.state, TaskState::Failed);
    assert_eq!(t.error_code.as_deref(), Some("CHECKSUM_MISMATCH"));
    assert_eq!(
        read_file(&dest.path().join("f3.bin")),
        body,
        "final file must be preserved on checksum mismatch"
    );

    // part-only -> re-finalize safely
    let body = pattern_bytes(14, 20 * 1024);
    let hash = sha256_hex(&body);
    let dest = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let db = open_store(&store_dir);
    let id = db.create_task("http://127.0.0.1/f4", "f4.bin").unwrap();
    let cap = CapDir::open(dest.path()).unwrap();
    cap.write(&PartFile::part_name("f4.bin"), &body)
        .await
        .unwrap();
    transition_to_finalizing(&db, &id);
    db.record_discovered_metadata(&id, Some(body.len() as u64), None, None)
        .unwrap();
    db.record_part_path(&id, &PartFile::part_name("f4.bin"))
        .unwrap();
    let engine = make_engine(open_store_dir(store_dir.path()), dest.path());
    engine.recover().expect("recover part-only finalize");
    let t = open_store_dir(store_dir.path()).task(&id).unwrap().unwrap();
    assert_eq!(t.state, TaskState::Completed);
    assert_eq!(t.actual_sha256.as_deref(), Some(hash.as_str()));
    assert!(dest.path().join("f4.bin").exists());
    assert!(!dest.path().join(PartFile::part_name("f4.bin")).exists());

    // neither part nor final -> PART_MISSING
    let dest = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let db = open_store(&store_dir);
    let id = db.create_task("http://127.0.0.1/f5", "f5.bin").unwrap();
    transition_to_finalizing(&db, &id);
    db.record_discovered_metadata(&id, Some(1024), None, None)
        .unwrap();
    db.record_part_path(&id, &PartFile::part_name("f5.bin"))
        .unwrap();
    let engine = make_engine(open_store_dir(store_dir.path()), dest.path());
    let res = engine.recover();
    if let Err(e) = res {
        assert_eq!(e.code(), "PART_MISSING");
    }
    let t = open_store_dir(store_dir.path()).task(&id).unwrap().unwrap();
    assert_eq!(t.state, TaskState::Failed);
    assert_eq!(t.error_code.as_deref(), Some("PART_MISSING"));

    // cancelled leftover part removed without touching final file
    let dest = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let db = open_store(&store_dir);
    let id = db.create_task("http://127.0.0.1/f6", "f6.bin").unwrap();
    std::fs::write(dest.path().join("f6.bin"), b"final").unwrap();
    let cap = CapDir::open(dest.path()).unwrap();
    cap.write(&PartFile::part_name("f6.bin"), b"leftover")
        .await
        .unwrap();
    db.record_part_path(&id, &PartFile::part_name("f6.bin"))
        .unwrap();
    db.transition_task(&id, TaskState::Queued, TaskState::Cancelled)
        .unwrap();
    let engine = make_engine(open_store_dir(store_dir.path()), dest.path());
    engine.recover().expect("recover cancelled leftover");
    assert!(!dest.path().join(PartFile::part_name("f6.bin")).exists());
    assert_eq!(read_file(&dest.path().join("f6.bin")), b"final");
}

#[tokio::test]
async fn resume_rejects_changed_remote_metadata_and_preserves_part() {
    let body = pattern_bytes(51, 256 * 1024);

    for (label, mutate) in [
        ("etag", "etag"),
        ("last_modified", "last_modified"),
        ("total", "total"),
    ] {
        let config = FixtureConfig {
            body: body.clone(),
            range_mode: RangeMode::HonorRange,
            etag: Some("\"v1\"".to_string()),
            last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".to_string()),
            ..Default::default()
        };
        let server = RangeServer::start(config.clone());
        let url = server.url("/changed.bin");
        let dest = tempdir().unwrap();
        let store_dir = tempdir().unwrap();
        let db = open_store(&store_dir);
        let id = db.create_task(&url, "changed.bin").unwrap();

        prepare_paused_task(&db, dest.path(), &id, "changed.bin", &body).await;
        db.record_discovered_metadata(
            &id,
            Some(body.len() as u64),
            Some("\"v1\""),
            Some("Wed, 21 Oct 2015 07:28:00 GMT"),
        )
        .unwrap();

        match mutate {
            "etag" => server.set_runtime_etag(Some("\"v2\"".to_string())),
            "last_modified" => {
                server.set_runtime_last_modified(Some("Thu, 22 Oct 2015 07:28:00 GMT".to_string()))
            }
            "total" => server.set_runtime_total(body.len() as u64 + 4096),
            _ => unreachable!(),
        }

        let engine = make_engine(open_store_dir(store_dir.path()), dest.path());
        engine.recover().expect("recover changed remote task");
        // resume path: recovery/re-probe should fail task with FILE_CHANGED
        let _ = timeout(WAIT_TIMEOUT, engine.run()).await;
        let store_after = open_store_dir(store_dir.path());
        let state = wait_for_terminal_async(&store_after, &id, WAIT_TIMEOUT).await;
        assert_eq!(
            state,
            TaskState::Failed,
            "changed {label} must fail resume/recovery"
        );
        let task = store_after.task(&id).unwrap().unwrap();
        assert_eq!(
            task.error_code.as_deref(),
            Some("FILE_CHANGED"),
            "changed {label} must report FILE_CHANGED"
        );
        assert!(
            dest.path()
                .join(PartFile::part_name("changed.bin"))
                .exists(),
            "part must be preserved on FILE_CHANGED ({label})"
        );
        assert!(
            !dest.path().join("changed.bin").exists(),
            "no final file on FILE_CHANGED ({label})"
        );
        let _ = config;
    }
}

#[tokio::test]
async fn resume_missing_part_fails_with_part_missing() {
    let body = pattern_bytes(61, 128 * 1024);
    let server = RangeServer::start(FixtureConfig {
        body: body.clone(),
        range_mode: RangeMode::HonorRange,
        ..Default::default()
    });
    let url = server.url("/missing-part.bin");
    let dest = tempdir().unwrap();
    let store_dir = tempdir().unwrap();
    let db = open_store(&store_dir);
    let id = db.create_task(&url, "missing-part.bin").unwrap();
    prepare_paused_task(&db, dest.path(), &id, "missing-part.bin", &body).await;
    db.record_discovered_metadata(&id, Some(body.len() as u64), None, None)
        .unwrap();
    // remove part to simulate missing input
    let part = dest.path().join(PartFile::part_name("missing-part.bin"));
    if part.exists() {
        std::fs::remove_file(&part).unwrap();
    }

    let engine = make_engine(open_store_dir(store_dir.path()), dest.path());
    let res = engine.recover();
    if let Err(e) = res {
        assert_code(&e, "PART_MISSING");
    }
    let _ = timeout(WAIT_TIMEOUT, engine.run()).await;
    let store_after = open_store_dir(store_dir.path());
    let state = wait_for_terminal_async(&store_after, &id, WAIT_TIMEOUT).await;
    assert_eq!(state, TaskState::Failed);
    let task = store_after.task(&id).unwrap().unwrap();
    assert_eq!(task.error_code.as_deref(), Some("PART_MISSING"));
}

async fn wait_for_downloading_progress(
    store: &Database,
    id: &str,
    min_bytes: u64,
    deadline: Duration,
) -> u64 {
    let start = std::time::Instant::now();
    loop {
        let task = store.task(id).expect("load task").expect("task exists");
        if task.state == TaskState::Downloading && task.downloaded_bytes >= min_bytes {
            return task.downloaded_bytes;
        }
        if matches!(task.state, TaskState::Failed | TaskState::Cancelled) {
            panic!(
                "task {id} reached terminal state {:?} while waiting for progress, error={:?} {:?}",
                task.state, task.error_code, task.error_message
            );
        }
        if start.elapsed() > deadline {
            panic!(
                "timed out waiting for task {id} progress >= {min_bytes}, current {:?} {}",
                task.state, task.downloaded_bytes
            );
        }
        sleep(POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn resume_with_missing_cookie_file_fails_with_cookie_file_error_and_keeps_part() {
    let body = pattern_bytes(151, 768 * 1024);
    let server = RangeServer::start(FixtureConfig {
        body: body.clone(),
        range_mode: RangeMode::HonorRange,
        stream_delay: Some(Duration::from_millis(12)),
        stream_chunk_size: 16 * 1024,
        ..Default::default()
    });
    let url = server.url("/missing-cookie.bin");
    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let cookie_dir = tempdir().expect("cookies");
    let cookie_path = cookie_dir.path().join("cookies.txt");
    std::fs::write(&cookie_path, b"").expect("write empty cookie file");

    let engine = make_engine(open_store(&store_dir), dest.path());
    engine.set_cookie_file(Some(cookie_path.clone()));
    let id = engine
        .store()
        .create_task(&url, "missing-cookie.bin")
        .unwrap();
    engine.store().record_connections(&id, 1).unwrap();

    tokio::join!(
        async {
            timeout(WAIT_TIMEOUT, engine.run())
                .await
                .expect("run timeout")
                .expect("run");
        },
        async {
            wait_for_downloading_progress(engine.store(), &id, 64 * 1024, WAIT_TIMEOUT).await;
            engine.pause(&id).await.expect("pause downloading task");
            let state = wait_for_terminal_async(engine.store(), &id, WAIT_TIMEOUT).await;
            assert_eq!(state, TaskState::Paused, "task must pause before resume");
        }
    );

    let part = dest.path().join(PartFile::part_name("missing-cookie.bin"));
    assert!(
        part.exists(),
        "paused task must keep its part before resume"
    );
    std::fs::remove_file(&cookie_path).expect("delete the cookie file");

    engine.resume(&id).await.expect("resume paused task");
    timeout(WAIT_TIMEOUT, engine.run())
        .await
        .expect("resume run timeout")
        .expect("resume run");

    let state = wait_for_terminal_async(engine.store(), &id, WAIT_TIMEOUT).await;
    assert!(
        matches!(state, TaskState::Failed | TaskState::Paused),
        "missing cookie file must end failed or paused, got {state:?}"
    );
    let task = engine.store().task(&id).unwrap().unwrap();
    assert_eq!(
        task.error_code.as_deref(),
        Some("COOKIE_FILE_ERROR"),
        "missing cookie file must report the typed cookie error, got {:?}: {:?}",
        task.error_code,
        task.error_message
    );
    assert!(
        part.exists(),
        "the part must be preserved on a cookie error"
    );
    assert!(
        !dest.path().join("missing-cookie.bin").exists(),
        "no final file may appear on a cookie error"
    );
}

fn harness_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_engine_harness"))
}

fn run_harness(args: &[&str], deadline: Duration) -> (std::process::ExitStatus, String, String) {
    let mut child = Command::new(harness_exe())
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn engine harness");
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("poll harness") {
            let mut stdout = String::new();
            child
                .stdout
                .take()
                .expect("harness stdout")
                .read_to_string(&mut stdout)
                .expect("read harness stdout");
            let mut stderr = String::new();
            child
                .stderr
                .take()
                .expect("harness stderr")
                .read_to_string(&mut stderr)
                .expect("read harness stderr");
            return (status, stdout, stderr);
        }
        if start.elapsed() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("harness command {args:?} exceeded the {deadline:?} bound");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn assert_harness_status(args: &[&str], expected_state: &str) -> String {
    let (status, stdout, stderr) = run_harness(args, Duration::from_secs(15));
    assert!(
        status.success(),
        "harness {args:?} must exit successfully, status={status:?}, stdout={stdout}, stderr={stderr}"
    );
    let line = stdout.trim();
    assert!(
        line.starts_with(&format!("state {expected_state} downloaded ")),
        "status must print a bounded state line for {expected_state}, got {line:?}"
    );
    assert!(
        line.len() < 200 && line.lines().count() == 1,
        "status output must stay bounded, got {line:?}"
    );
    line.to_string()
}

fn assert_harness_control(args: &[&str]) {
    let (status, stdout, stderr) = run_harness(args, Duration::from_secs(15));
    assert!(
        status.success(),
        "harness {args:?} must exit successfully, status={status:?}, stdout={stdout}, stderr={stderr}"
    );
}

#[test]
fn harness_status_pause_resume_cancel_controls_persist_transitions() {
    let store_dir = tempdir().expect("store");
    let db_path = store_dir.path().join("tasks.db");
    let db_path_text = db_path.to_string_lossy().to_string();
    let id = {
        let db = Database::open(&db_path).expect("open store");
        let id = db
            .create_task(
                "http://127.0.0.1:9/harness-control.bin",
                "harness-control.bin",
            )
            .expect("seed queued task");
        db.record_connections(&id, 1).expect("record connections");
        id
    };

    let status_args = ["status", "--db", db_path_text.as_str(), "--id", id.as_str()];
    assert_harness_status(&status_args, "queued");

    assert_harness_control(&["pause", "--db", db_path_text.as_str(), "--id", id.as_str()]);
    let state = Database::open(&db_path)
        .expect("reopen store")
        .task(&id)
        .expect("load task")
        .expect("task")
        .state;
    assert_eq!(
        state,
        TaskState::Paused,
        "pause must persist the paused state"
    );
    assert_harness_status(&status_args, "paused");

    assert_harness_control(&["resume", "--db", db_path_text.as_str(), "--id", id.as_str()]);
    let state = Database::open(&db_path)
        .expect("reopen store")
        .task(&id)
        .expect("load task")
        .expect("task")
        .state;
    assert_eq!(state, TaskState::Queued, "resume must requeue the task");
    assert_harness_status(&status_args, "queued");

    assert_harness_control(&["cancel", "--db", db_path_text.as_str(), "--id", id.as_str()]);
    let state = Database::open(&db_path)
        .expect("reopen store")
        .task(&id)
        .expect("load task")
        .expect("task")
        .state;
    assert_eq!(
        state,
        TaskState::Cancelled,
        "cancel must persist the cancelled state"
    );
    assert_harness_status(&status_args, "cancelled");
}

fn read_progress_until(child: &mut Child, target_downloaded: u64, timeout_dur: Duration) -> u64 {
    let stdout = child.stdout.take().expect("child stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let start = Instant::now();
    let mut last = 0u64;
    while start.elapsed() < timeout_dur {
        line.clear();
        // non-blocking-ish read by checking child status and using small waits is hard with blocking BufRead;
        // use a simple approach: read line and rely on harness printing progress; if it ends early, break.
        let n = reader.read_line(&mut line).expect("read stdout line");
        if n == 0 {
            break;
        }
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("progress ") {
            if let Ok(v) = rest.trim().parse::<u64>() {
                last = v;
                if v >= target_downloaded {
                    // put stdout back? can't; but we have reached target and will kill soon.
                    // store reader in child is not possible; leak is fine for test (child killed).
                    // To keep handle, we can't easily; instead we just return and kill.
                    // Prevent dropping reader from closing pipe issues by forgetting it.
                    std::mem::forget(reader);
                    return last;
                }
            }
        }
    }
    std::mem::forget(reader);
    panic!(
        "did not observe progress >= {target_downloaded} within {:?}, last={last}",
        timeout_dur
    );
}

#[test]
fn kill_and_restart_harness_recovers_with_bounded_redownload_and_no_orphans() {
    let body = pattern_bytes(73, 8 * 1024 * 1024);
    let expected_hash = sha256_hex(&body);
    let server = RangeServer::start(FixtureConfig {
        body: body.clone(),
        range_mode: RangeMode::HonorRange,
        stream_delay: Some(Duration::from_millis(2)),
        stream_chunk_size: 16 * 1024,
        ..Default::default()
    });
    let url = server.url("/harness.bin");

    let dest = tempdir().expect("dest");
    let store_dir = tempdir().expect("store");
    let db_path = store_dir.path().join("tasks.db");
    let filename = "harness.bin";

    let db_path_s = db_path.to_string_lossy().to_string();
    let dest_s = dest.path().to_string_lossy().to_string();

    let mut child = Command::new(harness_exe())
        .args([
            "run",
            "--db",
            &db_path_s,
            "--dest",
            &dest_s,
            "--url",
            &url,
            "--filename",
            filename,
            "--connections",
            "8",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn harness run");

    let target = (body.len() as u64) * 40 / 100;
    let _progress = read_progress_until(&mut child, target, Duration::from_secs(60));

    // kill the harness process
    let _ = child.kill();
    let _ = child.wait();

    assert!(
        !dest.path().join(filename).exists(),
        "no final file must exist yet after kill"
    );

    // capture pre-restart state: segment high-water marks from store and served ranges snapshot
    let store = Database::open(&db_path).expect("open store after kill");
    let tasks = store.list_tasks().expect("list tasks");
    assert_eq!(tasks.len(), 1, "expected exactly one task");
    let task_id = tasks[0].id.clone();
    let segments = store.segments_for_task(&task_id).expect("segments");
    let pre_high_waters: Vec<(u64, u64)> = segments
        .iter()
        .map(|s| (s.start, s.downloaded_bytes))
        .collect();
    let served_before_restart = server.served_ranges();

    // restart harness on same db/dest
    let mut restart = Command::new(harness_exe())
        .args([
            "run",
            "--db",
            &db_path_s,
            "--dest",
            &dest_s,
            "--url",
            &url,
            "--filename",
            filename,
            "--connections",
            "8",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn harness restart");

    let mut stdout = String::new();
    restart
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .expect("read restart stdout");
    let mut stderr = String::new();
    restart
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .expect("read restart stderr");
    let status = restart.wait().expect("wait restart");
    assert!(
        status.success(),
        "restart harness must exit success, status={status:?}, stdout={stdout}, stderr={stderr}"
    );

    let final_path = dest.path().join(filename);
    assert!(final_path.exists(), "final file must exist after restart");
    assert_eq!(read_file(&final_path), body, "final bytes must match");
    assert_eq!(sha256_hex(&body), expected_hash, "sanity: expected hash");

    let served_after = server.served_ranges();
    // only ranges served after restart
    let new_ranges: Vec<ServedRange> = if served_after.len() >= served_before_restart.len() {
        served_after[served_before_restart.len()..].to_vec()
    } else {
        served_after.clone()
    };
    let per_segment = bytes_reserved_below_high_water(&new_ranges, &pre_high_waters);
    for (i, bytes) in per_segment.iter().enumerate() {
        assert!(
            *bytes <= ONE_MIB,
            "segment {i} re-downloaded {bytes} bytes below pre-kill high-water mark after restart"
        );
    }

    // bounded settle: no orphan request after restart completes
    let count_after = server.served_ranges().len();
    std::thread::sleep(SETTLE);
    let count_settled = server.served_ranges().len();
    assert_eq!(
        count_after, count_settled,
        "no orphan requests should be observed after restart completes"
    );
}
