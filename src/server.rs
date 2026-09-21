use std::net::IpAddr;
use std::sync::Arc;

use anyhow::bail;
use ratatui::Terminal;
use ratatui::backend::{Backend, ClearType, CrosstermBackend};
use ratatui::layout::Rect;
use russh::keys::PublicKey;
use russh::server::{Auth, ChannelOpenHandle, Handler, Msg, Server as ServerTrait, Session};
use russh::{Channel, ChannelId, Pty};

use crate::{auth, boards};
use crate::db::{DbError, MAX_KEYS_PER_USER};
use crate::state::{ConnectionGuard, Event, Identity, Shared, Subject};
use crate::terminal::TerminalHandle;
use crate::ui::{Action, App, KeyInfo, Request, Response};

type SshTerminal = Terminal<CrosstermBackend<TerminalHandle>>;

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
        BbsHandler {
            shared: Arc::clone(&self.shared),
            peer_ip,
            over_limit: peer_ip.is_some() && guard.is_none(),
            _guard: guard,
            identity: None,
            output: None,
            terminal: None,
            app: None,
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
    /// Where the UI is drawn: the client's SSH channel.
    output: Option<TerminalHandle>,
    terminal: Option<SshTerminal>,
    app: Option<App>,
}

impl BbsHandler {
    fn redraw(&mut self) {
        if let (Some(terminal), Some(app)) = (self.terminal.as_mut(), self.app.as_ref()) {
            let _ = terminal.draw(|frame| app.draw(frame));
        }
    }

    fn auth_throttled(&self) -> bool {
        self.peer_ip
            .is_some_and(|ip| !self.shared.limiter.allowed(Subject::ip(ip), Event::AuthFailure))
    }

    fn note_auth_failure(&self) {
        if let Some(ip) = self.peer_ip {
            self.shared.limiter.record(Subject::ip(ip), Event::AuthFailure);
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
            (true, Some(r)) => Some(Identity::User {
                id: r.id,
                name: r.username,
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
        Some(Identity::User {
            id: found.0,
            name: found.1,
        })
    }

    async fn serve(&mut self, request: Request) -> Response {
        match request {
            Request::Register { username, password } => {
                Response::Registered(self.register(username, password).await)
            }
            Request::ListBoards => boards::list_boards(&self.shared).await,
            Request::ListThreads { board_id } => boards::list_threads(&self.shared, board_id).await,
            Request::OpenThread { thread_id } => {
                boards::open_thread(&self.shared, thread_id, false).await
            }
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
            if !self.shared.limiter.allowed(Subject::ip(ip), Event::Registration) {
                return Err("Too many registrations from your address. Try again later.".into());
            }
            self.shared.limiter.record(Subject::ip(ip), Event::Registration);
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
                let identity = Identity::User { id, name: username };
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
            Err(DbError::Other | DbError::NotFound) => Err(INTERNAL_ERROR.into()),
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
        if self.terminal.is_some() {
            return Ok(());
        }

        self.output = Some(TerminalHandle::start(session.handle(), channel.id()).await);
        // The real size arrives with the client's pty request; start at zero.
        self.terminal = Some(self.new_terminal(Rect::default())?);
        self.app = Some(App::new(identity));

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
        let Some(app) = self.app.as_mut() else {
            return Ok(());
        };
        app.push_input(data);

        loop {
            let Some(action) = self.app.as_mut().and_then(App::pump) else {
                break;
            };
            match action {
                Action::Quit => {
                    session.close(channel)?;
                    return Ok(());
                }
                Action::Request(request) => {
                    // Show any "working…" state before doing slow work.
                    self.redraw();
                    let response = self.serve(request).await;
                    if let Some(app) = self.app.as_mut() {
                        app.on_response(response);
                    }
                }
            }
        }

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
        if self.terminal.is_some() {
            let area = Rect {
                x: 0,
                y: 0,
                width: cols.clamp(1, MAX_TERMINAL_DIM) as u16,
                height: rows.clamp(1, MAX_TERMINAL_DIM) as u16,
            };
            self.terminal = Some(self.new_terminal(area)?);
        }
        Ok(())
    }
}
