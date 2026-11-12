//! Task persistence and lifecycle transitions.

use std::borrow::Borrow;

use rusqlite::{params, OptionalExtension, Row};

use super::db::now_ms;
use super::types::{SegmentProgressInput, Task};
use super::Database;
use crate::engine::error::EngineError;
use crate::engine::state::TaskState;

const TASK_COLUMNS: &str = "id, url, filename, part_path, state, downloaded_bytes, \
     total_bytes, expected_sha256, actual_sha256, etag, last_modified, error_code, error_message, \
     created_at_ms, updated_at_ms";

impl Database {
    /// Insert a new queued task and return its identifier.
    pub fn create_task(&self, url: &str, filename: &str) -> Result<String, EngineError> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO tasks (id, url, filename, state, downloaded_bytes, created_at_ms, updated_at_ms) \
             VALUES (?1, ?2, ?3, 'queued', 0, ?4, ?4)",
            params![id, url, filename, now],
        )?;
        Ok(id)
    }

    /// Look up one task by identifier.
    pub fn task(&self, id: &str) -> Result<Option<Task>, EngineError> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks WHERE id = ?1");
        Ok(self
            .conn
            .query_row(&sql, params![id], task_from_row)
            .optional()?)
    }

    /// List every task in FIFO creation order.
    pub fn list_tasks(&self) -> Result<Vec<Task>, EngineError> {
        let sql = format!("SELECT {TASK_COLUMNS} FROM tasks ORDER BY created_at_ms ASC, rowid ASC");
        let mut statement = self.conn.prepare(&sql)?;
        let tasks = statement
            .query_map([], task_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(tasks)
    }

    /// Claim the oldest queued task and move it to `connecting`.
    pub fn claim_oldest_queued_task(&self) -> Result<Option<Task>, EngineError> {
        let transaction = self.conn.unchecked_transaction()?;
        let id: Option<String> = transaction
            .query_row(
                "SELECT id FROM tasks WHERE state = 'queued' \
                 ORDER BY created_at_ms ASC, rowid ASC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;

        let Some(id) = id else {
            transaction.commit()?;
            return Ok(None);
        };

        transaction.execute(
            "UPDATE tasks SET state = 'connecting', updated_at_ms = ?1 \
             WHERE id = ?2 AND state = 'queued'",
            params![now_ms(), id],
        )?;
        transaction.commit()?;
        self.task(&id)
    }

    /// Count tasks that occupy a scheduler slot.
    pub fn active_task_count(&self) -> Result<i64, EngineError> {
        let count = self.conn.query_row(
            "SELECT COUNT(*) FROM tasks \
             WHERE state IN ('connecting', 'downloading', 'finalizing')",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Apply a state transition only when the stored state matches `from`.
    pub fn transition_task(
        &self,
        id: &str,
        from: TaskState,
        to: TaskState,
    ) -> Result<(), EngineError> {
        let stored: Option<String> = self
            .conn
            .query_row(
                "SELECT state FROM tasks WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(stored) = stored else {
            return Err(task_not_found());
        };
        if TaskState::parse(&stored) != Some(from) || !from.can_transition_to(to) {
            return Err(EngineError::invalid_state(
                "the requested task transition is not permitted",
            ));
        }

        let changed = self.conn.execute(
            "UPDATE tasks SET state = ?1, updated_at_ms = ?2 \
             WHERE id = ?3 AND state = ?4",
            params![to.as_str(), now_ms(), id, from.as_str()],
        )?;
        if changed == 0 {
            return Err(EngineError::invalid_state(
                "the task state changed before the transition could be applied",
            ));
        }
        Ok(())
    }

    /// Persist a stable failure code and user-safe message for a task.
    pub fn update_task_error(
        &self,
        id: &str,
        code: &str,
        message: &str,
    ) -> Result<(), EngineError> {
        let changed = self.conn.execute(
            "UPDATE tasks SET error_code = ?1, error_message = ?2, updated_at_ms = ?3 \
             WHERE id = ?4",
            params![code, message, now_ms(), id],
        )?;
        if changed == 0 {
            return Err(task_not_found());
        }
        Ok(())
    }

    /// Persist a paused progress snapshot and move a running task to `paused`.
    ///
    /// Only a queued, connecting, downloading, or already-paused task may
    /// persist paused progress. Any other stored state is rejected with
    /// `INVALID_STATE` and the row is left unchanged. Pausing a task that is
    /// already paused keeps the paused state and refreshes the snapshot.
    pub fn persist_paused_progress<T>(
        &self,
        id: &str,
        segments: &[T],
        downloaded_bytes: u64,
    ) -> Result<(), EngineError>
    where
        T: Borrow<SegmentProgressInput>,
    {
        let downloaded = offset_to_sql(downloaded_bytes)?;
        let transaction = self.conn.unchecked_transaction()?;

        let stored: Option<String> = transaction
            .query_row(
                "SELECT state FROM tasks WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(stored) = stored else {
            return Err(task_not_found());
        };
        let stored = TaskState::parse(&stored)
            .ok_or_else(|| EngineError::invalid_state("the stored task state is not recognized"))?;

        if stored == TaskState::Paused {
            transaction.execute(
                "UPDATE tasks SET downloaded_bytes = ?1, updated_at_ms = ?2 WHERE id = ?3",
                params![downloaded, now_ms(), id],
            )?;
        } else if stored.can_transition_to(TaskState::Paused) {
            let changed = transaction.execute(
                "UPDATE tasks SET state = 'paused', downloaded_bytes = ?1, updated_at_ms = ?2 \
                 WHERE id = ?3 AND state = ?4",
                params![downloaded, now_ms(), id, stored.as_str()],
            )?;
            if changed == 0 {
                return Err(EngineError::invalid_state(
                    "the task state changed before paused progress could be persisted",
                ));
            }
        } else {
            return Err(EngineError::invalid_state(
                "only a queued, connecting, downloading, or paused task can persist paused progress",
            ));
        }

        for segment in segments {
            let segment = segment.borrow();
            transaction.execute(
                "UPDATE segments SET downloaded_bytes = ?1 \
                 WHERE task_id = ?2 AND segment_index = ?3",
                params![
                    offset_to_sql(segment.downloaded_bytes)?,
                    id,
                    offset_to_sql(u64::from(segment.segment_id))?,
                ],
            )?;
        }

        transaction.commit()?;
        Ok(())
    }

    /// Persist a running task's segment progress and downloaded-byte snapshot.
    ///
    /// This never changes the task state. Only connecting, downloading, and
    /// finalizing tasks accept progress so a worker cannot write after a task
    /// was paused, cancelled, or completed.
    pub fn persist_progress(
        &self,
        id: &str,
        segments: &[SegmentProgressInput],
        downloaded_bytes: u64,
    ) -> Result<(), EngineError> {
        let downloaded = offset_to_sql(downloaded_bytes)?;
        let transaction = self.conn.unchecked_transaction()?;

        let stored: Option<String> = transaction
            .query_row(
                "SELECT state FROM tasks WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(stored) = stored else {
            return Err(task_not_found());
        };
        let stored = TaskState::parse(&stored)
            .ok_or_else(|| EngineError::invalid_state("the stored task state is not recognized"))?;
        if !matches!(
            stored,
            TaskState::Connecting | TaskState::Downloading | TaskState::Finalizing
        ) {
            return Err(EngineError::invalid_state(
                "only a connecting, downloading, or finalizing task can persist progress",
            ));
        }

        transaction.execute(
            "UPDATE tasks SET downloaded_bytes = ?1, updated_at_ms = ?2 WHERE id = ?3",
            params![downloaded, now_ms(), id],
        )?;

        for segment in segments {
            transaction.execute(
                "UPDATE segments SET downloaded_bytes = ?1 \
                 WHERE task_id = ?2 AND segment_index = ?3",
                params![
                    offset_to_sql(segment.downloaded_bytes)?,
                    id,
                    offset_to_sql(u64::from(segment.segment_id))?,
                ],
            )?;
        }

        transaction.commit()?;
        Ok(())
    }

    /// Record the remote metadata discovered while probing without changing state.
    pub fn record_discovered_metadata(
        &self,
        id: &str,
        total_bytes: impl Into<Option<u64>>,
        etag: Option<&str>,
        last_modified: Option<&str>,
    ) -> Result<(), EngineError> {
        let total: Option<u64> = total_bytes.into();
        let total = total.map(offset_to_sql).transpose()?;
        let changed = self.conn.execute(
            "UPDATE tasks SET total_bytes = ?1, etag = ?2, last_modified = ?3, updated_at_ms = ?4 \
             WHERE id = ?5",
            params![total, etag, last_modified, now_ms(), id],
        )?;
        if changed == 0 {
            return Err(task_not_found());
        }
        Ok(())
    }

    /// Record the SHA-256 computed for a task without changing its state.
    pub fn record_actual_hash(&self, id: &str, actual_sha256: &str) -> Result<(), EngineError> {
        validate_hash(actual_sha256)?;
        let changed = self.conn.execute(
            "UPDATE tasks SET actual_sha256 = ?1, updated_at_ms = ?2 WHERE id = ?3",
            params![actual_sha256, now_ms(), id],
        )?;
        if changed == 0 {
            return Err(task_not_found());
        }
        Ok(())
    }

    /// Mark a finalizing task completed once its actual SHA-256 is known.
    pub fn complete_task(&self, id: &str, actual_sha256: Option<&str>) -> Result<(), EngineError> {
        let hash = actual_sha256.ok_or_else(|| {
            EngineError::new(
                "CHECKSUM_MISSING",
                "a completed task requires its computed SHA-256 hash",
            )
        })?;
        validate_hash(hash)?;

        let changed = self.conn.execute(
            "UPDATE tasks SET state = 'completed', actual_sha256 = ?1, \
             error_code = NULL, error_message = NULL, updated_at_ms = ?2 \
             WHERE id = ?3 AND state = 'finalizing'",
            params![hash, now_ms(), id],
        )?;
        if changed == 0 {
            return Err(EngineError::invalid_state(
                "only a finalizing task can be completed",
            ));
        }
        Ok(())
    }

    /// Record the part file that belongs to a task.
    pub fn record_part_path(&self, id: &str, path: &str) -> Result<(), EngineError> {
        let changed = self.conn.execute(
            "UPDATE tasks SET part_path = ?1, updated_at_ms = ?2 WHERE id = ?3",
            params![path, now_ms(), id],
        )?;
        if changed == 0 {
            return Err(task_not_found());
        }
        Ok(())
    }
}

fn task_from_row(row: &Row<'_>) -> rusqlite::Result<Task> {
    let raw_state: String = row.get(4)?;
    let state = TaskState::parse(&raw_state).ok_or_else(|| {
        rusqlite::Error::InvalidColumnType(4, "state".to_owned(), rusqlite::types::Type::Text)
    })?;

    Ok(Task {
        id: row.get(0)?,
        url: row.get(1)?,
        filename: row.get(2)?,
        part_path: row.get(3)?,
        state,
        downloaded_bytes: read_u64(row, 5, "downloaded_bytes")?,
        total_bytes: read_optional_u64(row, 6, "total_bytes")?,
        expected_sha256: row.get(7)?,
        actual_sha256: row.get(8)?,
        etag: row.get(9)?,
        last_modified: row.get(10)?,
        error_code: row.get(11)?,
        error_message: row.get(12)?,
        created_at_ms: row.get(13)?,
        updated_at_ms: row.get(14)?,
    })
}

fn read_u64(row: &Row<'_>, index: usize, name: &str) -> rusqlite::Result<u64> {
    let raw: i64 = row.get(index)?;
    u64::try_from(raw).map_err(|_| {
        rusqlite::Error::InvalidColumnType(index, name.to_owned(), rusqlite::types::Type::Integer)
    })
}

fn read_optional_u64(row: &Row<'_>, index: usize, name: &str) -> rusqlite::Result<Option<u64>> {
    let raw: Option<i64> = row.get(index)?;
    match raw {
        Some(value) => u64::try_from(value).map(Some).map_err(|_| {
            rusqlite::Error::InvalidColumnType(
                index,
                name.to_owned(),
                rusqlite::types::Type::Integer,
            )
        }),
        None => Ok(None),
    }
}

pub(crate) fn offset_to_sql(value: u64) -> Result<i64, EngineError> {
    i64::try_from(value).map_err(|_| {
        EngineError::new(
            "OFFSET_OUT_OF_RANGE",
            "a byte offset is outside the supported 64-bit range",
        )
    })
}

fn task_not_found() -> EngineError {
    EngineError::new("TASK_NOT_FOUND", "the requested task does not exist")
}

fn validate_hash(hash: &str) -> Result<(), EngineError> {
    if hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(EngineError::new(
            "CHECKSUM_INVALID",
            "the computed SHA-256 hash is not a 64-character hexadecimal value",
        ))
    }
}
