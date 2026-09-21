use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params};

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

CREATE TABLE IF NOT EXISTS boards (
    id          INTEGER PRIMARY KEY,
    name        TEXT NOT NULL UNIQUE COLLATE NOCASE,
    description TEXT NOT NULL,
    position    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS threads (
    id           INTEGER PRIMARY KEY,
    board_id     INTEGER NOT NULL REFERENCES boards(id),
    author_id    INTEGER NOT NULL REFERENCES users(id),
    title        TEXT NOT NULL,
    post_count   INTEGER NOT NULL DEFAULT 0,
    created_at   INTEGER NOT NULL DEFAULT (unixepoch()),
    last_post_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX IF NOT EXISTS threads_board ON threads(board_id, last_post_at DESC);

CREATE TABLE IF NOT EXISTS posts (
    id         INTEGER PRIMARY KEY,
    thread_id  INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    author_id  INTEGER NOT NULL REFERENCES users(id),
    body       TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (unixepoch())
);

CREATE INDEX IF NOT EXISTS posts_thread ON posts(thread_id, id);
";

/// Created on first start, when there are no boards at all.
const DEFAULT_BOARDS: &[(&str, &str)] = &[
    ("General", "Anything goes (within reason)"),
    ("Tech", "Programming, hardware, retro computing"),
    ("Off-Topic", "Everything else"),
];

pub const MAX_THREADS_LISTED: usize = 100;
pub const MAX_POSTS_PER_THREAD: usize = 200;
const TIME_FORMAT: &str = "%Y-%m-%d %H:%M";

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

pub struct BoardRecord {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub thread_count: i64,
}

pub struct ThreadRecord {
    pub id: i64,
    pub title: String,
    pub author: String,
    pub post_count: i64,
    /// UTC, already formatted for display.
    pub last_post: String,
}

pub struct ThreadHead {
    pub id: i64,
    pub board_id: i64,
    pub board_name: String,
    pub title: String,
}

pub struct PostRecord {
    pub author: String,
    pub body: String,
    pub created: String,
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
    /// A referenced row (board, thread, user) doesn't exist.
    NotFound,
    Other,
}

impl From<rusqlite::Error> for DbError {
    fn from(err: rusqlite::Error) -> Self {
        use rusqlite::ffi;
        match err.sqlite_error().map(|e| e.extended_code) {
            Some(ffi::SQLITE_CONSTRAINT_UNIQUE | ffi::SQLITE_CONSTRAINT_PRIMARYKEY) => {
                DbError::Duplicate
            }
            Some(ffi::SQLITE_CONSTRAINT_FOREIGNKEY) => DbError::NotFound,
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
        let boards: i64 = conn.query_row("SELECT COUNT(*) FROM boards", [], |r| r.get(0))?;
        if boards == 0 {
            for (position, (name, description)) in DEFAULT_BOARDS.iter().enumerate() {
                conn.execute(
                    "INSERT INTO boards (name, description, position) VALUES (?1, ?2, ?3)",
                    params![name, description, position as i64],
                )?;
            }
        }
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

    pub fn list_boards(&self) -> Result<Vec<BoardRecord>, DbError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT b.id, b.name, b.description,
                    (SELECT COUNT(*) FROM threads t WHERE t.board_id = b.id)
             FROM boards b ORDER BY b.position, b.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(BoardRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                thread_count: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn board_name(&self, board_id: i64) -> Result<Option<String>, DbError> {
        Ok(self
            .conn()
            .query_row("SELECT name FROM boards WHERE id = ?1", params![board_id], |r| {
                r.get(0)
            })
            .optional()?)
    }

    /// Most recently active threads first, at most `MAX_THREADS_LISTED`.
    pub fn list_threads(&self, board_id: i64) -> Result<Vec<ThreadRecord>, DbError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT t.id, t.title, u.username, t.post_count,
                    strftime('{TIME_FORMAT}', t.last_post_at, 'unixepoch')
             FROM threads t JOIN users u ON u.id = t.author_id
             WHERE t.board_id = ?1
             ORDER BY t.last_post_at DESC, t.id DESC LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![board_id, MAX_THREADS_LISTED as i64], |row| {
            Ok(ThreadRecord {
                id: row.get(0)?,
                title: row.get(1)?,
                author: row.get(2)?,
                post_count: row.get(3)?,
                last_post: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn thread_head(&self, thread_id: i64) -> Result<Option<ThreadHead>, DbError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT t.id, t.board_id, b.name, t.title
                 FROM threads t JOIN boards b ON b.id = t.board_id WHERE t.id = ?1",
                params![thread_id],
                |row| {
                    Ok(ThreadHead {
                        id: row.get(0)?,
                        board_id: row.get(1)?,
                        board_name: row.get(2)?,
                        title: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    pub fn list_posts(&self, thread_id: i64) -> Result<Vec<PostRecord>, DbError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT u.username, p.body, strftime('{TIME_FORMAT}', p.created_at, 'unixepoch')
             FROM posts p JOIN users u ON u.id = p.author_id
             WHERE p.thread_id = ?1 ORDER BY p.id LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![thread_id, MAX_POSTS_PER_THREAD as i64], |row| {
            Ok(PostRecord {
                author: row.get(0)?,
                body: row.get(1)?,
                created: row.get(2)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Creates a thread and its first post atomically; returns the thread id.
    pub fn create_thread(
        &self,
        board_id: i64,
        author_id: i64,
        title: &str,
        body: &str,
    ) -> Result<i64, DbError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO threads (board_id, author_id, title, post_count) VALUES (?1, ?2, ?3, 1)",
            params![board_id, author_id, title],
        )?;
        let thread_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO posts (thread_id, author_id, body) VALUES (?1, ?2, ?3)",
            params![thread_id, author_id, body],
        )?;
        tx.commit()?;
        Ok(thread_id)
    }

    /// Appends a post; fails with `LimitReached` once the thread is full and
    /// `NotFound` if the thread doesn't exist.
    pub fn add_post(&self, thread_id: i64, author_id: i64, body: &str) -> Result<(), DbError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let count: Option<i64> = tx
            .query_row(
                "SELECT post_count FROM threads WHERE id = ?1",
                params![thread_id],
                |r| r.get(0),
            )
            .optional()?;
        match count {
            None => return Err(DbError::NotFound),
            Some(n) if n >= MAX_POSTS_PER_THREAD as i64 => return Err(DbError::LimitReached),
            Some(_) => {}
        }
        tx.execute(
            "INSERT INTO posts (thread_id, author_id, body) VALUES (?1, ?2, ?3)",
            params![thread_id, author_id, body],
        )?;
        tx.execute(
            "UPDATE threads SET post_count = post_count + 1, last_post_at = unixepoch()
             WHERE id = ?1",
            params![thread_id],
        )?;
        tx.commit()?;
        Ok(())
    }
}
