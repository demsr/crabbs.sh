use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use russh::server::Handle;
use tokio::sync::Semaphore;

pub use crate::db::Role;
use crate::chat::ChatRoom;
use crate::db::Db;

/// Who a session is acting as. This is BBS-level identity, separate from the
/// SSH login name: a guest can register mid-session and become a `User`
/// without reconnecting.
#[derive(Clone, Debug)]
pub enum Identity {
    Guest,
    User { id: i64, name: String, role: Role },
}

impl Identity {
    /// The account id, or `None` for guests.
    pub fn user_id(&self) -> Option<i64> {
        match self {
            Identity::Guest => None,
            Identity::User { id, .. } => Some(*id),
        }
    }

    /// For display only. Anything that grants power must re-check the
    /// database, since roles can be changed while a session is running.
    pub fn is_sysop(&self) -> bool {
        matches!(self, Identity::User { role: Role::Sysop, .. })
    }

    pub fn display_name(&self) -> &str {
        match self {
            Identity::Guest => "guest",
            Identity::User { name, .. } => name,
        }
    }
}

/// State shared by every connection.
pub struct Shared {
    pub db: Db,
    /// `None` disables the guest account (and, with it, self-registration,
    /// which is only reachable from the guest session).
    pub guest_password: Option<String>,
    /// Bounds concurrent Argon2 runs (each uses ~19 MiB and a CPU core), so a
    /// flood of login attempts can't exhaust memory.
    pub hash_slots: Semaphore,
    pub limiter: Arc<Limiter>,
    pub online: Arc<Online>,
    pub chat: Arc<ChatRoom>,
    /// Announces delivered mail to the recipient's live sessions.
    pub mail: tokio::sync::broadcast::Sender<crate::mail::MailNotice>,
    /// Verified against when a username doesn't exist, so unknown and known
    /// users take equally long to reject.
    pub dummy_hash: String,
}

impl Shared {
    /// Runs blocking work (SQLite, Argon2) off the async runtime.
    pub async fn blocking<T, F>(self: &Arc<Self>, f: F) -> T
    where
        T: Send + 'static,
        F: FnOnce(&Shared) -> T + Send + 'static,
    {
        let shared = Arc::clone(self);
        tokio::task::spawn_blocking(move || f(&shared))
            .await
            .expect("blocking task panicked")
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    AuthFailure,
    Registration,
    /// Any new post, including the first post of a new thread.
    Post,
    NewThread,
    ChatMessage,
    /// A wrong "current password" while changing the password.
    PasswordAttempt,
    MailSend,
}

impl Event {
    fn limit(self) -> (usize, Duration) {
        match self {
            Event::AuthFailure => (10, Duration::from_secs(10 * 60)),
            Event::Registration => (3, Duration::from_secs(60 * 60)),
            Event::Post => (10, Duration::from_secs(10 * 60)),
            Event::NewThread => (3, Duration::from_secs(60 * 60)),
            Event::ChatMessage => (8, Duration::from_secs(10)),
            Event::PasswordAttempt => (5, Duration::from_secs(10 * 60)),
            Event::MailSend => (10, Duration::from_secs(10 * 60)),
        }
    }
}

const MAX_CONNECTIONS_PER_IP: usize = 8;
const PRUNE_THRESHOLD: usize = 10_000;

/// Who a limit applies to: a network address or a logged-in user.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Subject {
    Ip(IpAddr),
    User(i64),
}

impl Subject {
    pub fn ip(ip: IpAddr) -> Self {
        Subject::Ip(bucket(ip))
    }
}

/// In-memory sliding-window limits. IPv6 addresses are grouped by /64, since
/// a single subscriber usually controls a whole /64.
#[derive(Default)]
pub struct Limiter {
    events: Mutex<HashMap<(Subject, Event), Vec<Instant>>>,
    connections: Mutex<HashMap<IpAddr, usize>>,
}

fn bucket(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => {
            let mut octets = v6.octets();
            octets[8..].fill(0);
            IpAddr::V6(octets.into())
        }
        v4 => v4,
    }
}

impl Limiter {
    /// True if `subject` is still under the limit for `event`.
    pub fn allowed(&self, subject: Subject, event: Event) -> bool {
        let (max, window) = event.limit();
        let now = Instant::now();
        let events = self.events.lock().unwrap_or_else(|e| e.into_inner());
        events.get(&(subject, event)).map_or(true, |hits| {
            hits.iter()
                .filter(|t| now.duration_since(**t) < window)
                .count()
                < max
        })
    }

