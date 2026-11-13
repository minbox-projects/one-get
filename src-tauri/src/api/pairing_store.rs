//! Dedicated SQLite store for browser-extension pairing state.
//!
//! Only SHA-256 hashes of pairing codes and bearer tokens are persisted. Client
//! rows bind a token hash to the exact extension origin that paired, while a
//! single pending-code row enforces one-time codes and supersession.

use std::path::Path;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};

use super::error::{self, ApiError};
use super::pairing::{
    generate_pairing_code, generate_token, hash_secret, verify_secret_hash, CODE_TTL_MS,
    LAST_USED_UPDATE_INTERVAL_MS, LOCKOUT_MS, MAX_FAILED_ATTEMPTS,
};

const SCHEMA_VERSION: i64 = 1;

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS clients (
    id TEXT PRIMARY KEY NOT NULL,
    token_hash TEXT NOT NULL UNIQUE,
    origin TEXT NOT NULL,
    label TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    last_used_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS pending_code (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    code_hash TEXT NOT NULL,
    expires_at_ms INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS pairing_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    failed_attempts INTEGER NOT NULL DEFAULT 0,
    locked_until_ms INTEGER NOT NULL DEFAULT 0
);
";

const DEFAULT_LABEL: &str = "Chrome extension";
const MAX_LABEL_CHARS: usize = 64;

/// A pending one-time pairing code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingCode {
    /// Eight-digit code shown to the user exactly once.
    pub code: String,
    /// Epoch-millisecond expiry of the code.
    pub expires_at_ms: i64,
}

/// A paired browser-extension client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedClient {
    /// Stable client identifier used for revocation.
    pub id: String,
    /// Exact Chromium extension origin bound to the token.
    pub origin: String,
    /// User-visible label.
    pub label: String,
    /// Epoch-millisecond creation time.
    pub created_at_ms: i64,
    /// Epoch-millisecond last successful authorization time.
    pub last_used_at_ms: i64,
}

/// Outcome of a pairing attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairResult {
    /// The code was correct; the token is returned exactly once.
    Paired {
        /// Bearer token to deliver to the extension.
        token: String,
        /// Newly stored client.
        client: PairedClient,
    },
    /// The submitted code did not match the pending code.
    InvalidCode,
    /// The pending code has expired.
    Expired,
    /// Pairing is locked until the advertised delay elapses.
    Locked {
        /// Whole seconds the caller should wait before retrying.
        retry_after_secs: u64,
    },
}

/// A dedicated pairing database.
pub struct PairingStore {
    conn: Connection,
}

impl PairingStore {
    /// Open or create the pairing database.
    ///
    /// A database written by a newer schema, or one that cannot be read, is
    /// refused with a typed 503 error.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, ApiError> {
        let path = path.as_ref();
        let conn = Connection::open(path).map_err(|_| database_unavailable())?;
        conn.busy_timeout(Duration::from_secs(5))
            .map_err(|_| database_unavailable())?;

        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(|_| database_unavailable())?;
        if version > SCHEMA_VERSION {
            return Err(schema_unsupported());
        }

        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(|_| database_unavailable())?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(|_| database_unavailable())?;

