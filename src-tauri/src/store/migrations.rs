//! Schema creation, version bookkeeping, and pre-migration backups.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension};

use crate::engine::error::EngineError;

/// Schema version this build creates and understands.
pub(crate) const SCHEMA_VERSION: i64 = 2;

const SCHEMA_V1: &str = "
CREATE TABLE IF NOT EXISTS schema_meta (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS tasks (
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
CREATE TABLE IF NOT EXISTS segments (
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
CREATE INDEX IF NOT EXISTS idx_tasks_state_created_at
    ON tasks (state, created_at_ms);
CREATE INDEX IF NOT EXISTS idx_segments_task_id
    ON segments (task_id);
INSERT OR REPLACE INTO schema_meta (key, value) VALUES ('schema_version', '1');
";

/// Schema 2 adds the per-task destination and the completion timestamp.
const SCHEMA_V2: &str = "
ALTER TABLE tasks ADD COLUMN destination TEXT;
ALTER TABLE tasks ADD COLUMN completed_at_ms INTEGER;
UPDATE schema_meta SET value = '2' WHERE key = 'schema_version';
";

/// Read the stored schema version, treating an absent table or row as version 0.
pub(crate) fn read_schema_version(conn: &Connection) -> rusqlite::Result<i64> {
    let has_meta: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='schema_meta')",
        [],
        |row| row.get(0),
    )?;
    if !has_meta {
        return Ok(0);
    }

    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key='schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    match value {
        Some(raw) => raw.parse::<i64>().map_err(|_| {
            rusqlite::Error::InvalidColumnType(
                0,
                "schema_version".to_owned(),
                rusqlite::types::Type::Text,
            )
        }),
        None => Ok(0),
    }
}

/// Apply every migration needed to reach [`SCHEMA_VERSION`] in one transaction.
pub(crate) fn migrate(conn: &Connection, from_version: i64) -> Result<(), EngineError> {
    if from_version >= SCHEMA_VERSION {
        return Ok(());
    }

    let transaction = conn
        .unchecked_transaction()
        .map_err(|_| migration_error())?;
    transaction
        .execute_batch(SCHEMA_V1)
        .map_err(|_| migration_error())?;
    transaction
        .execute_batch(SCHEMA_V2)
        .map_err(|_| migration_error())?;
    if !required_indexes_present(&transaction).map_err(|_| migration_error())?
        || !required_constraints_present(&transaction).map_err(|_| migration_error())?
    {
        return Err(migration_error());
    }
    transaction.commit().map_err(|_| migration_error())?;
    Ok(())
}

/// Copy the current database beside itself before applying a migration.
pub(crate) fn backup_before_migration(
    conn: &Connection,
    db_path: &Path,
) -> Result<PathBuf, EngineError> {
    let mut backup_name = OsString::from(db_path.as_os_str());
    backup_name.push(".backup");
    let backup_path = PathBuf::from(backup_name);

    if backup_path.exists() {
        std::fs::remove_file(&backup_path).map_err(|_| backup_error())?;
    }

    conn.backup(
        rusqlite::MAIN_DB,
        &backup_path,
        None::<fn(rusqlite::backup::Progress)>,
    )
    .map_err(|_| backup_error())?;
    Ok(backup_path)
}

/// Whether the queue and segment indexes are present.
pub(crate) fn required_indexes_present(conn: &Connection) -> rusqlite::Result<bool> {
    let mut statement = conn.prepare(
        "SELECT name FROM sqlite_master \
         WHERE type='index' AND name IN ('idx_tasks_state_created_at', 'idx_segments_task_id')",
    )?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(names.len() == 2)
}

/// Whether the three schema-1 tables carry their keys, checks, and foreign key.
pub(crate) fn required_constraints_present(conn: &Connection) -> rusqlite::Result<bool> {
    let mut statement = conn.prepare(
        "SELECT name, sql FROM sqlite_master \
         WHERE type='table' AND name IN ('schema_meta', 'tasks', 'segments')",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let find = |table: &str| {
        rows.iter()
            .find(|(name, _)| name == table)
            .and_then(|(_, sql)| sql.as_deref())
            .map(|sql| sql.to_ascii_uppercase())
    };

    let schema_meta = find("schema_meta");
    let tasks = find("tasks");
    let segments = find("segments");

    Ok(schema_meta.is_some_and(|sql| sql.contains("PRIMARY KEY"))
        && tasks.is_some_and(|sql| sql.contains("PRIMARY KEY") && sql.contains("CHECK"))
        && segments.is_some_and(|sql| {
            sql.contains("PRIMARY KEY") && sql.contains("FOREIGN KEY") && sql.contains("CHECK")
        }))
}

fn migration_error() -> EngineError {
    EngineError::new("MIGRATION_FAILED", "the task database migration failed")
}

fn backup_error() -> EngineError {
    EngineError::new(
        "BACKUP_FAILED",
        "the task database backup before migration failed",
    )
}
