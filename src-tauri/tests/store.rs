use std::fs;

use rusqlite::Connection;
use tempfile::tempdir;

use oneget_lib::engine::error::EngineError;
use oneget_lib::engine::state::{SegmentState, TaskState};
use oneget_lib::store::{Database, SegmentInput, SegmentProgressInput};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn open_temp_db() -> (tempfile::TempDir, Database) {
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("tasks.sqlite");
    let db = Database::open(&db_path).expect("open new database");
    (dir, db)
}

fn create_queued_task(db: &Database) -> String {
    db.create_task("https://example.invalid/file.bin", "file.bin")
        .expect("create queued task")
}

#[test]
fn opening_new_database_creates_schema_version_2_and_required_structures_idempotently() {
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("store.sqlite");

    {
        let db = Database::open(&db_path).expect("first open creates schema");
        let meta = db.schema_metadata().expect("read schema metadata");
        assert_eq!(meta.version, 2, "schema version must be 2");
        assert!(
            meta.journal_mode.eq_ignore_ascii_case("wal"),
            "journal_mode must be WAL"
        );
        assert!(meta.foreign_keys_enabled, "foreign_keys must be ON");
        assert_eq!(
            meta.busy_timeout_ms, 5_000,
            "busy timeout must be 5 seconds"
        );
        assert!(
            meta.has_required_indexes,
            "required indexes on task state/creation time and segment task must exist"
        );
        assert!(
            meta.has_required_constraints,
            "required constraints on schema_meta, tasks, segments must exist"
        );
    }

    let before = fs::metadata(&db_path).expect("db metadata before reopen");
    {
        let _db = Database::open(&db_path).expect("second open must be idempotent");
    }
    let after = fs::metadata(&db_path).expect("db metadata after reopen");
    assert_eq!(
        before.len(),
        after.len(),
        "idempotent reopen must not rewrite schema"
    );
}

#[test]
fn create_task_at_persists_destination_and_completion_records_completed_at_ms() {
    let (_dir, db) = open_temp_db();

    let destination = "/tmp/oneget-downloads";
    let task_id = db
        .create_task_at(
            "https://example.invalid/with-destination.bin",
            "with-destination.bin",
            Some(destination),
        )
        .expect("create task with destination");
    let created = db.task(&task_id).expect("lookup").expect("exists");
    assert_eq!(
        created.destination.as_deref(),
        Some(destination),
        "create_task_at must persist the requested destination"
    );
    assert!(
        created.completed_at_ms.is_none(),
        "a new task has no completion time"
    );

    let legacy_id = db
        .create_task("https://example.invalid/legacy.bin", "legacy.bin")
        .expect("create legacy task");
    let legacy = db.task(&legacy_id).expect("lookup").expect("exists");
    assert!(
        legacy.destination.is_none(),
        "create_task must keep the destination unset for the engine default"
    );
    assert!(legacy.completed_at_ms.is_none());

    db.transition_task(&task_id, TaskState::Queued, TaskState::Connecting)
        .expect("setup connecting");
    db.transition_task(&task_id, TaskState::Connecting, TaskState::Downloading)
        .expect("setup downloading");
    db.transition_task(&task_id, TaskState::Downloading, TaskState::Finalizing)
        .expect("setup finalizing");
    db.complete_task(&task_id, Some("c".repeat(64).as_str()))
        .expect("complete task");

    let completed = db.task(&task_id).expect("lookup").expect("exists");
    assert_eq!(completed.state, TaskState::Completed);
    let completed_at_ms = completed
        .completed_at_ms
        .expect("complete_task must record completed_at_ms");
    assert!(
        completed_at_ms >= completed.created_at_ms,
        "completion time must not precede creation time"
    );
}

