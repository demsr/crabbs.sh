use std::path::Path;
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params};

pub const MAX_KEYS_PER_USER: usize = 10;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS users (
    id            INTEGER PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,
    created_at    INTEGER NOT NULL DEFAULT (unixepoch()),
    role          TEXT NOT NULL DEFAULT 'user',
    banned        INTEGER NOT NULL DEFAULT 0,
    ban_reason    TEXT
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    User,
    Sysop,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Sysop => "sysop",
        }
    }

    pub fn parse(s: &str) -> Option<Role> {
        match s {
            "user" => Some(Role::User),
            "sysop" => Some(Role::Sysop),
            _ => None,
        }
    }

    /// Anything unrecognised in the database is treated as an ordinary user,
    /// never as a privileged one.
    fn from_db(s: &str) -> Role {
        Role::parse(s).unwrap_or(Role::User)
    }
}

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
    pub role: Role,
    pub banned: bool,
}

/// What the server needs to know about a user to decide whether they may
/// log in or act. Always read fresh, so changes made by the admin tool take
/// effect immediately.
pub struct Account {
    pub id: i64,
    pub username: String,
    pub role: Role,
    pub banned: bool,
}

pub struct UserSummary {
    pub id: i64,
    pub username: String,
    pub role: Role,
    pub banned: bool,
    pub ban_reason: Option<String>,
    pub created: String,
    pub posts: i64,
    pub keys: i64,
}

pub struct Stats {
    pub users: i64,
    pub sysops: i64,
    pub banned: i64,
    pub boards: i64,
    pub threads: i64,
    pub posts: i64,
    pub keys: i64,
}

#[derive(Debug)]
pub struct PostDeletion {
    pub thread_id: i64,
    pub board_id: i64,
    /// The thread had no other posts left, so it was removed too.
    pub thread_deleted: bool,
}

pub struct BoardRecord {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub position: i64,
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
    pub id: i64,
    pub author: String,
    pub author_is_sysop: bool,
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
    /// Refused because the thing still contains data (e.g. a board with threads).
    NotEmpty,
    Other,
}

impl std::fmt::Display for DbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            DbError::Duplicate => "already exists",
            DbError::LimitReached => "limit reached",
            DbError::NotFound => "not found",
            DbError::NotEmpty => "not empty",
            DbError::Other => "database error",
        })
    }
}

