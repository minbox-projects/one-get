//! Durable component installation records.
//!
//! `components.db` stores one row per installed component version and marks at
//! most one row per component as active. A partial unique index enforces that
//! invariant structurally, and every activation deactivates the previous row in
//! the same `BEGIN IMMEDIATE` transaction as the insert. A database written by a
//! newer schema is refused; the file and its WAL sidecars are owner-only on
//! Unix.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use super::error::KernelError;

/// The only supported `components.db` schema version.
pub const COMPONENTS_SCHEMA_VERSION: u32 = 1;

const SCHEMA_VERSION_KEY: &str = "schema_version";

const CREATE_SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS schema_meta (
        key TEXT PRIMARY KEY NOT NULL,
        value TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS component_installs (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        component TEXT NOT NULL,
        version TEXT NOT NULL,
        sequence INTEGER NOT NULL,
        target TEXT NOT NULL,
        version_dir TEXT NOT NULL,
        files_json TEXT NOT NULL,
        installed_at_ms INTEGER NOT NULL,
        active INTEGER NOT NULL DEFAULT 0 CHECK (active IN (0, 1))
    );
    CREATE UNIQUE INDEX IF NOT EXISTS component_installs_component_sequence
        ON component_installs (component, sequence);
    CREATE UNIQUE INDEX IF NOT EXISTS component_installs_one_active
        ON component_installs (component) WHERE active = 1;
";

/// One installed file belonging to a component version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledFile {
    /// Role the file fills, for example `ffmpeg` or `ffprobe`.
    pub role: String,
    /// File name inside the version directory.
    pub name: String,
    /// Lowercase hexadecimal SHA-256 of the installed bytes.
    pub sha256: String,
    /// Installed size in bytes.
    pub size: u64,
}

/// One recorded component installation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentInstall {
    /// Component name: `yt-dlp`, `ffmpeg`, or `deno`.
    pub component: String,
    /// Human-readable version label.
    pub version: String,
    /// Monotonically increasing update sequence.
    pub sequence: u64,
    /// Target key the version was installed for.
    pub target: String,
    /// Version directory relative to the component root.
    pub version_dir: String,
    /// Installed files with role, name, hash, and size.
    pub files: Vec<InstalledFile>,
    /// Installation time in epoch milliseconds.
    pub installed_at_ms: i64,
}

/// A component installation store backed by one SQLite connection.
pub struct ComponentStore {
    conn: Connection,
}

impl ComponentStore {
    /// Open or create `components.db`, applying schema version 1.
    ///
    /// A database written by a newer schema returns
    /// `COMPONENT_SCHEMA_UNSUPPORTED` before any table or row is written.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, KernelError> {
        let path = path.as_ref();
        let conn = Connection::open(path).map_err(store_failed)?;
        configure(&conn)?;
        restrict_permissions(path)?;

        let stored = read_schema_version(&conn)?;
        if stored > COMPONENTS_SCHEMA_VERSION {
            return Err(KernelError::new(
                "COMPONENT_SCHEMA_UNSUPPORTED",
                "components.db was written by a newer schema version",
            ));
        }

        if stored == 0 {
            initialize(&conn)?;
        } else {
            conn.execute_batch(CREATE_SCHEMA).map_err(store_failed)?;
        }
        restrict_permissions(path)?;

        Ok(Self { conn })
    }

    /// Read the live schema version.
    pub fn schema_version(&self) -> Result<u32, KernelError> {
        read_schema_version(&self.conn)
    }

    /// Record `install` as the one active version for its component.
    ///
    /// The previous active row and the new active row change in one immediate
    /// transaction so a reader never observes two active versions.
    pub fn record_active(&self, install: &ComponentInstall) -> Result<(), KernelError> {
        let sequence = to_i64(install.sequence)?;
        let files_json = serde_json::to_string(&install.files)
            .map_err(|_| store_corrupt("component file record could not be encoded"))?;

        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(store_failed)?;
        let result = self.write_active(install, sequence, &files_json);
        match result {
            Ok(()) => self
                .conn
                .execute_batch("COMMIT")
                .map_err(store_failed)
                .map(|_| ()),
            Err(error) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    fn write_active(
        &self,
        install: &ComponentInstall,
        sequence: i64,
        files_json: &str,
    ) -> Result<(), KernelError> {
        self.conn
            .execute(
                "UPDATE component_installs SET active = 0 WHERE component = ?1 AND active = 1",
                params![install.component],
            )
            .map_err(store_failed)?;
        self.conn
            .execute(
                "INSERT INTO component_installs (
                    component, version, sequence, target, version_dir,
                    files_json, installed_at_ms, active
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1)
                ON CONFLICT(component, sequence) DO UPDATE SET
                    version = excluded.version,
                    target = excluded.target,
                    version_dir = excluded.version_dir,
                    files_json = excluded.files_json,
                    installed_at_ms = excluded.installed_at_ms,
                    active = 1",
                params![
                    install.component,
                    install.version,
                    sequence,
                    install.target,
                    install.version_dir,
                    files_json,
                    install.installed_at_ms,
                ],
            )
            .map_err(store_failed)?;
        Ok(())
    }

    /// Return the active record for `component`, when one exists.
    pub fn active(&self, component: &str) -> Result<Option<ComponentInstall>, KernelError> {
        self.conn
            .query_row(
                "SELECT component, version, sequence, target, version_dir, files_json, installed_at_ms
                 FROM component_installs
                 WHERE component = ?1 AND active = 1
                 LIMIT 1",
                params![component],
                row_to_install,
            )
            .optional()
            .map_err(store_failed)?
            .transpose()
    }

    /// List every recorded version for `component`, newest sequence first.
    pub fn installed(&self, component: &str) -> Result<Vec<ComponentInstall>, KernelError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT component, version, sequence, target, version_dir, files_json, installed_at_ms
                 FROM component_installs
                 WHERE component = ?1
                 ORDER BY sequence DESC",
            )
            .map_err(store_failed)?;
        let rows = statement
            .query_map(params![component], row_to_install)
            .map_err(store_failed)?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(store_failed)??);
        }
        Ok(records)
    }
}