#[test]
fn delete_task_removes_the_task_and_cascades_segments_without_touching_other_tasks() {
    let (_dir, db) = open_temp_db();

    let removed_id = create_queued_task(&db);
    db.replace_segments(
        &removed_id,
        &[
            SegmentInput {
                index: 0,
                start: 0,
                end: 1024,
                downloaded: 0,
                state: SegmentState::Pending,
            },
            SegmentInput {
                index: 1,
                start: 1024,
                end: 2048,
                downloaded: 0,
                state: SegmentState::Pending,
            },
        ],
    )
    .expect("segments for removed task");

    let survivor_id = create_queued_task(&db);
    db.replace_segments(
        &survivor_id,
        &[SegmentInput {
            index: 0,
            start: 0,
            end: 4096,
            downloaded: 512,
            state: SegmentState::Active,
        }],
    )
    .expect("segments for survivor task");

    db.delete_task(&removed_id)
        .expect("delete the selected task and its segments");

    assert!(
        db.task(&removed_id).expect("lookup removed task").is_none(),
        "the deleted task row must be gone"
    );
    assert!(
        db.segments_for_task(&removed_id)
            .expect("segments for removed task")
            .is_empty(),
        "deleting a task must cascade its segments"
    );

    assert!(
        db.task(&survivor_id)
            .expect("lookup survivor task")
            .is_some(),
        "deleting one task must not remove another task"
    );
    assert_eq!(
        db.segments_for_task(&survivor_id)
            .expect("survivor segments")
            .len(),
        1,
        "deleting one task must not remove another task's segments"
    );
    assert_eq!(
        db.list_tasks().expect("list tasks").len(),
        1,
        "only the survivor remains"
    );
}

#[test]
fn migrating_schema_1_database_preserves_rows_and_is_idempotent_on_reopen() {
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("store.sqlite");

    {
        let conn = Connection::open(&db_path).expect("create raw schema-1 database");
        conn.execute_batch(
            "PRAGMA foreign_keys=ON;
             CREATE TABLE schema_meta (
                 key TEXT PRIMARY KEY NOT NULL,
                 value TEXT NOT NULL
             );
             CREATE TABLE tasks (
                 id TEXT PRIMARY KEY NOT NULL,
                 url TEXT NOT NULL,
                 filename TEXT NOT NULL,
                 part_path TEXT,
                 state TEXT NOT NULL CHECK (state IN (
                     'queued', 'connecting', 'downloading', 'paused',
                     'finalizing', 'completed', 'failed', 'cancelled'
                 )),
                 downloaded_bytes INTEGER NOT NULL DEFAULT 0 CHECK (downloaded_bytes >= 0),
                 total_bytes INTEGER CHECK (total_bytes IS NULL OR total_bytes >= 0),
                 expected_sha256 TEXT,
                 actual_sha256 TEXT,
                 etag TEXT,
                 last_modified TEXT,
                 error_code TEXT,
                 error_message TEXT,
                 created_at_ms INTEGER NOT NULL,
                 updated_at_ms INTEGER NOT NULL,
                 connections INTEGER NOT NULL DEFAULT 1 CHECK (connections > 0),
                 headers_json TEXT NOT NULL DEFAULT '[]'
             );
             CREATE TABLE segments (
                 task_id TEXT NOT NULL,
                 segment_index INTEGER NOT NULL CHECK (segment_index >= 0),
                 start_offset INTEGER NOT NULL CHECK (start_offset >= 0),
                 end_offset INTEGER NOT NULL CHECK (end_offset > start_offset),
                 downloaded_bytes INTEGER NOT NULL DEFAULT 0 CHECK (downloaded_bytes >= 0),
                 state TEXT NOT NULL CHECK (state IN ('pending', 'active', 'done', 'failed')),
                 PRIMARY KEY (task_id, segment_index),
                 FOREIGN KEY (task_id) REFERENCES tasks (id) ON DELETE CASCADE,
                 CHECK (downloaded_bytes <= end_offset - start_offset)
             );
             CREATE INDEX idx_tasks_state_created_at ON tasks (state, created_at_ms);
             CREATE INDEX idx_segments_task_id ON segments (task_id);
             INSERT INTO schema_meta (key, value) VALUES ('schema_version', '1');
             INSERT INTO tasks (
                 id, url, filename, state, downloaded_bytes, created_at_ms,
                 updated_at_ms, connections
             ) VALUES (
                 'legacy-task', 'https://example.invalid/legacy.bin', 'legacy.bin',
                 'queued', 0, 1000, 1000, 4
             );
             INSERT INTO segments (
                 task_id, segment_index, start_offset, end_offset, downloaded_bytes, state
             ) VALUES ('legacy-task', 0, 0, 1024, 0, 'pending');",
        )
        .expect("write schema-1 fixture");
    }

    let db = Database::open(&db_path).expect("schema-1 database must migrate");
    assert_eq!(
        db.schema_metadata().expect("metadata").version,
        2,
        "migration must reach schema version 2"
    );
    let task = db
        .task("legacy-task")
        .expect("lookup migrated task")
        .expect("migrated row must be preserved");
    assert_eq!(task.filename, "legacy.bin");
    assert_eq!(task.connections, 4);
    assert_eq!(task.state, TaskState::Queued);
    assert!(
        task.destination.is_none(),
        "a migrated row has no recorded destination and falls back to the engine default"
    );
    assert!(
        task.completed_at_ms.is_none(),
        "a migrated row has no completion time"
    );
    assert_eq!(
        db.segments_for_task("legacy-task")
            .expect("migrated segments")
            .len(),
        1,
        "existing segments must survive the migration"
    );

    drop(db);
    let reopened = Database::open(&db_path).expect("reopen migrated database");
    assert_eq!(
        reopened.schema_metadata().expect("metadata").version,
        2,
        "reopening a migrated database stays at version 2"
    );
    assert!(
        reopened
            .task("legacy-task")
            .expect("lookup after reopen")
            .is_some(),
        "the migration must be idempotent and keep existing rows"
    );
    assert_eq!(
        reopened
            .segments_for_task("legacy-task")
            .expect("segments after reopen")
            .len(),
        1,
        "reopening must not drop existing segments"
    );
}

