//! Task persistence and lifecycle transitions.

use std::borrow::Borrow;

use rusqlite::{params, OptionalExtension, Row, Transaction, TransactionBehavior};

use super::db::now_ms;
use super::types::{SegmentProgressInput, Task};
use super::Database;
use crate::engine::error::EngineError;
use crate::engine::part_file::PartFile;
use crate::engine::state::TaskState;
use crate::platform::files::CapDir;

const TASK_COLUMNS: &str = "id, url, filename, part_path, state, downloaded_bytes, \
     total_bytes, expected_sha256, actual_sha256, etag, last_modified, error_code, error_message, \
     created_at_ms, updated_at_ms, connections, destination, completed_at_ms, final_url";

impl Database {
    /// Insert a new queued task and return its identifier.
    pub fn create_task(&self, url: &str, filename: &str) -> Result<String, EngineError> {
        self.create_task_at(url, filename, None)
    }

    /// Insert a new queued task with an optional per-task destination.
    pub fn create_task_at(
        &self,
        url: &str,
        filename: &str,
        destination: Option<&str>,
    ) -> Result<String, EngineError> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO tasks \
                 (id, url, filename, destination, state, downloaded_bytes, created_at_ms, updated_at_ms) \
             VALUES (?1, ?2, ?3, ?4, 'queued', 0, ?5, ?5)",
            params![id, url, filename, destination, now],
        )?;
        Ok(id)
    }

    /// Insert a new queued task with a collision-free part name.
    ///
    /// Conflict detection, part-name choice, and insertion share one immediate
    /// SQLite transaction, so two engines adding the same filename and
    /// destination cannot both claim the legacy `<filename>.oneget.part` name.
    /// A stored row without a destination is compared through the engine's
    /// default directory. Any same-filename task in the effective destination
    /// that is not completed or cancelled conflicts, as does a completed or
    /// cancelled task that still owns a part file.
    pub fn create_task_with_part(
        &self,
        url: &str,
        filename: &str,
        destination: Option<&str>,
        default_destination: &str,
    ) -> Result<(String, String), EngineError> {
        let id = uuid::Uuid::new_v4().to_string();
        let now = now_ms();
        let effective_destination = destination.unwrap_or(default_destination);
        let transaction = Transaction::new_unchecked(&self.conn, TransactionBehavior::Immediate)?;

        let collides = {
            let mut statement = transaction.prepare(
                "SELECT filename, state, part_path, destination \
                 FROM tasks WHERE filename = ?1",
            )?;
            let rows = statement.query_map(params![filename], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                ))
            })?;
            let mut collides = false;
            for row in rows {
                let (other_filename, other_state, other_part_path, other_destination) = row?;
                let other_effective = other_destination.as_deref().unwrap_or(default_destination);
                if other_effective != effective_destination {
                    continue;
                }
                let Some(other_state) = TaskState::parse(&other_state) else {
                    continue;
                };
                if !matches!(other_state, TaskState::Completed | TaskState::Cancelled)
                    || part_exists(
                        effective_destination,
                        other_part_path.as_deref(),
                        &other_filename,
                    )
                {
                    collides = true;
                    break;
                }
            }
            collides
        };

        let part_name = if collides {
            format!(
                "{filename}.{}.oneget.part",
                id.chars().take(8).collect::<String>()
            )
        } else {
            PartFile::part_name(filename)
        };

        transaction.execute(
            "INSERT INTO tasks \
                 (id, url, filename, part_path, destination, state, downloaded_bytes, created_at_ms, updated_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'queued', 0, ?6, ?6)",
            params![id, url, filename, part_name, destination, now],
        )?;
        transaction.commit()?;
        Ok((id, part_name))
    }

    /// Remove one task; its segments cascade through the foreign key.
    pub fn delete_task(&self, id: &str) -> Result<(), EngineError> {
        let changed = self
            .conn
            .execute("DELETE FROM tasks WHERE id = ?1", params![id])?;
        if changed == 0 {
            return Err(task_not_found());
        }
        Ok(())
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

    /// Count tasks waiting for a scheduler slot.
    pub fn queued_task_count(&self) -> Result<i64, EngineError> {
        let count = self.conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE state = 'queued'",
            [],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Record the expected SHA-256 for a task without changing its state.
    pub fn record_expected_sha256(
        &self,
        id: &str,
        expected_sha256: &str,
    ) -> Result<(), EngineError> {
        let changed = self.conn.execute(
            "UPDATE tasks SET expected_sha256 = ?1, updated_at_ms = ?2 WHERE id = ?3",
            params![expected_sha256, now_ms(), id],
        )?;
        if changed == 0 {
            return Err(task_not_found());
        }
        Ok(())
    }

    /// Record the requested connection count for a task.
    pub fn record_connections(&self, id: &str, connections: u32) -> Result<(), EngineError> {
        let changed = self.conn.execute(
            "UPDATE tasks SET connections = ?1, updated_at_ms = ?2 WHERE id = ?3",
            params![i64::from(connections), now_ms(), id],
        )?;
        if changed == 0 {
            return Err(task_not_found());
        }
        Ok(())
    }

    /// Persist safe custom header names with empty values.
    ///
    /// Header values can carry credentials, so only the names survive a store
    /// round trip. A fresh engine cannot replay the original values.
    pub fn record_header_names(&self, id: &str, names: &[String]) -> Result<(), EngineError> {
        let entries: Vec<serde_json::Value> = names
            .iter()
            .map(|name| serde_json::json!({ "name": name, "value": "" }))
            .collect();
        let encoded = serde_json::to_string(&entries).unwrap_or_else(|_| String::from("[]"));

        let changed = self.conn.execute(
            "UPDATE tasks SET headers_json = ?1, updated_at_ms = ?2 WHERE id = ?3",
            params![encoded, now_ms(), id],
        )?;
        if changed == 0 {
            return Err(task_not_found());
        }
        Ok(())
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
             completed_at_ms = ?2, error_code = NULL, error_message = NULL, updated_at_ms = ?2 \
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

    /// Record the redacted final URL discovered after redirects.
    pub fn record_final_url(&self, id: &str, url: &str) -> Result<(), EngineError> {
        let changed = self.conn.execute(
            "UPDATE tasks SET final_url = ?1, updated_at_ms = ?2 WHERE id = ?3",
            params![url, now_ms(), id],
        )?;
        if changed == 0 {
            return Err(task_not_found());
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

fn part_exists(destination: &str, part_path: Option<&str>, filename: &str) -> bool {
    let name = part_path
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| PartFile::part_name(filename));
    CapDir::open(std::path::Path::new(destination))
        .map(|cap| cap.exists(&name))
        .unwrap_or(false)
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
        connections: read_u32(row, 15, "connections")?,
        destination: row.get(16)?,
        completed_at_ms: row.get(17)?,
        final_url: row.get(18)?,
    })
}

fn read_u32(row: &Row<'_>, index: usize, name: &str) -> rusqlite::Result<u32> {
    let raw: i64 = row.get(index)?;
    u32::try_from(raw).map_err(|_| {
        rusqlite::Error::InvalidColumnType(index, name.to_owned(), rusqlite::types::Type::Integer)
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
