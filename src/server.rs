use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::bail;
use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, CrosstermBackend};
use ratatui::layout::Rect;
use russh::keys::PublicKey;
use russh::server::{Auth, ChannelOpenHandle, Handler, Msg, Server as ServerTrait, Session};
use russh::{Channel, ChannelId, Pty};
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;

use crate::chat::ChatRoom;
use crate::db::{DbError, MAX_KEYS_PER_USER};
use russh::Disconnect;
use crate::state::{
    ConnectionGuard, Event, Identity, OnlineEntry, OnlineGuard, Role, Shared, Subject,
};
use crate::terminal::TerminalHandle;
use crate::ui::{Action, App, KeyInfo, OnlineInfo, Request, Response};
use crate::{auth, boards, content, mail};

type SshTerminal = Terminal<CrosstermBackend<TerminalHandle>>;

/// The screen state of one session. It is shared with the session's chat
/// listener task (which redraws when chat events arrive), so it sits behind a
/// mutex that is only ever held for short synchronous sections - never across
/// an `.await`.
struct Ui {
    terminal: Option<SshTerminal>,
    app: Option<App>,
}

impl Ui {
    fn redraw(&mut self) {
        if let (Some(terminal), Some(app)) = (self.terminal.as_mut(), self.app.as_ref()) {
            let _ = terminal.draw(|frame| app.draw(frame));
        }
    }
}

type SharedUi = Arc<Mutex<Ui>>;

fn lock_ui(ui: &SharedUi) -> MutexGuard<'_, Ui> {
    ui.lock().unwrap_or_else(|e| e.into_inner())
}

/// Takes the session out of the chat room when the connection ends,
/// however it ends.
struct ChatSeat {
    room: Arc<ChatRoom>,
    id: u64,
}

impl Drop for ChatSeat {
    fn drop(&mut self) {
        self.room.leave(self.id);
    }
}

/// Stops a background task when its session ends.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

const GUEST_USER: &str = "guest";
/// Sanity bounds so absurd credentials are rejected before any real work.
const MAX_SSH_USER_LEN: usize = 64;
const MAX_SSH_PASSWORD_LEN: usize = 1024;
const MAX_TERMINAL_DIM: u32 = 500;

const INTERNAL_ERROR: &str = "Internal error, please try again later.";

/// Factory for per-connection handlers; holds the state they all share.
pub struct BbsServer {
    shared: Arc<Shared>,
}

impl BbsServer {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

impl ServerTrait for BbsServer {
    type Handler = BbsHandler;

    fn new_client(&mut self, peer_addr: Option<std::net::SocketAddr>) -> BbsHandler {
        let peer_ip = peer_addr.map(|addr| addr.ip());
        let guard = peer_ip.and_then(|ip| self.shared.limiter.connect(ip));
        let chat_id = self.shared.chat.new_id();
        BbsHandler {
            chat_id,
            _chat_seat: ChatSeat {
                room: Arc::clone(&self.shared.chat),
                id: chat_id,
            },
            _chat_task: None,
            shared: Arc::clone(&self.shared),
            peer_ip,
            over_limit: peer_ip.is_some() && guard.is_none(),
            _guard: guard,
            identity: None,
            online: None,
            output: None,
            ui: Arc::new(Mutex::new(Ui {
                terminal: None,
                app: None,
            })),
        }
    }

    fn handle_session_error(&mut self, error: anyhow::Error) {
        eprintln!("session error: {error:#}");
    }
}

/// One handler exists per SSH connection.
pub struct BbsHandler {
    shared: Arc<Shared>,
    peer_ip: Option<IpAddr>,
    /// Too many simultaneous connections from this address; refuse to auth.
    over_limit: bool,
    /// Releases this address's connection slot when the handler is dropped.
    _guard: Option<ConnectionGuard>,
    /// Set once SSH authentication succeeds; changes when a guest registers.
    identity: Option<Identity>,
    /// Our entry in the who's-online registry (once the session is open).
    online: Option<OnlineGuard>,
    /// Where the UI is drawn: the client's SSH channel.
    output: Option<TerminalHandle>,
    ui: SharedUi,
    /// This session's id in the chat room.
    chat_id: u64,
    _chat_seat: ChatSeat,
    /// Listens to the room and redraws this session on new events.
    _chat_task: Option<AbortOnDrop>,
}

impl BbsHandler {
    fn redraw(&self) {
        lock_ui(&self.ui).redraw();
    }

