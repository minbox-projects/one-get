//! Durable Hugging Face task metadata.
//!
//! Metadata lives in its own SQLite database so a missing, corrupt, or newer
//! file never affects task listing or control. The store is cheaply cloneable:
//! every clone shares one connection behind a mutex.

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension};

use crate::engine::error::EngineError;

/// Current Hugging Face metadata schema version.
pub const HF_SCHEMA_VERSION: i64 = 1;

/// One persisted Hugging Face task record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HfMetadata {
    /// Owning task identifier.
    pub task_id: String,
    /// Repository identifier without the family prefix.
    pub repo_id: String,
    /// File path within the repository.
    pub file_path: String,
    /// Revision, branch, or tag.
    pub revision: String,
    /// Connection cap captured when the task started.
    pub connection_cap: u32,
    /// Connections actually used by the task.
    pub effective_connections: u32,
    /// Metadata refresh count.
    pub refresh_count: u32,
    /// Whether the transfer fell back to a downgraded connection count.
    pub downgraded: bool,
}

/// A shared Hugging Face metadata store.
#[derive(Clone)]
pub struct HfMetadataStore {
    conn: Arc<Mutex<Connection>>,
}

impl HfMetadataStore {
    /// Open or create a metadata database, refusing a newer schema.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, EngineError> {
        let path = path.as_ref();
        let conn = Connection::open(path)?;
        configure(&conn)?;
        restrict_permissions(path)?;

        let version = read_schema_version(&conn)?;
        if version > HF_SCHEMA_VERSION {
            return Err(EngineError::schema_too_new(version));
        }
        if version < HF_SCHEMA_VERSION {
            create_schema(&conn)?;
            restrict_permissions(path)?;
        }

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Insert or replace the record for its task.
    pub fn record(&self, metadata: &HfMetadata) -> Result<(), EngineError> {
        let mut conn = self.lock()?;
        let transaction = conn.transaction()?;
        transaction.execute(
            "INSERT OR REPLACE INTO hf_tasks (
                task_id, repo_id, file_path, revision, connection_cap,
                effective_connections, refresh_count, downgraded
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                metadata.task_id,
                metadata.repo_id,
                metadata.file_path,
                metadata.revision,
                i64::from(metadata.connection_cap),
                i64::from(metadata.effective_connections),
                i64::from(metadata.refresh_count),
                i64::from(metadata.downgraded),
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Load one task's metadata.
    pub fn get(&self, task_id: &str) -> Result<Option<HfMetadata>, EngineError> {
        let conn = self.lock()?;
        conn.query_row(
            "SELECT repo_id, file_path, revision, connection_cap,
                    effective_connections, refresh_count, downgraded
             FROM hf_tasks WHERE task_id = ?1",
            [task_id],
            |row| {
                Ok(HfMetadata {
                    task_id: task_id.to_string(),
                    repo_id: row.get(0)?,
                    file_path: row.get(1)?,
                    revision: row.get(2)?,
                    connection_cap: u32::try_from(row.get::<_, i64>(3)?).unwrap_or(0),
                    effective_connections: u32::try_from(row.get::<_, i64>(4)?).unwrap_or(0),
                    refresh_count: u32::try_from(row.get::<_, i64>(5)?).unwrap_or(0),
                    downgraded: row.get::<_, i64>(6)? != 0,
                })
            },
        )
        .optional()
        .map_err(EngineError::from)
    }

    /// Delete one task's metadata, succeeding when the row is already absent.
    pub fn delete(&self, task_id: &str) -> Result<(), EngineError> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM hf_tasks WHERE task_id = ?1", [task_id])?;
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, EngineError> {
        self.conn
            .lock()
            .map_err(|_| EngineError::database("the Hugging Face metadata store is unavailable"))
    }
}

fn configure(conn: &Connection) -> Result<(), EngineError> {
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    Ok(())
}

fn read_schema_version(conn: &Connection) -> Result<i64, EngineError> {
    let exists: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_meta'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Ok(0);
    }

    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key = 'schema_version'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value
        .as_deref()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(0))
}

fn create_schema(conn: &Connection) -> Result<(), EngineError> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_meta (
            key TEXT PRIMARY KEY NOT NULL,
            value TEXT NOT NULL
         );
         CREATE TABLE IF NOT EXISTS hf_tasks (
            task_id TEXT PRIMARY KEY NOT NULL,
            repo_id TEXT NOT NULL,
            file_path TEXT NOT NULL,
            revision TEXT NOT NULL,
            connection_cap INTEGER NOT NULL,
            effective_connections INTEGER NOT NULL,
            refresh_count INTEGER NOT NULL,
            downgraded INTEGER NOT NULL
         );
         INSERT OR REPLACE INTO schema_meta (key, value)
            VALUES ('schema_version', '1');",
    )?;
    Ok(())
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), EngineError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    for suffix in ["-wal", "-shm"] {
        let sidecar = std::path::PathBuf::from(format!("{}{}", path.display(), suffix));
        if sidecar.exists() {
            std::fs::set_permissions(sidecar, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), EngineError> {
    Ok(())
}