#[test]
fn database_with_schema_version_newer_than_supported_is_refused_with_typed_error() {
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("store.sqlite");
    {
        let _db = Database::open(&db_path).expect("create schema version 2 database");
    }

    {
        let conn = Connection::open(&db_path).expect("open raw sqlite connection");
        conn.execute(
            "UPDATE schema_meta SET value='999' WHERE key='schema_version'",
            [],
        )
        .expect("set newer schema version");
    }

    let err = Database::open(&db_path).expect_err("newer schema must be refused");
    assert_typed_engine_error_code(&err, "SCHEMA_TOO_NEW");
}

#[test]
fn failing_migration_rolls_back_schema_change_and_leaves_backup_beside_database() {
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("store.sqlite");
    {
        let _db = Database::open(&db_path).expect("create schema version 2 database");
    }

    // Craft a state that requires migration but makes applying migration fail:
    // force schema version down and leave a tasks table that lacks required CHECK constraints.
    {
        let conn = Connection::open(&db_path).expect("open raw sqlite connection");
        conn.execute_batch(
            "PRAGMA foreign_keys=OFF;
UPDATE schema_meta SET value='0' WHERE key='schema_version';
DROP TABLE IF EXISTS segments;
DROP TABLE IF EXISTS tasks;
CREATE TABLE tasks(id TEXT PRIMARY KEY, broken_column_does_not_match_expected_schema INTEGER);",
        )
        .expect("craft failing migration fixture");
    }

    let _err = Database::open(&db_path).expect_err("failing migration must be refused");

    let backup_path = dir.path().join("store.sqlite.backup");
    assert!(
        backup_path.exists(),
        "a migration backup file ending in .backup must exist beside the database"
    );

    let conn = Connection::open(&db_path).expect("reopen raw sqlite connection after failure");
    let stored_version: String = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )
        .expect("read stored schema version");
    assert_eq!(
        stored_version, "0",
        "migration transaction rollback must leave stored schema version unchanged"
    );
    let segments_present: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='segments'",
            [],
            |row| row.get(0),
        )
        .expect("check segments table presence");
    assert_eq!(
        segments_present, 0,
        "migration transaction rollback must leave the segments table absent"
    );
}