    /// Starts the task that turns live events (chat-room activity, newly
    /// delivered mail) into redraws. It runs for the whole session; the
    /// screen ignores events that don't concern what it is showing.
    fn spawn_listener(&mut self) {
        let ui = Arc::clone(&self.ui);
        let room = Arc::clone(&self.shared.chat);
        let mut events = room.subscribe();
        let mut mail = self.shared.mail.subscribe();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    event = events.recv() => match event {
                        Ok(event) => {
                            let mut ui = lock_ui(&ui);
                            if ui.app.as_mut().is_some_and(|app| app.on_chat_event(event)) {
                                ui.redraw();
                            }
                        }
                        // Fell too far behind: start over from a fresh snapshot.
                        Err(RecvError::Lagged(_)) => {
                            let snapshot = room.snapshot();
                            let mut ui = lock_ui(&ui);
                            if ui.app.as_mut().is_some_and(|app| app.on_chat_snapshot(snapshot)) {
                                ui.redraw();
                            }
                        }
                        Err(RecvError::Closed) => break,
                    },
                    notice = mail.recv() => match notice {
                        Ok(notice) => {
                            let mut ui = lock_ui(&ui);
                            if ui.app.as_mut().is_some_and(|app| app.on_mail_notice(notice)) {
                                ui.redraw();
                            }
                        }
                        // A missed notice only means the menu count is stale
                        // until the mailbox is next opened.
                        Err(RecvError::Lagged(_)) => {}
                        Err(RecvError::Closed) => break,
                    },
                }
            }
        });
        self._chat_task = Some(AbortOnDrop(task));
    }

    fn auth_throttled(&self) -> bool {
        self.peer_ip.is_some_and(|ip| {
            !self
                .shared
                .limiter
                .allowed(Subject::ip(ip), Event::AuthFailure)
        })
    }

    fn note_auth_failure(&self) {
        if let Some(ip) = self.peer_ip {
            self.shared
                .limiter
                .record(Subject::ip(ip), Event::AuthFailure);
        }
    }

    fn finish_auth(&mut self, identity: Option<Identity>) -> Auth {
        match identity {
            Some(identity) => {
                self.identity = Some(identity);
                Auth::Accept
            }
            None => {
                self.note_auth_failure();
                Auth::reject()
            }
        }
    }

    async fn verify_password_login(&self, user: &str, password: &str) -> Option<Identity> {
        let shared = &self.shared;
        let name = user.to_string();
        let record = shared
            .blocking(move |s| s.db.find_user(&name))
            .await
            .ok()
            .flatten();

        // Hash something even for unknown users so timing doesn't reveal
        // which usernames exist.
        let stored = match &record {
            Some(r) => r.password_hash.clone(),
            None => shared.dummy_hash.clone(),
        };
        let password = password.to_string();
        let _permit = shared.hash_slots.acquire().await.ok()?;
        let ok = shared
            .blocking(move |_| auth::verify_password(&password, &stored))
            .await;

        match (ok, record) {
            (true, Some(r)) if !r.banned => Some(Identity::User {
                id: r.id,
                name: r.username,
                role: r.role,
            }),
            _ => None,
        }
    }

    async fn key_login(&self, user: &str, key: &PublicKey) -> Option<Identity> {
        if user.eq_ignore_ascii_case(GUEST_USER) {
            return None;
        }
        let name = user.to_string();
        let fingerprint = auth::fingerprint(key);
        let found = self
            .shared
            .blocking(move |s| s.db.find_user_by_key(&name, &fingerprint))
            .await
            .ok()
            .flatten()?;
        if found.banned {
            return None;
        }
        Some(Identity::User {
            id: found.id,
            name: found.username,
            role: found.role,
        })
    }

    async fn serve(&mut self, request: Request) -> Response {
        match request {
            Request::Register { username, password } => {
                Response::Registered(self.register(username, password).await)
            }
            Request::ListBoards => boards::list_boards(&self.shared, self.user_id()).await,
            Request::ListThreads { board_id, page } => {
                boards::list_threads(&self.shared, board_id, page, None, self.user_id()).await
            }
            Request::OpenThread { thread_id, target } => {
                boards::open_thread(&self.shared, thread_id, target, None, self.user_id()).await
            }
            Request::MarkBoardRead { board_id, page } => match &self.identity {
                Some(identity) => {
                    boards::mark_board_read(&self.shared, identity, board_id, page).await
                }
                None => Response::Error(INTERNAL_ERROR.into()),
            },
            Request::WhoIsOnline => self.who_is_online(),
            Request::ChangePassword { current, new } => {
                Response::PasswordChanged(self.change_password(current, new).await)
            }
            Request::OpenMailbox { folder } => {
                let Some(identity) = &self.identity else {
                    return Response::Error(INTERNAL_ERROR.into());
                };
                mail::open_mailbox(&self.shared, identity, folder, None).await
            }
            Request::ReadMessage { id } => {
                let Some(identity) = &self.identity else {
                    return Response::Error(INTERNAL_ERROR.into());
                };
                mail::read_message(&self.shared, identity, id).await
            }
            Request::SendMail { to, subject, body } => {
                let Some(identity) = &self.identity else {
                    return Response::Error(INTERNAL_ERROR.into());
                };
                mail::send(&self.shared, identity, to, subject, body).await
            }
            Request::DeleteMessage { id, folder } => {
                let Some(identity) = &self.identity else {
                    return Response::Error(INTERNAL_ERROR.into());
                };
                mail::delete_message(&self.shared, identity, id, folder).await
            }
            Request::ListBlocks => {
                let Some(identity) = &self.identity else {
                    return Response::Error(INTERNAL_ERROR.into());
                };
                mail::list_blocks(&self.shared, identity, None).await
            }
            Request::BlockUser { name } => {
                let Some(identity) = &self.identity else {
                    return Response::Error(INTERNAL_ERROR.into());
                };
                mail::block(&self.shared, identity, name).await
            }
            Request::UnblockUser { name } => {
                let Some(identity) = &self.identity else {
                    return Response::Error(INTERNAL_ERROR.into());
                };
                mail::unblock(&self.shared, identity, name).await
            }
            Request::JoinChat => self.join_chat().await,
            Request::LeaveChat => {
                self.shared.chat.leave(self.chat_id);
                Response::Nothing
            }
            Request::ChatSay { text, action } => self.chat_say(text, action).await,
            Request::DeleteThread { thread_id, page } => match &self.identity {
                Some(identity) => {
                    boards::delete_thread(&self.shared, identity, thread_id, page).await
                }
                None => Response::Error(INTERNAL_ERROR.into()),
            },
            Request::DeletePost { post_id, page } => match &self.identity {
                Some(identity) => boards::delete_post(&self.shared, identity, post_id, page).await,
                None => Response::Error(INTERNAL_ERROR.into()),
            },
            Request::CreateThread {
                board_id,
                title,
                body,
            } => match &self.identity {
                Some(identity) => {
                    boards::create_thread(&self.shared, identity, board_id, title, body).await
                }
                None => Response::Error(INTERNAL_ERROR.into()),
            },
            Request::Reply { thread_id, body } => match &self.identity {
                Some(identity) => boards::reply(&self.shared, identity, thread_id, body).await,
                None => Response::Error(INTERNAL_ERROR.into()),
            },
            Request::ListKeys => self.keys_response(None).await,
            Request::AddKey(line) => {
                let notice = self.add_key(line).await;
                self.keys_response(Some(notice)).await
            }
            Request::DeleteKey(id) => {
                let notice = match self.user_id() {
                    Some(user_id) => self
                        .shared
                        .blocking(move |s| s.db.delete_key(user_id, id))
                        .await
                        .map(|()| "Key removed.".to_string())
                        .map_err(|_| INTERNAL_ERROR.to_string()),
                    None => Err("Register an account first.".into()),
                };
                self.keys_response(Some(notice)).await
            }
        }
    }

    /// Changes the logged-in user's password. Requires the current one, so a
    /// hijacked or unattended session can't take the account over, and signs
    /// out the user's other sessions so anyone who had the old password (or
    /// a session) is locked out.
    async fn change_password(&mut self, current: String, new: String) -> Result<String, String> {
        let Some(identity @ Identity::User { id, .. }) = &self.identity else {
            return Err("Register an account first.".into());
        };
        let user_id = *id;
        // Name and status come from the database (suspended accounts are refused).
        let account = boards::live_account(&self.shared, identity).await?;

        let subject = Subject::User(user_id);
        if !self.shared.limiter.allowed(subject, Event::PasswordAttempt) {
            return Err("Too many wrong attempts. Try again in a few minutes.".into());
        }
        auth::validate_password(&account.username, &new)?;
        if new == current {
            return Err("The new password must differ from the current one.".into());
        }

        let name = account.username.clone();
        let record = self
            .shared
            .blocking(move |s| s.db.find_user(&name))
            .await
            .map_err(|_| INTERNAL_ERROR.to_string())?
            .ok_or_else(|| INTERNAL_ERROR.to_string())?;

        let _permit = self
            .shared
            .hash_slots
            .acquire()
            .await
            .map_err(|_| INTERNAL_ERROR.to_string())?;
        let stored = record.password_hash;
        let correct = self
            .shared
            .blocking(move |_| auth::verify_password(&current, &stored))
            .await;
        if !correct {
            self.shared.limiter.record(subject, Event::PasswordAttempt);
            return Err("Current password is wrong.".into());
        }

        let hash = self
            .shared
            .blocking(move |_| auth::hash_password(&new))
            .await
            .map_err(|err| {
                eprintln!("{err}");
                INTERNAL_ERROR.to_string()
            })?;
        self.shared
            .blocking(move |s| s.db.set_password_hash(user_id, &hash))
            .await
            .map_err(|_| INTERNAL_ERROR.to_string())?;

        let keep = self.online.as_ref().map(|guard| guard.id());
        let mut signed_out = 0;
        for (session_id, handle) in self.shared.online.sessions_of(user_id) {
            if Some(session_id) != keep {
                let _ = handle
                    .disconnect(
                        Disconnect::ByApplication,
                        "Your password was changed; please log in again.".into(),
                        String::new(),
                    )
                    .await;
                signed_out += 1;
            }
        }
        Ok(match signed_out {
            0 => "Password changed.".to_string(),
            1 => "Password changed. Your other session was signed out.".to_string(),
            n => format!("Password changed. Your {n} other sessions were signed out."),
        })
    }

    async fn join_chat(&self) -> Response {
        let Some(identity @ Identity::User { .. }) = &self.identity else {
            return Response::ChatRejected("The chat is for registered users.".into());
        };
        // Name and role come from the database, not from the login session.
        match boards::live_account(&self.shared, identity).await {
            Ok(account) => Response::ChatJoined(self.shared.chat.join(
                self.chat_id,
                &account.username,
                account.role == Role::Sysop,
            )),
            Err(message) => Response::ChatRejected(message),
        }
    }

    async fn chat_say(&self, text: String, action: bool) -> Response {
        let Some(identity) = &self.identity else {
            return Response::ChatRejected(INTERNAL_ERROR.into());
        };
        let account = match boards::live_account(&self.shared, identity).await {
            Ok(account) => account,
            Err(message) => return Response::ChatRejected(message),
        };
        if !self.shared.chat.is_member(self.chat_id) {
            return Response::ChatRejected("You're not in the chat room.".into());
        }
        let subject = Subject::User(account.id);
        if !self.shared.limiter.allowed(subject, Event::ChatMessage) {
            return Response::ChatRejected("Slow down a little.".into());
        }
        let text = match content::clean_chat(&text) {
            Ok(text) => text,
            Err(message) => return Response::ChatRejected(message),
        };
        self.shared.limiter.record(subject, Event::ChatMessage);
        self.shared
            .chat
            .say(&account.username, account.role == Role::Sysop, &text, action);
        Response::Nothing
    }

    fn who_is_online(&self) -> Response {
        let mut users: Vec<OnlineInfo> = Vec::new();
        let mut guests = 0;
        for entry in self.shared.online.snapshot() {
            // Snapshot is ordered longest-connected first, so the first
            // entry for a user carries their real "connected since".
            match entry.user_id {
                None => guests += 1,
                Some(id) => match users.iter_mut().find(|u| u.user_id == id) {
                    Some(existing) => existing.sessions += 1,
                    None => users.push(OnlineInfo {
                        user_id: id,
                        name: entry.name,
                        is_sysop: entry.role == Role::Sysop,
                        connected: entry.connected,
                        activity: entry.activity,
                        sessions: 1,
                    }),
                },
            }
        }
        users.sort_by_key(|u| u.name.to_lowercase());
        Response::Online { users, guests }
    }

    /// Adds this session to the who's-online registry.
    fn join_online(&mut self, handle: russh::server::Handle) {
        let Some(identity) = &self.identity else {
            return;
        };
        let (user_id, role) = match identity {
            Identity::Guest => (None, Role::User),
            Identity::User { id, role, .. } => (Some(*id), *role),
        };
        self.online = Some(self.shared.online.join(OnlineEntry {
            name: identity.display_name().to_string(),
            user_id,
            role,
            since: std::time::Instant::now(),
            activity: "Main menu",
            handle,
        }));
    }

    /// Keeps the registry entry in step with the session's identity and screen.
    fn sync_online(&self) {
        let (Some(guard), Some(identity)) = (&self.online, &self.identity) else {
            return;
        };
        let Some(activity) = lock_ui(&self.ui).app.as_ref().map(|app| app.activity()) else {
            return;
        };
        let (name, user_id, role) = match identity {
            Identity::Guest => ("guest".to_string(), None, Role::User),
            Identity::User { id, name, role } => (name.clone(), Some(*id), *role),
        };
        guard.update(|e| {
            e.activity = activity;
            e.name = name;
            e.user_id = user_id;
            e.role = role;
        });
    }

    fn user_id(&self) -> Option<i64> {
        match self.identity {
            Some(Identity::User { id, .. }) => Some(id),
            _ => None,
        }
    }

    async fn register(&mut self, username: String, password: String) -> Result<Identity, String> {
        if !matches!(self.identity, Some(Identity::Guest)) {
            return Err("You already have an account.".into());
        }
        // The UI validates too, but never trust the client side of anything.
        auth::validate_username(&username)?;
        auth::validate_password(&username, &password)?;

        if let Some(ip) = self.peer_ip {
            if !self
                .shared
                .limiter
                .allowed(Subject::ip(ip), Event::Registration)
            {
                return Err("Too many registrations from your address. Try again later.".into());
            }
            self.shared
                .limiter
                .record(Subject::ip(ip), Event::Registration);
        }

        let _permit = self
            .shared
            .hash_slots
            .acquire()
            .await
            .map_err(|_| INTERNAL_ERROR.to_string())?;
        let hash = self
            .shared
            .blocking(move |_| auth::hash_password(&password))
            .await
            .map_err(|err| {
                eprintln!("{err}");
                INTERNAL_ERROR.to_string()
            })?;

        let name = username.clone();
        match self
            .shared
            .blocking(move |s| s.db.create_user(&name, &hash))
            .await
        {
            Ok(id) => {
                let identity = Identity::User {
                    id,
                    name: username,
                    role: Role::User,
                };
                self.identity = Some(identity.clone());
                Ok(identity)
            }
            Err(DbError::Duplicate) => Err("That username is already taken.".into()),
            Err(_) => Err(INTERNAL_ERROR.into()),
        }
    }

    async fn add_key(&self, line: String) -> Result<String, String> {
        let user_id = self.user_id().ok_or("Register an account first.")?;
        let (key, comment) = auth::parse_public_key(&line)?;
        let fingerprint = auth::fingerprint(&key);
        let shown = fingerprint.clone();
        match self
            .shared
            .blocking(move |s| s.db.add_key(user_id, &fingerprint, &comment))
            .await
        {
            Ok(()) => Ok(format!("Key added: {shown}")),
            Err(DbError::Duplicate) => Err("That key is already registered.".into()),
            Err(DbError::LimitReached) => {
                Err(format!("You can store at most {MAX_KEYS_PER_USER} keys."))
            }
            Err(DbError::Other | DbError::NotFound | DbError::NotEmpty) => {
                Err(INTERNAL_ERROR.into())
            }
        }
    }

    async fn keys_response(&self, notice: Option<Result<String, String>>) -> Response {
        let Some(user_id) = self.user_id() else {
            return Response::Keys {
                keys: Vec::new(),
                notice: Some(Err("Register an account first.".into())),
            };
        };
        match self.shared.blocking(move |s| s.db.list_keys(user_id)).await {
            Ok(records) => Response::Keys {
                keys: records
                    .into_iter()
                    .map(|r| KeyInfo {
                        id: r.id,
                        fingerprint: r.fingerprint,
                        comment: r.comment,
                    })
                    .collect(),
                notice,
            },
            Err(_) => Response::Keys {
                keys: Vec::new(),
                notice: Some(Err(INTERNAL_ERROR.into())),
            },
        }
    }
}