    pub fn record(&self, subject: Subject, event: Event) {
        let (_, window) = event.limit();
        let now = Instant::now();
        let mut events = self.events.lock().unwrap_or_else(|e| e.into_inner());
        events.entry((subject, event)).or_default().push(now);
        if events.len() > PRUNE_THRESHOLD {
            events.retain(|(_, ev), hits| {
                let (_, window) = ev.limit();
                hits.retain(|t| now.duration_since(*t) < window);
                !hits.is_empty()
            });
        } else if let Some(hits) = events.get_mut(&(subject, event)) {
            hits.retain(|t| now.duration_since(*t) < window);
        }
    }

    /// Registers a new connection. Returns `None` if `ip` already has too many.
    pub fn connect(self: &Arc<Self>, ip: IpAddr) -> Option<ConnectionGuard> {
        let ip = bucket(ip);
        let mut conns = self.connections.lock().unwrap_or_else(|e| e.into_inner());
        let count = conns.entry(ip).or_default();
        if *count >= MAX_CONNECTIONS_PER_IP {
            return None;
        }
        *count += 1;
        Some(ConnectionGuard {
            limiter: Arc::clone(self),
            ip,
        })
    }
}

/// Releases the per-address connection slot when the connection ends.
pub struct ConnectionGuard {
    limiter: Arc<Limiter>,
    ip: IpAddr,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        let mut conns = self
            .limiter
            .connections
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(count) = conns.get_mut(&self.ip) {
            *count -= 1;
            if *count == 0 {
                conns.remove(&self.ip);
            }
        }
    }
}

// ---------------------------------------------------------------- online

/// A connected session, as shown on the "Who's online" screen.
pub struct OnlineEntry {
    pub name: String,
    /// `None` for guests.
    pub user_id: Option<i64>,
    pub role: Role,
    pub since: Instant,
    /// What the session is currently doing, e.g. "Reading boards".
    pub activity: &'static str,
    /// Lets the server end the session (e.g. when the user gets banned).
    pub handle: Handle,
}

pub struct OnlineUser {
    pub name: String,
    pub user_id: Option<i64>,
    pub role: Role,
    pub connected: Duration,
    pub activity: &'static str,
}

/// Registry of live sessions. Sessions add themselves with `join` and are
/// removed automatically when the returned guard is dropped.
#[derive(Default)]
pub struct Online {
    next_id: AtomicU64,
    entries: Mutex<HashMap<u64, OnlineEntry>>,
}

impl Online {
    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<u64, OnlineEntry>> {
        self.entries.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn join(self: &Arc<Self>, entry: OnlineEntry) -> OnlineGuard {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.entries().insert(id, entry);
        OnlineGuard {
            online: Arc::clone(self),
            id,
        }
    }

    pub fn snapshot(&self) -> Vec<OnlineUser> {
        let now = Instant::now();
        let mut users: Vec<OnlineUser> = self
            .entries()
            .values()
            .map(|e| OnlineUser {
                name: e.name.clone(),
                user_id: e.user_id,
                role: e.role,
                connected: now.duration_since(e.since),
                activity: e.activity,
            })
            .collect();
        users.sort_by(|a, b| b.connected.cmp(&a.connected));
        users
    }

    /// The sessions of one user, by registry id, with a handle to end each.
    pub fn sessions_of(&self, user_id: i64) -> Vec<(u64, Handle)> {
        self.entries()
            .iter()
            .filter(|(_, e)| e.user_id == Some(user_id))
            .map(|(id, e)| (*id, e.handle.clone()))
            .collect()
    }

    /// Sessions of logged-in users, with a handle to end each one.
    pub fn user_sessions(&self) -> Vec<(i64, Handle)> {
        self.entries()
            .values()
            .filter_map(|e| e.user_id.map(|id| (id, e.handle.clone())))
            .collect()
    }
}

/// Removes the session from the registry when dropped.
pub struct OnlineGuard {
    online: Arc<Online>,
    id: u64,
}

impl OnlineGuard {
    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn update(&self, f: impl FnOnce(&mut OnlineEntry)) {
        if let Some(entry) = self.online.entries().get_mut(&self.id) {
            f(entry);
        }
    }
}

impl Drop for OnlineGuard {
    fn drop(&mut self) {
        self.online.entries().remove(&self.id);
    }
}