fn configure(conn: &Connection) -> Result<(), KernelError> {
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(store_failed)?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(store_failed)?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(store_failed)?;
    Ok(())
}

fn initialize(conn: &Connection) -> Result<(), KernelError> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .map_err(store_failed)?;
    let result = conn
        .execute_batch(CREATE_SCHEMA)
        .map_err(store_failed)
        .and_then(|_| {
            conn.execute(
                "INSERT OR REPLACE INTO schema_meta (key, value) VALUES (?1, ?2)",
                params![SCHEMA_VERSION_KEY, COMPONENTS_SCHEMA_VERSION.to_string()],
            )
            .map_err(store_failed)
            .map(|_| ())
        });
    match result {
        Ok(()) => conn
            .execute_batch("COMMIT")
            .map_err(store_failed)
            .map(|_| ()),
        Err(error) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(error)
        }
    }
}

fn read_schema_version(conn: &Connection) -> Result<u32, KernelError> {
    let exists = conn
        .query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'schema_meta'
             )",
            [],
            |row| row.get::<_, i64>(0),
        )
        .map_err(store_failed)?;
    if exists == 0 {
        return Ok(0);
    }

    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM schema_meta WHERE key = ?1",
            params![SCHEMA_VERSION_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(store_failed)?;
    let Some(value) = value else {
        return Ok(0);
    };
    value.parse::<u32>().map_err(|_| {
        KernelError::new(
            "COMPONENT_SCHEMA_UNSUPPORTED",
            "components.db declares an unreadable schema version",
        )
    })
}

fn row_to_install(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<Result<ComponentInstall, KernelError>> {
    let component: String = row.get(0)?;
    let version: String = row.get(1)?;
    let sequence: i64 = row.get(2)?;
    let target: String = row.get(3)?;
    let version_dir: String = row.get(4)?;
    let files_json: String = row.get(5)?;
    let installed_at_ms: i64 = row.get(6)?;

    let sequence = match u64::try_from(sequence) {
        Ok(value) => value,
        Err(_) => {
            return Ok(Err(store_corrupt("component sequence is out of range")));
        }
    };
    let files = match serde_json::from_str::<Vec<InstalledFile>>(&files_json) {
        Ok(files) => files,
        Err(_) => {
            return Ok(Err(store_corrupt("component file record is unreadable")));
        }
    };

    Ok(Ok(ComponentInstall {
        component,
        version,
        sequence,
        target,
        version_dir,
        files,
        installed_at_ms,
    }))
}

fn to_i64(value: u64) -> Result<i64, KernelError> {
    i64::try_from(value).map_err(|_| store_corrupt("component value is out of range"))
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<(), KernelError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(store_io_failed)?;
    for suffix in ["-wal", "-shm"] {
        let mut sidecar = OsString::from(path.as_os_str());
        sidecar.push(suffix);
        let sidecar = PathBuf::from(sidecar);
        if sidecar.exists() {
            std::fs::set_permissions(sidecar, std::fs::Permissions::from_mode(0o600))
                .map_err(store_io_failed)?;
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<(), KernelError> {
    Ok(())
}

fn store_failed(error: rusqlite::Error) -> KernelError {
    KernelError::new(
        "COMPONENT_STORE_FAILED",
        format!("the component store operation failed: {error}"),
    )
}

fn store_io_failed(error: std::io::Error) -> KernelError {
    KernelError::new(
        "COMPONENT_STORE_FAILED",
        format!("the component store file could not be protected: {error}"),
    )
}

fn store_corrupt(message: &str) -> KernelError {
    KernelError::new("COMPONENT_STORE_CORRUPT", message)
}