impl Handler for BbsHandler {
    type Error = anyhow::Error;

    async fn auth_password(&mut self, user: &str, password: &str) -> Result<Auth, Self::Error> {
        if self.over_limit {
            bail!("too many connections from this address");
        }
        if user.len() > MAX_SSH_USER_LEN
            || password.len() > MAX_SSH_PASSWORD_LEN
            || self.auth_throttled()
        {
            return Ok(Auth::reject());
        }

        let identity = if user.eq_ignore_ascii_case(GUEST_USER) {
            (password == self.shared.guest_password).then_some(Identity::Guest)
        } else {
            self.verify_password_login(user, password).await
        };
        Ok(self.finish_auth(identity))
    }

    /// Cheap pre-check so clients don't sign for keys we'd reject anyway.
    /// Deliberately doesn't count towards the failure limit.
    async fn auth_publickey_offered(
        &mut self,
        user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        if self.over_limit {
            bail!("too many connections from this address");
        }
        if user.len() > MAX_SSH_USER_LEN || self.auth_throttled() {
            return Ok(Auth::reject());
        }
        Ok(match self.key_login(user, public_key).await {
            Some(_) => Auth::Accept,
            None => Auth::reject(),
        })
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        if self.over_limit {
            bail!("too many connections from this address");
        }
        if user.len() > MAX_SSH_USER_LEN || self.auth_throttled() {
            return Ok(Auth::reject());
        }
        let identity = self.key_login(user, public_key).await;
        Ok(self.finish_auth(identity))
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        // One session channel per connection; dropping `reply` rejects the rest.
        let Some(identity) = self.identity.clone() else {
            return Ok(());
        };
        if lock_ui(&self.ui).terminal.is_some() {
            return Ok(());
        }

        self.output = Some(TerminalHandle::start(session.handle(), channel.id()).await);
        // The real size arrives with the client's pty request; start at zero.
        let terminal = self.new_terminal(Rect::default())?;
        lock_ui(&self.ui).terminal = Some(terminal);
        let unread = match identity.user_id() {
            Some(id) => boards::unread_total(&self.shared, id).await,
            None => 0,
        };
        let unread_mail = match identity.user_id() {
            Some(id) => mail::unread_count(&self.shared, id).await,
            None => 0,
        };
        let mut app = App::new(identity);
        app.set_unread_threads(unread);
        app.set_unread_mail(unread_mail);
        lock_ui(&self.ui).app = Some(app);
        self.join_online(session.handle());
        self.spawn_listener();

        reply.accept().await;
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.resize(col_width, row_height)?;
        session.channel_success(channel)?;
        self.redraw();
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        _channel: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.resize(col_width, row_height)?;
        self.redraw();
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(channel)?;
        self.redraw();
        Ok(())
    }