#[cfg(unix)]
#[test]
fn database_file_permissions_are_owner_only_on_unix() {
    let (_dir, _db) = open_temp_db();
    let dir = tempdir().expect("tempdir");
    let db_path = dir.path().join("store.sqlite");
    let _db = Database::open(&db_path).expect("open database");
    let mode = fs::metadata(&db_path)
        .expect("db metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "database file mode must be 0600 on Unix");
}

#[test]
fn task_lifecycle_supports_create_lookup_list_claim_count_state_error_and_paused_progress() {
    let (_dir, db) = open_temp_db();

    let first_id = db
        .create_task("https://example.invalid/first", "first.bin")
        .expect("create first task");
    let second_id = db
        .create_task("https://example.invalid/second", "second.bin")
        .expect("create second task");

    let first = db
        .task(&first_id)
        .expect("lookup first")
        .expect("first exists");
    assert_eq!(first.id, first_id);
    assert_eq!(first.state, TaskState::Queued);

    let listed = db.list_tasks().expect("list tasks");
    assert_eq!(listed.len(), 2, "list must return both tasks");

    let claimed = db
        .claim_oldest_queued_task()
        .expect("claim oldest queued")
        .expect("a queued task exists");
    assert_eq!(
        claimed.id, first_id,
        "claim must return oldest queued task in FIFO order"
    );

    db.transition_task(&first_id, TaskState::Connecting, TaskState::Downloading)
        .expect("set downloading");
    db.transition_task(&second_id, TaskState::Queued, TaskState::Connecting)
        .expect("set second connecting");
    let active = db
        .active_task_count()
        .expect("active task count for connecting/downloading/finalizing");
    assert_eq!(active, 2, "connecting and downloading tasks are active");

    db.transition_task(&second_id, TaskState::Connecting, TaskState::Downloading)
        .expect("set second downloading");
    db.transition_task(&second_id, TaskState::Downloading, TaskState::Finalizing)
        .expect("set finalizing");
    let active_finalizing = db.active_task_count().expect("count with finalizing");
    assert_eq!(active_finalizing, 2, "finalizing task is active");

    db.update_task_error(&first_id, "NETWORK_TIMEOUT", "timeout")
        .expect("record task error");
    let after_error = db
        .task(&first_id)
        .expect("lookup after error")
        .expect("exists");
    assert_eq!(
        after_error.error_code.as_deref(),
        Some("NETWORK_TIMEOUT"),
        "state/error update must persist error code"
    );

    let paused_segments = vec![
        SegmentProgressInput {
            segment_id: 0,
            downloaded_bytes: 1_048_576,
        },
        SegmentProgressInput {
            segment_id: 1,
            downloaded_bytes: 524_288,
        },
    ];
    db.persist_paused_progress(&first_id, &paused_segments, 1_572_864)
        .expect("persist paused progress snapshot");
    let paused_task = db
        .task(&first_id)
        .expect("lookup paused task")
        .expect("exists");
    assert_eq!(
        paused_task.state,
        TaskState::Paused,
        "persist_paused_progress must move a downloading task to paused"
    );
    assert_eq!(
        paused_task.downloaded_bytes, 1_572_864,
        "paused progress must persist recomputed downloaded bytes snapshot"
    );

    let completed_id = create_queued_task(&db);
    db.transition_task(&completed_id, TaskState::Queued, TaskState::Connecting)
        .expect("setup completed connecting");
    db.transition_task(&completed_id, TaskState::Connecting, TaskState::Downloading)
        .expect("setup completed downloading");
    db.transition_task(&completed_id, TaskState::Downloading, TaskState::Finalizing)
        .expect("setup completed finalizing");
    db.complete_task(&completed_id, Some(&"a".repeat(64)))
        .expect("setup completed task");
    let err = db
        .persist_paused_progress::<SegmentProgressInput>(&completed_id, &[], 0)
        .expect_err("persist_paused_progress on completed task must be rejected");
    assert_typed_engine_error_code(&err, "INVALID_STATE");
    let completed_task = db
        .task(&completed_id)
        .expect("lookup completed task")
        .expect("exists");
    assert_eq!(
        completed_task.state,
        TaskState::Completed,
        "persist_paused_progress must leave a completed task unchanged"
    );
}

#[test]
fn valid_task_transitions_persist_and_invalid_transitions_return_invalid_state_without_change() {
    let (_dir, db) = open_temp_db();

    let path_a = create_queued_task(&db);
    db.transition_task(&path_a, TaskState::Queued, TaskState::Connecting)
        .expect("queued->connecting");
    db.transition_task(&path_a, TaskState::Connecting, TaskState::Downloading)
        .expect("connecting->downloading");
    db.transition_task(&path_a, TaskState::Downloading, TaskState::Finalizing)
        .expect("downloading->finalizing");
    db.transition_task(&path_a, TaskState::Finalizing, TaskState::Completed)
        .expect("finalizing->completed allowed at state level");

    let queued_pause = create_queued_task(&db);
    db.transition_task(&queued_pause, TaskState::Queued, TaskState::Paused)
        .expect("queued->paused");

    let downloading_pause = create_queued_task(&db);
    db.transition_task(&downloading_pause, TaskState::Queued, TaskState::Connecting)
        .expect("setup connecting");
    db.transition_task(
        &downloading_pause,
        TaskState::Connecting,
        TaskState::Downloading,
    )
    .expect("setup downloading");
    db.transition_task(
        &downloading_pause,
        TaskState::Downloading,
        TaskState::Paused,
    )
    .expect("downloading->paused");

    let paused_resume = create_queued_task(&db);
    db.transition_task(&paused_resume, TaskState::Queued, TaskState::Paused)
        .expect("setup paused");
    db.transition_task(&paused_resume, TaskState::Paused, TaskState::Queued)
        .expect("paused->queued");

    let failed_retry = create_queued_task(&db);
    db.transition_task(&failed_retry, TaskState::Queued, TaskState::Connecting)
        .expect("setup connecting");
    db.transition_task(&failed_retry, TaskState::Connecting, TaskState::Failed)
        .expect("connecting->failed");
    db.transition_task(&failed_retry, TaskState::Failed, TaskState::Queued)
        .expect("failed->queued");

    let cancellable = create_queued_task(&db);
    db.transition_task(&cancellable, TaskState::Queued, TaskState::Cancelled)
        .expect("queued->cancelled");
    let cancelled_again =
        db.transition_task(&cancellable, TaskState::Cancelled, TaskState::Cancelled);
    match cancelled_again {
        Ok(()) => {}
        Err(e) => assert_typed_engine_error_code(&e, "INVALID_STATE"),
    }

    let invalid = create_queued_task(&db);
    let err = db
        .transition_task(&invalid, TaskState::Queued, TaskState::Completed)
        .expect_err("queued->completed must be invalid");
    assert_typed_engine_error_code(&err, "INVALID_STATE");
    let unchanged = db.task(&invalid).expect("lookup").expect("exists");
    assert_eq!(
        unchanged.state,
        TaskState::Queued,
        "invalid transition must leave stored state unchanged"
    );

    let completed = create_queued_task(&db);
    db.transition_task(&completed, TaskState::Queued, TaskState::Connecting)
        .expect("setup");
    db.transition_task(&completed, TaskState::Connecting, TaskState::Downloading)
        .expect("setup");
    db.transition_task(&completed, TaskState::Downloading, TaskState::Finalizing)
        .expect("setup");
    db.transition_task(&completed, TaskState::Finalizing, TaskState::Completed)
        .expect("setup completed");
    let err = db
        .transition_task(&completed, TaskState::Completed, TaskState::Downloading)
        .expect_err("completed->downloading must be invalid");
    assert_typed_engine_error_code(&err, "INVALID_STATE");
    assert_eq!(
        db.task(&completed).unwrap().unwrap().state,
        TaskState::Completed
    );

    let cancelled = create_queued_task(&db);
    db.transition_task(&cancelled, TaskState::Queued, TaskState::Cancelled)
        .expect("cancel");
    let err = db
        .transition_task(&cancelled, TaskState::Cancelled, TaskState::Downloading)
        .expect_err("cancelled->downloading must be invalid");
    assert_typed_engine_error_code(&err, "INVALID_STATE");
    assert_eq!(
        db.task(&cancelled).unwrap().unwrap().state,
        TaskState::Cancelled
    );
}

#[test]
fn segment_replacement_is_atomic_enforces_ranges_foreign_keys_and_persists_large_u64_offsets() {
    let (_dir, db) = open_temp_db();
    let task_id = create_queued_task(&db);

    let segments = vec![
        SegmentInput {
            index: 0,
            start: 0,
            end: 1_024,
            downloaded: 0,
            state: SegmentState::Pending,
        },
        SegmentInput {
            index: 1,
            start: 4_500_000_000,
            end: 4_500_001_024,
            downloaded: 0,
            state: SegmentState::Pending,
        },
    ];
    db.replace_segments(&task_id, &segments)
        .expect("replace segments atomically");
    let stored = db.segments_for_task(&task_id).expect("load segments");
    assert_eq!(stored.len(), 2);
    assert_eq!(
        stored[1].start, 4_500_000_000,
        "segment start above 4 GiB must persist losslessly"
    );
    assert_eq!(stored[1].end, 4_500_001_024);

    let replacement = vec![SegmentInput {
        index: 0,
        start: 0,
        end: 2_048,
        downloaded: 0,
        state: SegmentState::Pending,
    }];
    db.replace_segments(&task_id, &replacement)
        .expect("replace all segments in one transaction");
    let after = db
        .segments_for_task(&task_id)
        .expect("segments after replace");
    assert_eq!(
        after.len(),
        1,
        "replacement must replace all segments for task"
    );

    let invalid_end = vec![SegmentInput {
        index: 0,
        start: 100,
        end: 99,
        downloaded: 0,
        state: SegmentState::Pending,
    }];
    let err = db
        .replace_segments(&task_id, &invalid_end)
        .expect_err("end < start must be rejected");
    assert_typed_engine_error(&err);

    let invalid_equal = vec![SegmentInput {
        index: 0,
        start: 100,
        end: 100,
        downloaded: 0,
        state: SegmentState::Pending,
    }];
    let err = db
        .replace_segments(&task_id, &invalid_equal)
        .expect_err("start == end must be rejected");
    assert_typed_engine_error(&err);

    let missing_task = db.replace_segments(
        "task-does-not-exist",
        &[SegmentInput {
            index: 0,
            start: 0,
            end: 1,
            downloaded: 0,
            state: SegmentState::Pending,
        }],
    );
    assert!(
        missing_task.is_err(),
        "segment belonging to a missing task must be rejected with foreign key enforcement"
    );
}

#[test]
fn segment_state_transitions_allow_defined_moves_and_reject_invalid_moves_with_invalid_state() {
    let (_dir, db) = open_temp_db();
    let task_id = create_queued_task(&db);
    db.replace_segments(
        &task_id,
        &[SegmentInput {
            index: 0,
            start: 0,
            end: 1024,
            downloaded: 0,
            state: SegmentState::Pending,
        }],
    )
    .expect("create segment");

    db.transition_segment(&task_id, 0, SegmentState::Pending, SegmentState::Active)
        .expect("pending->active");
    db.transition_segment(&task_id, 0, SegmentState::Active, SegmentState::Done)
        .expect("active->done");

    db.replace_segments(
        &task_id,
        &[SegmentInput {
            index: 0,
            start: 0,
            end: 1024,
            downloaded: 512,
            state: SegmentState::Active,
        }],
    )
    .expect("reset segment");
    db.transition_segment(&task_id, 0, SegmentState::Active, SegmentState::Failed)
        .expect("active->failed");
    db.transition_segment(&task_id, 0, SegmentState::Failed, SegmentState::Pending)
        .expect("failed->pending");

    db.replace_segments(
        &task_id,
        &[SegmentInput {
            index: 0,
            start: 0,
            end: 1024,
            downloaded: 512,
            state: SegmentState::Active,
        }],
    )
    .expect("reset segment for pause");
    db.transition_segment(&task_id, 0, SegmentState::Active, SegmentState::Pending)
        .expect("active->pending on pause");

    db.replace_segments(
        &task_id,
        &[SegmentInput {
            index: 0,
            start: 0,
            end: 1024,
            downloaded: 1024,
            state: SegmentState::Done,
        }],
    )
    .expect("set segment done");
    let err = db
        .transition_segment(&task_id, 0, SegmentState::Done, SegmentState::Active)
        .expect_err("done->active must be rejected");
    assert_typed_engine_error_code(&err, "INVALID_STATE");

    db.replace_segments(
        &task_id,
        &[SegmentInput {
            index: 0,
            start: 0,
            end: 1024,
            downloaded: 0,
            state: SegmentState::Failed,
        }],
    )
    .expect("reset failed segment");
    let err = db
        .transition_segment(&task_id, 0, SegmentState::Failed, SegmentState::Done)
        .expect_err("failed->done must be rejected");
    assert_typed_engine_error_code(&err, "INVALID_STATE");
}

#[test]
fn completing_task_requires_stored_actual_sha256_hash_value() {
    let (_dir, db) = open_temp_db();
    let task_id = create_queued_task(&db);
    db.transition_task(&task_id, TaskState::Queued, TaskState::Connecting)
        .expect("setup");
    db.transition_task(&task_id, TaskState::Connecting, TaskState::Downloading)
        .expect("setup");
    db.transition_task(&task_id, TaskState::Downloading, TaskState::Finalizing)
        .expect("setup");

    let err = db
        .complete_task(&task_id, None)
        .expect_err("completion without stored actual SHA-256 hash must be rejected");
    assert_typed_engine_error(&err);
    let task = db.task(&task_id).expect("lookup").expect("exists");
    assert_ne!(
        task.state,
        TaskState::Completed,
        "no completed row without hash"
    );

    let ok = db.complete_task(&task_id, Some("a".repeat(64).as_str()));
    assert!(
        ok.is_ok(),
        "completion with stored actual SHA-256 hash value must be accepted by store"
    );
}

#[test]
fn failed_tasks_preserve_recorded_part_path_and_segments_and_cancelled_tasks_keep_only_own_row() {
    let (_dir, db) = open_temp_db();
    let failed_id = create_queued_task(&db);
    db.replace_segments(
        &failed_id,
        &[SegmentInput {
            index: 0,
            start: 0,
            end: 2048,
            downloaded: 512,
            state: SegmentState::Failed,
        }],
    )
    .expect("segments for failed task");
    db.record_part_path(&failed_id, "/tmp/downloads/file.bin.oneget.part")
        .expect("record part path");
    db.transition_task(&failed_id, TaskState::Queued, TaskState::Connecting)
        .expect("setup");
    db.transition_task(&failed_id, TaskState::Connecting, TaskState::Downloading)
        .expect("setup");
    db.transition_task(&failed_id, TaskState::Downloading, TaskState::Failed)
        .expect("mark failed");

    let failed_task = db.task(&failed_id).expect("lookup").expect("exists");
    assert_eq!(
        failed_task.part_path.as_deref(),
        Some("/tmp/downloads/file.bin.oneget.part"),
        "failed task must preserve recorded part path"
    );
    let failed_segments = db.segments_for_task(&failed_id).expect("segments");
    assert_eq!(
        failed_segments.len(),
        1,
        "failed task must preserve segment progress"
    );

    let cancelled_id = create_queued_task(&db);
    db.replace_segments(
        &cancelled_id,
        &[SegmentInput {
            index: 0,
            start: 0,
            end: 1024,
            downloaded: 256,
            state: SegmentState::Active,
        }],
    )
    .expect("segments for cancelled task");
    db.transition_task(&cancelled_id, TaskState::Queued, TaskState::Cancelled)
        .expect("cancel task at store level");

    let cancelled_task = db.task(&cancelled_id).expect("lookup").expect("exists");
    assert_eq!(
        cancelled_task.state,
        TaskState::Cancelled,
        "cancelled task must keep its own row"
    );

    let survivor_id = create_queued_task(&db);
    db.replace_segments(
        &survivor_id,
        &[SegmentInput {
            index: 0,
            start: 0,
            end: 4096,
            downloaded: 1024,
            state: SegmentState::Active,
        }],
    )
    .expect("segments for survivor task");
    db.record_part_path(&survivor_id, "/tmp/downloads/survivor.bin.oneget.part")
        .expect("record survivor part path");

    // Re-cancel or ensure cancelled state operations do not affect other tasks at store level.
    let _ = db.transition_task(&cancelled_id, TaskState::Cancelled, TaskState::Cancelled);
    let survivor = db
        .task(&survivor_id)
        .expect("lookup survivor")
        .expect("exists");
    assert_eq!(
        survivor.part_path.as_deref(),
        Some("/tmp/downloads/survivor.bin.oneget.part"),
        "another task's part path must remain unchanged"
    );
    let survivor_segments = db
        .segments_for_task(&survivor_id)
        .expect("survivor segments");
    assert_eq!(
        survivor_segments.len(),
        1,
        "another task's segments must remain unchanged"
    );
}

fn assert_typed_engine_error(err: &EngineError) {
    let _ = err.code();
    let _ = err.to_string();
}

fn assert_typed_engine_error_code(err: &EngineError, expected: &str) {
    assert_eq!(err.code(), expected, "unexpected engine error code");
}