        if version < SCHEMA_VERSION {
            conn.execute_batch(SCHEMA_SQL)
                .map_err(|_| database_unavailable())?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)
                .map_err(|_| database_unavailable())?;
        }

        conn.execute(
            "INSERT OR IGNORE INTO pairing_state (id, failed_attempts, locked_until_ms)
             VALUES (1, 0, 0)",
            [],
        )
        .map_err(|_| database_unavailable())?;

        restrict_permissions(path).map_err(|_| database_unavailable())?;

        Ok(Self { conn })
    }

    /// Generate a new pending code, replacing any earlier pending code.
    pub fn generate_code(&mut self, now_ms: i64) -> Result<PairingCode, ApiError> {
        let code = generate_pairing_code();
        let expires_at_ms = now_ms.saturating_add(CODE_TTL_MS);
        let code_hash = hash_secret(&code);

        let transaction = self.conn.transaction().map_err(|_| operation_failed())?;
        transaction
            .execute("DELETE FROM pending_code", [])
            .map_err(|_| operation_failed())?;
        transaction
            .execute(
                "INSERT INTO pending_code (id, code_hash, expires_at_ms, created_at_ms)
                 VALUES (1, ?1, ?2, ?3)",
                params![code_hash, expires_at_ms, now_ms],
            )
            .map_err(|_| operation_failed())?;
        transaction.commit().map_err(|_| operation_failed())?;

        Ok(PairingCode {
            code,
            expires_at_ms,
        })
    }

    /// Submit a pairing code for an exact extension origin.
    pub fn pair(
        &mut self,
        code: &str,
        origin: &str,
        label: &str,
        now_ms: i64,
    ) -> Result<PairResult, ApiError> {
        let (failed_attempts, locked_until_ms) = self.read_state()?;
        if locked_until_ms > now_ms {
            return Ok(PairResult::Locked {
                retry_after_secs: remaining_secs(locked_until_ms - now_ms),
            });
        }
        if locked_until_ms != 0 {
            self.write_state(0, 0)?;
        }

        let pending: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT code_hash, expires_at_ms FROM pending_code WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|_| operation_failed())?;
        let Some((code_hash, expires_at_ms)) = pending else {
            return Ok(PairResult::InvalidCode);
        };
        if now_ms >= expires_at_ms {
            return Ok(PairResult::Expired);
        }

        if !verify_secret_hash(code, &code_hash) {
            let attempts = failed_attempts.saturating_add(1);
            if attempts >= i64::from(MAX_FAILED_ATTEMPTS) {
                let locked_until_ms = now_ms.saturating_add(LOCKOUT_MS);
                self.write_state(attempts, locked_until_ms)?;
                return Ok(PairResult::Locked {
                    retry_after_secs: remaining_secs(LOCKOUT_MS),
                });
            }
            self.write_state(attempts, 0)?;
            return Ok(PairResult::InvalidCode);
        }

        let token = generate_token();
        let token_hash = hash_secret(&token);
        let id = uuid::Uuid::new_v4().to_string();
        let label = sanitize_label(label);

        let transaction = self.conn.transaction().map_err(|_| operation_failed())?;
        transaction
            .execute("DELETE FROM pending_code WHERE id = 1", [])
            .map_err(|_| operation_failed())?;
        transaction
            .execute(
                "INSERT INTO clients
                 (id, token_hash, origin, label, created_at_ms, last_used_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![&id, &token_hash, origin, &label, now_ms],
            )
            .map_err(|_| operation_failed())?;
        transaction
            .execute(
                "UPDATE pairing_state
                 SET failed_attempts = 0, locked_until_ms = 0
                 WHERE id = 1",
                [],
            )
            .map_err(|_| operation_failed())?;
        transaction.commit().map_err(|_| operation_failed())?;

        Ok(PairResult::Paired {
            token,
            client: PairedClient {
                id,
                origin: origin.to_string(),
                label,
                created_at_ms: now_ms,
                last_used_at_ms: now_ms,
            },
        })
    }

    /// List paired clients ordered by creation time.
    pub fn list_clients(&self) -> Result<Vec<PairedClient>, ApiError> {
        let mut statement = self
            .conn
            .prepare(
                "SELECT id, origin, label, created_at_ms, last_used_at_ms
                 FROM clients ORDER BY created_at_ms, id",
            )
            .map_err(|_| operation_failed())?;
        let rows = statement
            .query_map([], |row| {
                Ok(PairedClient {
                    id: row.get(0)?,
                    origin: row.get(1)?,
                    label: row.get(2)?,
                    created_at_ms: row.get(3)?,
                    last_used_at_ms: row.get(4)?,
                })
            })
            .map_err(|_| operation_failed())?;

        let mut clients = Vec::new();
        for row in rows {
            clients.push(row.map_err(|_| operation_failed())?);
        }
        Ok(clients)
    }

    /// Authorize a token for an exact origin, updating last-used at most once
    /// per [`LAST_USED_UPDATE_INTERVAL_MS`].
    pub fn authorize(
        &mut self,
        token: &str,
        origin: &str,
        now_ms: i64,
    ) -> Result<Option<PairedClient>, ApiError> {
        let token_hash = hash_secret(token);
        let client: Option<PairedClient> = self
            .conn
            .query_row(
                "SELECT id, origin, label, created_at_ms, last_used_at_ms
                 FROM clients WHERE token_hash = ?1",
                params![token_hash],
                |row| {
                    Ok(PairedClient {
                        id: row.get(0)?,
                        origin: row.get(1)?,
                        label: row.get(2)?,
                        created_at_ms: row.get(3)?,
                        last_used_at_ms: row.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(|_| operation_failed())?;

        let Some(mut client) = client else {
            return Ok(None);
        };
        if client.origin != origin {
            return Ok(None);
        }
        if now_ms.saturating_sub(client.last_used_at_ms) >= LAST_USED_UPDATE_INTERVAL_MS {
            self.conn
                .execute(
                    "UPDATE clients SET last_used_at_ms = ?1 WHERE id = ?2",
                    params![now_ms, client.id.as_str()],
                )
                .map_err(|_| operation_failed())?;
            client.last_used_at_ms = now_ms;
        }
        Ok(Some(client))
    }

    /// Revoke a client by identifier.
    pub fn revoke(&mut self, id: &str) -> Result<bool, ApiError> {
        let changed = self
            .conn
            .execute("DELETE FROM clients WHERE id = ?1", params![id])
            .map_err(|_| operation_failed())?;
        Ok(changed > 0)
    }

    /// Revoke a client by its bearer token.
    pub fn revoke_token(&mut self, token: &str) -> Result<bool, ApiError> {
        let token_hash = hash_secret(token);
        let changed = self
            .conn
            .execute(
                "DELETE FROM clients WHERE token_hash = ?1",
                params![token_hash],
            )
            .map_err(|_| operation_failed())?;
        Ok(changed > 0)
    }

    fn read_state(&self) -> Result<(i64, i64), ApiError> {
        self.conn
            .query_row(
                "SELECT failed_attempts, locked_until_ms
                 FROM pairing_state WHERE id = 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|_| operation_failed())
    }

    fn write_state(&mut self, failed_attempts: i64, locked_until_ms: i64) -> Result<(), ApiError> {
        self.conn
            .execute(
                "UPDATE pairing_state
                 SET failed_attempts = ?1, locked_until_ms = ?2
                 WHERE id = 1",
                params![failed_attempts, locked_until_ms],
            )
            .map_err(|_| operation_failed())?;
        Ok(())
    }
}

fn sanitize_label(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .filter(|character| !character.is_control())
        .collect();
    let bounded: String = cleaned.trim().chars().take(MAX_LABEL_CHARS).collect();
    if bounded.is_empty() {
        DEFAULT_LABEL.to_string()
    } else {
        bounded
    }
}

fn remaining_secs(remaining_ms: i64) -> u64 {
    (remaining_ms.max(0) as u64).div_ceil(1_000).max(1)
}

fn database_unavailable() -> ApiError {
    error::service_unavailable("PAIRING_DATABASE_UNAVAILABLE")
}

fn operation_failed() -> ApiError {
    error::service_unavailable("PAIRING_DATABASE_ERROR")
}

fn schema_unsupported() -> ApiError {
    ApiError::new(
        503,
        "PAIRING_SCHEMA_UNSUPPORTED",
        "pairing database schema is newer than this build supports",
    )
}

#[cfg(unix)]
fn restrict_permissions(path: &Path) -> std::io::Result<()> {
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

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
fn restrict_permissions(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
