//! SQLite connection setup and database-level metadata.

#[cfg(unix)]
use std::ffi::OsString;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use super::migrations::{self, SCHEMA_VERSION};
use super::types::SchemaMetadata;
use crate::engine::error::EngineError;

/// The durable task store.
#[derive(Debug)]
pub struct Database {
    pub(crate) conn: Connection,
}

impl Database {
    /// Open or create the task database, migrating it when required.
    ///
    /// An existing non-empty database is backed up beside itself before any
    /// migration runs. A database written by a newer schema is refused.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, EngineError> {
        let path = path.as_ref();
        let existed_with_data = std::fs::metadata(path)
            .map(|metadata| metadata.len() > 0)
            .unwrap_or(false);

        let conn = Connection::open(path)?;
        configure(&conn)?;
        restrict_permissions(path)?;

        let stored_version = migrations::read_schema_version(&conn)?;
        if stored_version > SCHEMA_VERSION {
            return Err(EngineError::schema_too_new(stored_version));
        }
        if stored_version < SCHEMA_VERSION {
            if existed_with_data {
                migrations::backup_before_migration(&conn, path)?;
            }
            migrations::migrate(&conn, stored_version)?;
            restrict_permissions(path)?;
        }

        Ok(Self { conn })
    }

    /// Read the live schema version and connection settings.
    pub fn schema_metadata(&self) -> Result<SchemaMetadata, EngineError> {
        let version = migrations::read_schema_version(&self.conn)?;
        let journal_mode: String = self
            .conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        let foreign_keys: i64 = self
            .conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))?;
        let busy_timeout_ms: i64 = self
            .conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))?;

        Ok(SchemaMetadata {
            version,
            journal_mode,
            foreign_keys_enabled: foreign_keys != 0,
            busy_timeout_ms,
            has_required_indexes: migrations::required_indexes_present(&self.conn)?,
            has_required_constraints: migrations::required_constraints_present(&self.conn)?,
        })
    }
}

fn configure(conn: &Connection) -> Result<(), EngineError> {
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(())
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), EngineError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = OsString::from(path.as_os_str());
        sidecar.push(suffix);
        let sidecar = PathBuf::from(sidecar);
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

/// Current wall-clock time in epoch milliseconds.
pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}
