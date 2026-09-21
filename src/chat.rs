//! The chat room: who is in it, recent history, and a broadcast of events.
//!
//! Every event gets a sequence number. A session that joins receives a
//! snapshot stamped with the latest sequence number and ignores broadcast
//! events at or below it, so nothing is shown twice and nothing is missed
//! between taking the snapshot and listening.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::broadcast;

/// Events kept for people who join later.
const HISTORY_LEN: usize = 100;
/// Events a slow listener may fall behind by before it has to resync.
const CHANNEL_CAPACITY: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChatKind {
    Message {
        from: String,
        is_sysop: bool,
        text: String,
        /// `/me`-style action rather than a spoken line.
        action: bool,
    },
    Join {
        name: String,
        is_sysop: bool,
    },
    Leave {
        name: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChatEvent {
    pub seq: u64,
    /// Unix time in seconds.
    pub at: u64,
    pub kind: ChatKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    pub is_sysop: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ChatSnapshot {
    /// Sequence number of the newest event included.
    pub seq: u64,
    pub history: Vec<ChatEvent>,
    /// One entry per person, sorted by name (several sessions of the same
    /// user count once).
    pub members: Vec<Member>,
}

#[derive(Default)]
struct State {
    seq: u64,
    history: VecDeque<ChatEvent>,
    /// (session id, member) - a user with two sessions appears twice here.
    seats: Vec<(u64, Member)>,
}

pub struct ChatRoom {
    state: Mutex<State>,
    tx: broadcast::Sender<ChatEvent>,
    next_id: AtomicU64,
}

impl Default for ChatRoom {
    fn default() -> Self {
        Self::new()
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl ChatRoom {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            tx: broadcast::channel(CHANNEL_CAPACITY).0,
            next_id: AtomicU64::new(1),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A fresh id to identify one session in the room.
    pub fn new_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ChatEvent> {
        self.tx.subscribe()
    }

    /// Records and broadcasts an event. Called with the state lock held, so
    /// broadcast order always matches sequence order.
    fn emit(&self, state: &mut State, kind: ChatKind) {
        state.seq += 1;
        let event = ChatEvent {
            seq: state.seq,
            at: now(),
            kind,
        };
        state.history.push_back(event.clone());
        while state.history.len() > HISTORY_LEN {
            state.history.pop_front();
        }
        // No receivers just means nobody is listening right now.
        let _ = self.tx.send(event);
    }

    fn snapshot_of(state: &State) -> ChatSnapshot {
        let mut members: Vec<Member> = Vec::new();
        for (_, m) in &state.seats {
            if !members.iter().any(|x| x.name == m.name) {
                members.push(m.clone());
            }
        }
        members.sort_by_key(|m| m.name.to_lowercase());
        ChatSnapshot {
            seq: state.seq,
            history: state.history.iter().cloned().collect(),
            members,
        }
    }

    pub fn snapshot(&self) -> ChatSnapshot {
        Self::snapshot_of(&self.state())
    }

    pub fn is_member(&self, id: u64) -> bool {
        self.state().seats.iter().any(|(seat, _)| *seat == id)
    }

    /// Adds a session to the room and returns the current snapshot. Others
    /// are told only when this is the person's first session.
    pub fn join(&self, id: u64, name: &str, is_sysop: bool) -> ChatSnapshot {
        let mut state = self.state();
        if !state.seats.iter().any(|(seat, _)| *seat == id) {
            let first = !state.seats.iter().any(|(_, m)| m.name == name);
            state.seats.push((
                id,
                Member {
                    name: name.to_string(),
                    is_sysop,
                },
            ));
            if first {
                self.emit(
                    &mut state,
                    ChatKind::Join {
                        name: name.to_string(),
                        is_sysop,
                    },
                );
            }
        }
        Self::snapshot_of(&state)
    }

    /// Removes a session. Safe to call for sessions that never joined.
    pub fn leave(&self, id: u64) {
        let mut state = self.state();
        let Some(pos) = state.seats.iter().position(|(seat, _)| *seat == id) else {
            return;
        };
        let (_, member) = state.seats.remove(pos);
        if !state.seats.iter().any(|(_, m)| m.name == member.name) {
            self.emit(&mut state, ChatKind::Leave { name: member.name });
        }
    }

    pub fn say(&self, from: &str, is_sysop: bool, text: &str, action: bool) {
        let mut state = self.state();
        self.emit(
            &mut state,
            ChatKind::Message {
                from: from.to_string(),
                is_sysop,
                text: text.to_string(),
                action,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(s: &ChatSnapshot) -> Vec<&str> {
        s.members.iter().map(|m| m.name.as_str()).collect()
    }

    #[test]
    fn join_say_leave_are_sequenced_and_in_history() {
        let room = ChatRoom::new();
        let (a, b) = (room.new_id(), room.new_id());
        room.join(a, "alice", false);
        let snap = room.join(b, "bob", true);
        room.say("bob", true, "hi", false);
        room.leave(a);

        let snap2 = room.snapshot();
        assert_eq!(names(&snap), ["alice", "bob"]);
        assert_eq!(names(&snap2), ["bob"]);
        let seqs: Vec<u64> = snap2.history.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, [1, 2, 3, 4]);
        assert!(matches!(snap2.history[3].kind, ChatKind::Leave { ref name } if name == "alice"));
        assert_eq!(snap2.seq, 4);
    }

    #[test]
    fn several_sessions_of_one_user_announce_once() {
        let room = ChatRoom::new();
        let (s1, s2) = (room.new_id(), room.new_id());
        room.join(s1, "alice", false);
        let snap = room.join(s2, "alice", false);
        assert_eq!(names(&snap), ["alice"]);
        assert_eq!(snap.history.len(), 1, "only one join notice");

        room.leave(s1);
        assert_eq!(room.snapshot().history.len(), 1, "still here via the other session");
        room.leave(s2);
        assert_eq!(room.snapshot().history.len(), 2, "now she left");
        room.leave(s2); // harmless
        assert_eq!(room.snapshot().history.len(), 2);
    }

    #[test]
    fn joining_twice_with_same_id_is_idempotent() {
        let room = ChatRoom::new();
        let id = room.new_id();
        room.join(id, "alice", false);
        room.join(id, "alice", false);
        assert_eq!(room.snapshot().members.len(), 1);
        assert_eq!(room.snapshot().history.len(), 1);
        assert!(room.is_member(id));
        room.leave(id);
        assert!(!room.is_member(id));
    }

    #[test]
    fn history_is_capped() {
        let room = ChatRoom::new();
        for i in 0..HISTORY_LEN + 20 {
            room.say("bob", false, &format!("m{i}"), false);
        }
        let snap = room.snapshot();
        assert_eq!(snap.history.len(), HISTORY_LEN);
        assert_eq!(snap.history[0].seq, 21);
        assert_eq!(snap.seq, (HISTORY_LEN + 20) as u64);
    }

    #[tokio::test]
    async fn listeners_receive_events_in_order_and_snapshot_seq_lets_them_dedupe() {
        let room = ChatRoom::new();
        let mut rx = room.subscribe();
        let id = room.new_id();
        let snap = room.join(id, "alice", false);
        room.say("alice", false, "one", false);
        room.say("alice", false, "two", true);

        // The join event arrives too, but is covered by the snapshot.
        let mut received = Vec::new();
        for _ in 0..3 {
            received.push(rx.recv().await.unwrap());
        }
        assert_eq!(received[0].seq, snap.seq);
        let fresh: Vec<&ChatEvent> = received.iter().filter(|e| e.seq > snap.seq).collect();
        assert_eq!(fresh.len(), 2);
        assert!(matches!(&fresh[1].kind, ChatKind::Message { action: true, text, .. } if text == "two"));
    }
}