    // This is an interactive BBS, not a shell: refuse commands and subsystems
    // (sftp etc.) explicitly so clients get an answer instead of hanging.
    async fn exec_request(
        &mut self,
        channel: ChannelId,
        _data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        channel: ChannelId,
        _name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_failure(channel)?;
        Ok(())
    }

    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        {
            let mut ui = lock_ui(&self.ui);
            let Some(app) = ui.app.as_mut() else {
                return Ok(());
            };
            app.push_input(data);
        }

        loop {
            let action = {
                let mut ui = lock_ui(&self.ui);
                ui.app.as_mut().and_then(App::pump)
            };
            let Some(action) = action else {
                break;
            };
            match action {
                Action::Quit => {
                    session.close(channel)?;
                    return Ok(());
                }
                Action::Request(request) => {
                    // Show any "working…" state before doing slow work, and
                    // make sure the registry reflects the screen the request
                    // came from (who's-online lists the requester too).
                    self.sync_online();
                    self.redraw();
                    let response = self.serve(request).await;
                    if let Some(app) = lock_ui(&self.ui).app.as_mut() {
                        app.on_response(response);
                    }
                }
            }
        }

        self.sync_online();
        self.redraw();
        Ok(())
    }
}

impl BbsHandler {
    /// Builds a fixed-size terminal drawing into the client's channel.
    ///
    /// `Terminal::resize` can't be used for this: it asks the backend for
    /// its size, and the crossterm backend answers by querying *this
    /// process's* stdout, not the SSH client's terminal. A fresh terminal
    /// with a fixed viewport never asks.
    fn new_terminal(&self, area: Rect) -> anyhow::Result<SshTerminal> {
        let Some(output) = self.output.as_ref() else {
            bail!("no output channel");
        };
        let mut backend = CrosstermBackend::new(output.duplicate());
        backend.clear_region(ClearType::All)?;
        backend.flush()?;
        Ok(Terminal::with_options(
            backend,
            ratatui::TerminalOptions {
                viewport: ratatui::Viewport::Fixed(area),
            },
        )?)
    }

    fn resize(&mut self, cols: u32, rows: u32) -> anyhow::Result<()> {
        let has_terminal = lock_ui(&self.ui).terminal.is_some();
        if has_terminal {
            let area = Rect {
                x: 0,
                y: 0,
                width: cols.clamp(1, MAX_TERMINAL_DIM) as u16,
                height: rows.clamp(1, MAX_TERMINAL_DIM) as u16,
            };
            let terminal = self.new_terminal(area)?;
            lock_ui(&self.ui).terminal = Some(terminal);
        }
        Ok(())
    }
}

/// Disconnects sessions of users who were banned while online. Bans are made
/// through the admin tool, a separate process, so the server polls for them.
pub async fn sweep_banned(shared: Arc<Shared>) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(10));
    loop {
        tick.tick().await;
        for (user_id, handle) in shared.online.user_sessions() {
            let banned = matches!(
                shared.blocking(move |s| s.db.account(user_id)).await,
                Ok(Some(account)) if account.banned
            );
            if banned {
                let _ = handle
                    .disconnect(
                        Disconnect::ByApplication,
                        "Your account has been suspended.".into(),
                        String::new(),
                    )
                    .await;
            }
        }
    }
}
