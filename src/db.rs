use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, ErrorCode, OptionalExtension, params};

pub const MAX_KEYS_PER_USER: usize = 10;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS users (
    id            INTEGER PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,
    created_at    INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE TABLE IF NOT EXISTS ssh_keys (
    id          INTEGER PRIMARY KEY,
    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    fingerprint TEXT NOT NULL UNIQUE,
    comment     TEXT NOT NULL,
    created_at  INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX IF NOT EXISTS ssh_keys_user ON ssh_keys(user_id);
";

/// A single SQLite connection behind a mutex. Queries here are all tiny, so
/// serialising them is fine; callers on the async side should still go through
/// `Shared::blocking` so they never stall the runtime.
pub struct Db {
    conn: Mutex<Connection>,
}

pub struct UserRecord {
    pub id: i64,
    /// Username with the capitalisation it was registered with.
    pub username: String,
    pub password_hash: String,
}

pub struct KeyRecord {
    pub id: i64,
    pub fingerprint: String,
    pub comment: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum DbError {
    /// A UNIQUE constraint was violated (username or key fingerprint).
    Duplicate,
    LimitReached,
    Other,
}

impl From<rusqlite::Error> for DbError {
    fn from(err: rusqlite::Error) -> Self {
        match err.sqlite_error_code() {
            Some(ErrorCode::ConstraintViolation) => DbError::Duplicate,
            _ => {
                eprintln!("database error: {err}");
                DbError::Other
            }
        }
    }
}

impl Db {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn conn(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock can't leave the connection in a
        // state SQLite itself wouldn't roll back, so ignore poisoning.
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn create_user(&self, username: &str, password_hash: &str) -> Result<i64, DbError> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO users (username, password_hash) VALUES (?1, ?2)",
            params![username, password_hash],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn find_user(&self, username: &str) -> Result<Option<UserRecord>, DbError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, username, password_hash FROM users WHERE username = ?1",
                params![username],
                |row| {
                    Ok(UserRecord {
                        id: row.get(0)?,
                        username: row.get(1)?,
                        password_hash: row.get(2)?,
                    })
                },
            )
            .optional()?)
    }

    /// Looks up the user that owns `fingerprint`, but only if that user's
    /// name matches `username` (the name the client is logging in as).
    pub fn find_user_by_key(
        &self,
        username: &str,
        fingerprint: &str,
    ) -> Result<Option<(i64, String)>, DbError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT u.id, u.username FROM ssh_keys k JOIN users u ON u.id = k.user_id
                 WHERE k.fingerprint = ?1 AND u.username = ?2",
                params![fingerprint, username],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    pub fn list_keys(&self, user_id: i64) -> Result<Vec<KeyRecord>, DbError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT id, fingerprint, comment FROM ssh_keys WHERE user_id = ?1 ORDER BY id",
        )?;
        let rows = stmt.query_map(params![user_id], |row| {
            Ok(KeyRecord {
                id: row.get(0)?,
                fingerprint: row.get(1)?,
                comment: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn add_key(&self, user_id: i64, fingerprint: &str, comment: &str) -> Result<(), DbError> {
        let conn = self.conn();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM ssh_keys WHERE user_id = ?1",
            params![user_id],
            |row| row.get(0),
        )?;
        if count >= MAX_KEYS_PER_USER as i64 {
            return Err(DbError::LimitReached);
        }
        conn.execute(
            "INSERT INTO ssh_keys (user_id, fingerprint, comment) VALUES (?1, ?2, ?3)",
            params![user_id, fingerprint, comment],
        )?;
        Ok(())
    }

    /// Only deletes if the key belongs to `user_id`.
    pub fn delete_key(&self, user_id: i64, key_id: i64) -> Result<(), DbError> {
        self.conn().execute(
            "DELETE FROM ssh_keys WHERE id = ?1 AND user_id = ?2",
            params![key_id, user_id],
        )?;
        Ok(())
    }
}