impl std::error::Error for DbError {}

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
        Self::init(Connection::open(path)?)
    }

    #[cfg(test)]
    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;
        Self::migrate(&conn)?;
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

    /// Brings databases created by older versions up to date. Idempotent:
    /// columns that already exist (fresh databases) are left alone.
    fn migrate(conn: &Connection) -> rusqlite::Result<()> {
        const USER_COLUMNS: &[(&str, &str)] = &[
            ("role", "ALTER TABLE users ADD COLUMN role TEXT NOT NULL DEFAULT 'user'"),
            ("banned", "ALTER TABLE users ADD COLUMN banned INTEGER NOT NULL DEFAULT 0"),
            ("ban_reason", "ALTER TABLE users ADD COLUMN ban_reason TEXT"),
        ];
        for (column, ddl) in USER_COLUMNS {
            let exists: i64 = conn.query_row(
                "SELECT COUNT(*) FROM pragma_table_info('users') WHERE name = ?1",
                params![column],
                |r| r.get(0),
            )?;
            if exists == 0 {
                conn.execute(ddl, [])?;
            }
        }
        Ok(())
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
                "SELECT id, username, password_hash, role, banned FROM users WHERE username = ?1",
                params![username],
                |row| {
                    Ok(UserRecord {
                        id: row.get(0)?,
                        username: row.get(1)?,
                        password_hash: row.get(2)?,
                        role: Role::from_db(&row.get::<_, String>(3)?),
                        banned: row.get(4)?,
                    })
                },
            )
            .optional()?)
    }

    fn account_from_row(row: &rusqlite::Row) -> rusqlite::Result<Account> {
        Ok(Account {
            id: row.get(0)?,
            username: row.get(1)?,
            role: Role::from_db(&row.get::<_, String>(2)?),
            banned: row.get(3)?,
        })
    }

    pub fn account(&self, user_id: i64) -> Result<Option<Account>, DbError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, username, role, banned FROM users WHERE id = ?1",
                params![user_id],
                Self::account_from_row,
            )
            .optional()?)
    }

    pub fn account_by_name(&self, username: &str) -> Result<Option<Account>, DbError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT id, username, role, banned FROM users WHERE username = ?1",
                params![username],
                Self::account_from_row,
            )
            .optional()?)
    }

    /// Looks up the user that owns `fingerprint`, but only if that user's
    /// name matches `username` (the name the client is logging in as).
    pub fn find_user_by_key(
        &self,
        username: &str,
        fingerprint: &str,
    ) -> Result<Option<Account>, DbError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT u.id, u.username, u.role, u.banned
                 FROM ssh_keys k JOIN users u ON u.id = k.user_id
                 WHERE k.fingerprint = ?1 AND u.username = ?2",
                params![fingerprint, username],
                Self::account_from_row,
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
            "SELECT b.id, b.name, b.description, b.position,
                    (SELECT COUNT(*) FROM threads t WHERE t.board_id = b.id)
             FROM boards b ORDER BY b.position, b.id",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok(BoardRecord {
                id: row.get(0)?,
                name: row.get(1)?,
                description: row.get(2)?,
                position: row.get(3)?,
                thread_count: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn board_name(&self, board_id: i64) -> Result<Option<String>, DbError> {
        Ok(self
            .conn()
            .query_row(
                "SELECT name FROM boards WHERE id = ?1",
                params![board_id],
                |r| r.get(0),
            )
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
            "SELECT p.id, u.username, u.role = 'sysop', p.body,
                    strftime('{TIME_FORMAT}', p.created_at, 'unixepoch')
             FROM posts p JOIN users u ON u.id = p.author_id
             WHERE p.thread_id = ?1 ORDER BY p.id LIMIT ?2"
        ))?;
        let rows = stmt.query_map(params![thread_id, MAX_POSTS_PER_THREAD as i64], |row| {
            Ok(PostRecord {
                id: row.get(0)?,
                author: row.get(1)?,
                author_is_sysop: row.get(2)?,
                body: row.get(3)?,
                created: row.get(4)?,
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

    // ------------------------------------------------ moderation and admin

    /// Deletes a whole thread with its posts. Returns the board it was in.
    pub fn delete_thread(&self, thread_id: i64) -> Result<i64, DbError> {
        let conn = self.conn();
        let board_id: Option<i64> = conn
            .query_row(
                "SELECT board_id FROM threads WHERE id = ?1",
                params![thread_id],
                |r| r.get(0),
            )
            .optional()?;
        let board_id = board_id.ok_or(DbError::NotFound)?;
        conn.execute("DELETE FROM threads WHERE id = ?1", params![thread_id])?;
        Ok(board_id)
    }

    /// Deletes one post. If it was the last post the thread goes too;
    /// otherwise the thread's post count and last-activity time are
    /// recomputed.
    pub fn delete_post(&self, post_id: i64) -> Result<PostDeletion, DbError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let found: Option<(i64, i64)> = tx
            .query_row(
                "SELECT p.thread_id, t.board_id FROM posts p
                 JOIN threads t ON t.id = p.thread_id WHERE p.id = ?1",
                params![post_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (thread_id, board_id) = found.ok_or(DbError::NotFound)?;
        tx.execute("DELETE FROM posts WHERE id = ?1", params![post_id])?;
        let remaining: i64 = tx.query_row(
            "SELECT COUNT(*) FROM posts WHERE thread_id = ?1",
            params![thread_id],
            |r| r.get(0),
        )?;
        let thread_deleted = remaining == 0;
        if thread_deleted {
            tx.execute("DELETE FROM threads WHERE id = ?1", params![thread_id])?;
        } else {
            tx.execute(
                "UPDATE threads SET post_count = ?2,
                     last_post_at = COALESCE(
                         (SELECT MAX(created_at) FROM posts WHERE thread_id = ?1), created_at)
                 WHERE id = ?1",
                params![thread_id, remaining],
            )?;
        }
        tx.commit()?;
        Ok(PostDeletion {
            thread_id,
            board_id,
            thread_deleted,
        })
    }

    pub fn create_board(&self, name: &str, description: &str) -> Result<i64, DbError> {
        let conn = self.conn();
        conn.execute(
            "INSERT INTO boards (name, description, position)
             VALUES (?1, ?2, (SELECT COALESCE(MAX(position), -1) + 1 FROM boards))",
            params![name, description],
        )?;
        Ok(conn.last_insert_rowid())
    }

    pub fn update_board(
        &self,
        board_id: i64,
        name: Option<&str>,
        description: Option<&str>,
        position: Option<i64>,
    ) -> Result<(), DbError> {
        let changed = self.conn().execute(
            "UPDATE boards SET name = COALESCE(?2, name),
                 description = COALESCE(?3, description),
                 position = COALESCE(?4, position)
             WHERE id = ?1",
            params![board_id, name, description, position],
        )?;
        if changed == 0 {
            return Err(DbError::NotFound);
        }
        Ok(())
    }

    /// Deletes a board. A board that still has threads is only deleted with
    /// `force`, which removes those threads and their posts too.
    pub fn delete_board(&self, board_id: i64, force: bool) -> Result<(), DbError> {
        let mut conn = self.conn();
        let tx = conn.transaction()?;
        let exists: i64 = tx.query_row(
            "SELECT COUNT(*) FROM boards WHERE id = ?1",
            params![board_id],
            |r| r.get(0),
        )?;
        if exists == 0 {
            return Err(DbError::NotFound);
        }
        let threads: i64 = tx.query_row(
            "SELECT COUNT(*) FROM threads WHERE board_id = ?1",
            params![board_id],
            |r| r.get(0),
        )?;
        if threads > 0 && !force {
            return Err(DbError::NotEmpty);
        }
        tx.execute("DELETE FROM threads WHERE board_id = ?1", params![board_id])?;
        tx.execute("DELETE FROM boards WHERE id = ?1", params![board_id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_users(&self) -> Result<Vec<UserSummary>, DbError> {
        let conn = self.conn();
        let mut stmt = conn.prepare(&format!(
            "SELECT u.id, u.username, u.role, u.banned, u.ban_reason,
                    strftime('{TIME_FORMAT}', u.created_at, 'unixepoch'),
                    (SELECT COUNT(*) FROM posts p WHERE p.author_id = u.id),
                    (SELECT COUNT(*) FROM ssh_keys k WHERE k.user_id = u.id)
             FROM users u ORDER BY u.id"
        ))?;
        let rows = stmt.query_map([], |row| {
            Ok(UserSummary {
                id: row.get(0)?,
                username: row.get(1)?,
                role: Role::from_db(&row.get::<_, String>(2)?),
                banned: row.get(3)?,
                ban_reason: row.get(4)?,
                created: row.get(5)?,
                posts: row.get(6)?,
                keys: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn update_user(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
    ) -> Result<(), DbError> {
        if self.conn().execute(sql, params)? == 0 {
            return Err(DbError::NotFound);
        }
        Ok(())
    }

    pub fn set_role(&self, user_id: i64, role: Role) -> Result<(), DbError> {
        self.update_user(
            "UPDATE users SET role = ?2 WHERE id = ?1",
            params![user_id, role.as_str()],
        )
    }

    /// `Some(reason)` bans the user, `None` lifts the ban.
    pub fn set_banned(&self, user_id: i64, reason: Option<&str>) -> Result<(), DbError> {
        self.update_user(
            "UPDATE users SET banned = ?2, ban_reason = ?3 WHERE id = ?1",
            params![user_id, reason.is_some(), reason],
        )
    }

    pub fn set_password_hash(&self, user_id: i64, hash: &str) -> Result<(), DbError> {
        self.update_user(
            "UPDATE users SET password_hash = ?2 WHERE id = ?1",
            params![user_id, hash],
        )
    }

    /// Admin variant of `delete_key`: no ownership check.
    pub fn delete_key_by_id(&self, key_id: i64) -> Result<(), DbError> {
        if self
            .conn()
            .execute("DELETE FROM ssh_keys WHERE id = ?1", params![key_id])?
            == 0
        {
            return Err(DbError::NotFound);
        }
        Ok(())
    }

    pub fn stats(&self) -> Result<Stats, DbError> {
        Ok(self.conn().query_row(
            "SELECT (SELECT COUNT(*) FROM users),
                    (SELECT COUNT(*) FROM users WHERE role = 'sysop'),
                    (SELECT COUNT(*) FROM users WHERE banned = 1),
                    (SELECT COUNT(*) FROM boards),
                    (SELECT COUNT(*) FROM threads),
                    (SELECT COUNT(*) FROM posts),
                    (SELECT COUNT(*) FROM ssh_keys)",
            [],
            |r| {
                Ok(Stats {
                    users: r.get(0)?,
                    sysops: r.get(1)?,
                    banned: r.get(2)?,
                    boards: r.get(3)?,
                    threads: r.get(4)?,
                    posts: r.get(5)?,
                    keys: r.get(6)?,
                })
            },
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_user() -> (Db, i64) {
        let db = Db::open_in_memory().unwrap();
        let id = db.create_user("alice", "hash").unwrap();
        (db, id)
    }

    #[test]
    fn roles_and_bans() {
        let (db, id) = db_with_user();
        let a = db.account(id).unwrap().unwrap();
        assert_eq!((a.role, a.banned), (Role::User, false));
        db.set_role(id, Role::Sysop).unwrap();
        db.set_banned(id, Some("spam")).unwrap();
        let a = db.account_by_name("ALICE").unwrap().unwrap();
        assert_eq!((a.role, a.banned), (Role::Sysop, true));
        db.set_banned(id, None).unwrap();
        assert!(!db.account(id).unwrap().unwrap().banned);
        assert_eq!(db.set_role(9999, Role::User), Err(DbError::NotFound));
    }

    #[test]
    fn delete_post_updates_or_removes_thread() {
        let (db, id) = db_with_user();
        let board = db.list_boards().unwrap()[0].id;
        let thread = db.create_thread(board, id, "Title", "first").unwrap();
        db.add_post(thread, id, "second").unwrap();
        let posts = db.list_posts(thread).unwrap();
        assert_eq!(posts.len(), 2);

        let d = db.delete_post(posts[1].id).unwrap();
        assert!(!d.thread_deleted);
        assert_eq!(db.list_threads(board).unwrap()[0].post_count, 1);

        let d = db.delete_post(posts[0].id).unwrap();
        assert!(d.thread_deleted);
        assert!(db.thread_head(thread).unwrap().is_none());
        assert_eq!(db.delete_post(posts[0].id).unwrap_err(), DbError::NotFound);
    }

    #[test]
    fn delete_board_needs_force_when_not_empty() {
        let (db, id) = db_with_user();
        let board = db.create_board("Extra", "x").unwrap();
        db.create_thread(board, id, "Title", "body").unwrap();
        assert_eq!(db.delete_board(board, false), Err(DbError::NotEmpty));
        db.delete_board(board, true).unwrap();
        assert!(db.board_name(board).unwrap().is_none());
        assert_eq!(db.create_board("general", "dup").unwrap_err(), DbError::Duplicate);
    }

    #[test]
    fn migrates_old_user_table() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT NOT NULL UNIQUE COLLATE NOCASE,
             password_hash TEXT NOT NULL, created_at INTEGER NOT NULL DEFAULT (unixepoch()));
             INSERT INTO users (username, password_hash) VALUES ('old', 'h');",
        )
        .unwrap();
        let db = Db::init(conn).unwrap();
        let a = db.account_by_name("old").unwrap().unwrap();
        assert_eq!((a.role, a.banned), (Role::User, false));
    }
}
