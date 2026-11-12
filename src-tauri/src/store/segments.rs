//! Segment plan replacement, progress, and lifecycle transitions.

use rusqlite::{params, OptionalExtension, Row};

use super::tasks::offset_to_sql;
use super::types::{Segment, SegmentInput};
use super::Database;
use crate::engine::error::EngineError;
use crate::engine::state::SegmentState;

impl Database {
    /// Replace every segment for a task in one transaction.
    pub fn replace_segments(
        &self,
        task_id: &str,
        segments: &[SegmentInput],
    ) -> Result<(), EngineError> {
        let mut rows = Vec::with_capacity(segments.len());
        for segment in segments {
            let index = segment.index;
            let start = segment.start;
            let end = segment.end;
            let downloaded = segment.downloaded;
            let state = segment.state;

            if end <= start {
                return Err(EngineError::invalid_segment_range(
                    "a segment must end after it starts",
                ));
            }
            if downloaded > end - start {
                return Err(EngineError::invalid_segment_range(
                    "a segment cannot have more progress than its range",
                ));
            }
            rows.push((index, start, end, downloaded, state));
        }

        let transaction = self.conn.unchecked_transaction()?;
        transaction.execute("DELETE FROM segments WHERE task_id = ?1", params![task_id])?;
        {
            let mut statement = transaction.prepare(
                "INSERT INTO segments \
                 (task_id, segment_index, start_offset, end_offset, downloaded_bytes, state) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            )?;
            for (index, start, end, downloaded, state) in rows {
                statement.execute(params![
                    task_id,
                    i64::from(index),
                    offset_to_sql(start)?,
                    offset_to_sql(end)?,
                    offset_to_sql(downloaded)?,
                    state.as_str(),
                ])?;
            }
        }
        transaction.commit()?;
        Ok(())
    }

    /// Load a task's segments in index order.
    pub fn segments_for_task(&self, task_id: &str) -> Result<Vec<Segment>, EngineError> {
        let mut statement = self.conn.prepare(
            "SELECT task_id, segment_index, start_offset, end_offset, downloaded_bytes, state \
             FROM segments WHERE task_id = ?1 ORDER BY segment_index ASC",
        )?;
        let segments = statement
            .query_map(params![task_id], segment_from_row)?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(segments)
    }

    /// Apply a segment state transition when the stored state matches `from`.
    pub fn transition_segment(
        &self,
        task_id: &str,
        index: u32,
        from: SegmentState,
        to: SegmentState,
    ) -> Result<(), EngineError> {
        let stored: Option<String> = self
            .conn
            .query_row(
                "SELECT state FROM segments WHERE task_id = ?1 AND segment_index = ?2",
                params![task_id, i64::from(index)],
                |row| row.get(0),
            )
            .optional()?;
        let Some(stored) = stored else {
            return Err(EngineError::new(
                "SEGMENT_NOT_FOUND",
                "the requested segment does not exist",
            ));
        };
        if SegmentState::parse(&stored) != Some(from) || !from.can_transition_to(to) {
            return Err(EngineError::invalid_state(
                "the requested segment transition is not permitted",
            ));
        }

        let changed = self.conn.execute(
            "UPDATE segments SET state = ?1 \
             WHERE task_id = ?2 AND segment_index = ?3 AND state = ?4",
            params![to.as_str(), task_id, i64::from(index), from.as_str()],
        )?;
        if changed == 0 {
            return Err(EngineError::invalid_state(
                "the segment state changed before the transition could be applied",
            ));
        }
        Ok(())
    }
}

fn segment_from_row(row: &Row<'_>) -> rusqlite::Result<Segment> {
    let index: i64 = row.get(1)?;
    let index = u32::try_from(index).map_err(|_| {
        rusqlite::Error::InvalidColumnType(
            1,
            "segment_index".to_owned(),
            rusqlite::types::Type::Integer,
        )
    })?;
    let raw_state: String = row.get(5)?;
    let state = SegmentState::parse(&raw_state).ok_or_else(|| {
        rusqlite::Error::InvalidColumnType(5, "state".to_owned(), rusqlite::types::Type::Text)
    })?;

    Ok(Segment {
        task_id: row.get(0)?,
        index,
        start: read_u64(row, 2, "start_offset")?,
        end: read_u64(row, 3, "end_offset")?,
        downloaded_bytes: read_u64(row, 4, "downloaded_bytes")?,
        state,
    })
}

fn read_u64(row: &Row<'_>, index: usize, name: &str) -> rusqlite::Result<u64> {
    let raw: i64 = row.get(index)?;
    u64::try_from(raw).map_err(|_| {
        rusqlite::Error::InvalidColumnType(index, name.to_owned(), rusqlite::types::Type::Integer)
    })
}
