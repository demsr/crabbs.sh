use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;

use crate::db::Db;

/// Who a session is acting as. This is BBS-level identity, separate from the
/// SSH login name: a guest can register mid-session and become a `User`
/// without reconnecting.
#[derive(Clone, Debug)]
pub enum Identity {
    Guest,
    User { id: i64, name: String },
}

impl Identity {
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
    pub guest_password: String,
    /// Bounds concurrent Argon2 runs (each uses ~19 MiB and a CPU core), so a
    /// flood of login attempts can't exhaust memory.
    pub hash_slots: Semaphore,
    pub limiter: Arc<Limiter>,
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
}

impl Event {
    fn limit(self) -> (usize, Duration) {
        match self {
            Event::AuthFailure => (10, Duration::from_secs(10 * 60)),
            Event::Registration => (3, Duration::from_secs(60 * 60)),
            Event::Post => (10, Duration::from_secs(10 * 60)),
            Event::NewThread => (3, Duration::from_secs(60 * 60)),
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
            hits.iter().filter(|t| now.duration_since(**t) < window).count() < max
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
