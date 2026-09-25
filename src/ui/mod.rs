mod boards;
mod chat;
mod compose;
mod sysop;
mod input;
mod keys;
mod mail;
mod online;
mod password;
mod register;
mod text;

use std::collections::VecDeque;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use crate::chat::{ChatEvent, ChatSnapshot};
use crate::db::{Folder, MailItem, MailMessage};
use crate::mail::MailNotice;
use crate::state::Identity;
use boards::{BoardList, BoardsEvent, ThreadEvent, ThreadList, ThreadView, ThreadsEvent};
use chat::{ChatAction, ChatScreen};
use compose::{Compose, ComposeEvent};
use input::{Key, KeyParser};
use keys::{KeysEvent, KeysScreen};
use mail::{
    BlocksEvent, BlocksScreen, MailCompose, MailComposeEvent, MailboxEvent, MailboxScreen,
    MessageEvent, MessageScreen,
};
use sysop::{MotdEditor, MotdEvent, SysopMenu, SysopMenuEvent};
use online::{OnlineEvent, OnlineScreen};
use password::{PasswordEvent, PasswordForm};
use register::{FormEvent, RegisterForm};

/// Cap on buffered, not-yet-processed keystrokes.
const MAX_QUEUED_KEYS: usize = 4096;

/// Which page of a thread to show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageTarget {
    /// The page with the first unread post, or the first page.
    Smart,
    /// A specific page, shown from its top.
    Page(usize),
    /// A specific page, scrolled to its bottom (paging backwards).
    PageEnd(usize),
    /// The last page, scrolled to the bottom.
    Last,
}

/// Work the UI needs the server to do (database, hashing, ...). The UI itself
/// is synchronous; the SSH handler runs the request and feeds the answer back
/// through `App::on_response`.
pub enum Request {
    Register {
        username: String,
        password: String,
    },
    ListKeys,
    AddKey(String),
    DeleteKey(i64),
    ListBoards,
    WhoIsOnline,
    /// Members only: mark every thread in the board as read.
    MarkBoardRead {
        board_id: i64,
        /// Thread-list page to show afterwards.
        page: usize,
    },
    /// One page (0-based) of a board's thread list.
    ListThreads {
        board_id: i64,
        page: usize,
    },
    /// Sysop-only; the server checks. `page` is the list page to return to.
    DeleteThread {
        thread_id: i64,
        page: usize,
    },
    /// Sysop-only; the server checks. `page` is the thread page to stay on.
    DeletePost {
        post_id: i64,
        page: usize,
    },
    OpenThread {
        thread_id: i64,
        target: PageTarget,
    },
    /// `title` and `body` are already sanitised by the UI, and are
    /// sanitised again by the server.
    CreateThread {
        board_id: i64,
        title: String,
        body: String,
    },
    Reply {
        thread_id: i64,
        body: String,
    },
    /// Members only. Needs the current password; on success the user's other
    /// sessions are signed out.
    ChangePassword {
        current: String,
        new: String,
    },
    OpenMailbox {
        folder: Folder,
    },
    ReadMessage {
        id: i64,
    },
    SendMail {
        to: String,
        subject: String,
        body: String,
    },
    DeleteMessage {
        id: i64,
        /// Folder to show afterwards.
        folder: Folder,
    },
    ListBlocks,
    BlockUser {
        name: String,
    },
    UnblockUser {
        name: String,
    },
    /// Sysop-only; the server checks.
    CreateBoard {
        name: String,
        description: String,
    },
    /// Sysop-only; the server checks.
    UpdateBoardDescription {
        board_id: i64,
        description: String,
    },
    /// Sysop-only; the server checks.
    GetMotd,
    /// Sysop-only; the server checks. Empty text clears the MOTD.
    SetMotd {
        text: String,
    },
    /// Members only: enter the chat room. Answered with a snapshot.
    JoinChat,
    LeaveChat,
    /// `action` is a `/me` line. The message itself comes back through the
    /// room's broadcast like everyone else's.
    ChatSay {
        text: String,
        action: bool,
    },
}

pub enum Response {
    Registered(Result<Identity, String>),
    Keys {
        keys: Vec<KeyInfo>,
        /// Outcome of the add/delete that triggered this refresh, if any.
        notice: Option<Result<String, String>>,
    },
    Boards {
        boards: Vec<BoardInfo>,
        notice: Option<String>,
    },
    Threads {
        board_id: i64,
        board_name: String,
        threads: Vec<ThreadInfo>,
        notice: Option<String>,
        /// 0-based page shown, number of pages, threads on the whole board.
        page: usize,
        pages: usize,
        total: usize,
    },
    /// `to_end` scrolls to the newest post (used after posting).
    Thread {
        detail: ThreadDetail,
        to_end: bool,
        notice: Option<String>,
    },
    Online {
        users: Vec<OnlineInfo>,
        guests: usize,
    },
    Mailbox {
        folder: Folder,
        items: Vec<MailItem>,
        /// Unread messages in the inbox (whichever folder is shown).
        unread: i64,
        notice: Option<String>,
    },
    Message {
        message: MailMessage,
        unread: i64,
    },
    /// A message was refused; the compose screen stays open.
    MailFailed(String),
    Blocks {
        names: Vec<String>,
        notice: Option<String>,
    },
    /// Outcome of a password change: a confirmation, or why it was refused.
    PasswordChanged(Result<String, String>),
    ChatJoined(ChatSnapshot),
    /// A chat message was refused (rate limit, suspended, ...).
    ChatRejected(String),
    /// The request needs no visible answer.
    Nothing,
    /// A post was rejected; the compose screen stays open.
    PostFailed(String),
    /// A sysop-only board or MOTD action was rejected.
    SysopActionFailed(String),
    Motd {
        text: String,
        notice: Option<String>,
    },
    /// Something couldn't be loaded.
    Error(String),
}

pub struct BoardInfo {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub thread_count: i64,
    /// Threads with posts the viewer hasn't read.
    pub unread_threads: i64,
}

pub struct ThreadInfo {
    pub id: i64,
    pub title: String,
    pub author: String,
    pub post_count: i64,
    pub last_post: String,
    /// Posts in the thread the viewer hasn't read.
    pub unread: i64,
}

pub struct OnlineInfo {
    pub user_id: i64,
    pub name: String,
    pub is_sysop: bool,
    pub connected: std::time::Duration,
    pub activity: &'static str,
    pub sessions: usize,
}

pub struct PostInfo {
    pub id: i64,
    pub author: String,
    pub author_is_sysop: bool,
    pub body: String,
    pub created: String,
    pub unread: bool,
}

pub struct ThreadDetail {
    pub id: i64,
    pub board_id: i64,
    pub board_name: String,
    pub title: String,
    pub posts: Vec<PostInfo>,
    /// 0-based page of the thread shown, number of pages, total posts.
    pub page: usize,
    pub pages: usize,
    pub total: usize,
}

pub struct KeyInfo {
    pub id: i64,
    pub fingerprint: String,
    pub comment: String,
}

pub enum Action {
    Quit,
    Request(Request),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuItem {
    Board,
    Chat,
    Mail,
    Online,
    Register,
    Keys,
    Password,
    SysopTools,
    About,
    LogOff,
}

impl MenuItem {
    fn label(self, unread_threads: i64, unread_mail: i64) -> String {
        if self == MenuItem::Board && unread_threads > 0 {
            return format!("Message boards ({unread_threads} unread)");
        }
        if self == MenuItem::Mail && unread_mail > 0 {
            return format!("Mail ({unread_mail} unread)");
        }
        match self {
            MenuItem::Board => "Message boards",
            MenuItem::Chat => "Chat",
            MenuItem::Mail => "Mail",
            MenuItem::Online => "Who's online",
            MenuItem::Register => "Register an account",
            MenuItem::Keys => "SSH keys",
            MenuItem::Password => "Change password",
            MenuItem::SysopTools => "Sysop tools",
            MenuItem::About => "About this BBS",
            MenuItem::LogOff => "Log off",
        }
        .to_string()
    }
}

enum Screen {
    Menu,
    About,
    Register(RegisterForm),
    Keys(KeysScreen),
    Boards(BoardList),
    Threads(ThreadList),
    Thread(ThreadView),
    Compose(Box<ComposeScreen>),
    Online(OnlineScreen),
    Chat(ChatScreen),
    Password(PasswordForm),
    Mailbox(MailboxScreen),
    Message(MessageScreen),
    MailCompose(Box<MailComposeScreen>),
    Blocks(BlocksScreen),
    SysopMenu(SysopMenu),
    MotdEditor(MotdEditor),
}

/// Like `ComposeScreen`: remembers where to return if cancelled.
struct MailComposeScreen {
    form: MailCompose,
    back: Screen,
}

/// The compose screen remembers what to return to if it's cancelled.
struct ComposeScreen {
    compose: Compose,
    back: Screen,
}

struct Status {
    text: String,
    is_error: bool,
}

pub struct App {
    identity: Identity,
    screen: Screen,
    selected: usize,
    status: Option<Status>,
    parser: KeyParser,
    queue: VecDeque<Key>,
    /// Threads with unread posts, shown on the main menu.
    unread_threads: i64,
    /// Unread messages in the inbox, shown on the main menu.
    unread_mail: i64,
    /// The message of the day, shown as a popup over whatever screen is
    /// current (in practice always the menu, right after login) until any
    /// key dismisses it. Deliberately not a `Screen`: unlike every other
    /// screen it overlays rather than replaces what's underneath.
    motd_popup: Option<String>,
}

impl App {
    pub fn set_unread_threads(&mut self, count: i64) {
        self.unread_threads = count;
    }

    pub fn set_unread_mail(&mut self, count: i64) {
        self.unread_mail = count;
    }

    /// If a message of the day is set, shows it as a popup. Called once,
    /// right after construction.
    pub fn set_motd(&mut self, text: Option<String>) {
        self.motd_popup = text.filter(|t| !t.is_empty());
    }

    /// Mail was delivered somewhere. If it's for this user, note it. Returns
    /// whether the screen changed.
    pub fn on_mail_notice(&mut self, notice: MailNotice) -> bool {
        if self.identity.user_id() != Some(notice.to_user_id) {
            return false;
        }
        self.unread_mail += 1;
        let hint = if matches!(self.screen, Screen::Mailbox(_)) {
            " Press r to refresh."
        } else {
            ""
        };
        self.set_status(format!("New mail from {}.{hint}", notice.from), false);
        true
    }

    pub fn new(identity: Identity) -> Self {
        Self {
            identity,
            screen: Screen::Menu,
            selected: 0,
            status: Some(Status {
                text: "Use ↑/↓ and Enter, or press a number. 'q' logs off.".into(),
                is_error: false,
            }),
            parser: KeyParser::default(),
            queue: VecDeque::new(),
            unread_threads: 0,
            unread_mail: 0,
            motd_popup: None,
        }
    }

    /// What the session is doing, for the who's-online list.
    pub fn activity(&self) -> &'static str {
        if self.motd_popup.is_some() {
            return "Reading the message of the day";
        }
        match self.screen {
            Screen::Menu | Screen::About => "Main menu",
            Screen::Register(_) => "Registering",
            Screen::Keys(_) => "Managing SSH keys",
            Screen::Boards(_) => "Browsing boards",
            Screen::Threads(_) => "Browsing threads",
            Screen::Thread(_) => "Reading a thread",
            Screen::Compose(_) => "Writing a post",
            Screen::Online(_) => "Checking who's online",
            Screen::Chat(_) => "Chatting",
            Screen::Password(_) => "Account settings",
            Screen::Mailbox(_) | Screen::Message(_) | Screen::Blocks(_) => "Reading mail",
            Screen::MailCompose(_) => "Writing mail",
            Screen::SysopMenu(_) | Screen::MotdEditor(_) => "Sysop tools",
        }
    }

    /// A live event from the chat room. Returns whether the screen changed
    /// (and so needs redrawing). Ignored unless the chat screen is open.
    pub fn on_chat_event(&mut self, event: ChatEvent) -> bool {
        match &mut self.screen {
            Screen::Chat(chat) => chat.on_event(event),
            _ => false,
        }
    }

    /// A fresh snapshot after the listener fell behind.
    pub fn on_chat_snapshot(&mut self, snapshot: ChatSnapshot) -> bool {
        match &mut self.screen {
            Screen::Chat(chat) => {
                chat.set_snapshot(snapshot);
                true
            }
            _ => false,
        }
    }

    fn menu_items(&self) -> Vec<MenuItem> {
        let mut items = vec![MenuItem::Board, MenuItem::Chat];
        match self.identity {
            Identity::Guest => items.extend([MenuItem::Online, MenuItem::Register]),
            Identity::User { .. } => {
                items.extend([
                    MenuItem::Mail,
                    MenuItem::Online,
                    MenuItem::Keys,
                    MenuItem::Password,
                ]);
                if self.identity.is_sysop() {
                    items.push(MenuItem::SysopTools);
                }
            }
        }
        items.extend([MenuItem::About, MenuItem::LogOff]);
        items
    }

    /// Buffers raw bytes from the SSH channel. Call `pump` afterwards.
    pub fn push_input(&mut self, data: &[u8]) {
        self.queue.extend(self.parser.feed(data));
        self.queue.truncate(MAX_QUEUED_KEYS);
    }

    /// Processes buffered keys until one needs the server to act.
    /// Remaining keys stay queued until the response has been delivered.
    pub fn pump(&mut self) -> Option<Action> {
        while let Some(key) = self.queue.pop_front() {
            if key == Key::Ctrl('c') {
                return Some(Action::Quit);
            }
            if let Some(action) = self.handle_key(key) {
                return Some(action);
            }
        }
        None
    }

    pub fn on_response(&mut self, response: Response) {
        match response {
            Response::Registered(Ok(identity)) => {
                let name = identity.display_name().to_string();
                self.identity = identity;
                self.screen = Screen::Menu;
                self.selected = 0;
                self.set_status(
                    format!(
                        "Welcome, {name}! Your account is ready. Next time connect as '{name}' \
                         (ssh {name}@<this host>) with your password, or add an SSH key."
                    ),
                    false,
                );
            }
            Response::Registered(Err(message)) => {
                if let Screen::Register(form) = &mut self.screen {
                    form.fail(message);
                }
            }
            Response::Boards { boards, notice } => {
                // The board list is refreshed whenever you come back to it,
                // so it doubles as the source for the main-menu count.
                self.unread_threads = boards.iter().map(|b| b.unread_threads).sum();
                if let Screen::Boards(list) = &mut self.screen {
                    list.set(boards);
                }
                if let Some(text) = notice {
                    self.set_status(text, false);
                }
            }
            Response::Threads {
                board_id,
                board_name,
                threads,
                notice,
                page,
                pages,
                total,
            } => {
                // Replaces whatever is showing: this also answers a deleted
                // post that took its whole thread with it.
                let mut list = ThreadList::loading(
                    board_id,
                    self.identity.is_sysop(),
                    self.identity.user_id().is_some(),
                );
                list.set(board_name, threads, page, pages, total);
                self.screen = Screen::Threads(list);
                if let Some(text) = notice {
                    self.set_status(text, false);
                }
            }
            Response::Thread {
                detail,
                to_end,
                notice,
            } => {
                // Also arrives after a successful post, when the compose
                // screen is still up; the thread replaces it. Moving between
                // pages of the same thread keeps the list page we came from.
                let list_page = match &self.screen {
                    Screen::Thread(view) => view.list_page(),
                    _ => 0,
                };
                self.screen = Screen::Thread(ThreadView::from_detail(
                    detail,
                    to_end,
                    self.identity.is_sysop(),
                    list_page,
                ));
                if let Some(text) = notice {
                    self.set_status(text, false);
                }
            }
            Response::Online { users, guests } => {
                if let Screen::Online(screen) = &mut self.screen {
                    screen.set(users, guests);
                }
            }
            Response::Mailbox {
                folder,
                items,
                unread,
                notice,
            } => {
                self.unread_mail = unread;
                let mut mailbox = MailboxScreen::new(folder);
                mailbox.set(items, unread);
                self.screen = Screen::Mailbox(mailbox);
                if let Some(text) = notice {
                    self.set_status(text, false);
                }
            }
            Response::Message { message, unread } => {
                self.unread_mail = unread;
                self.screen = Screen::Message(MessageScreen::new(message));
            }
            Response::MailFailed(message) => {
                if let Screen::MailCompose(screen) = &mut self.screen {
                    screen.form.fail(message);
                }
            }
            Response::Blocks { names, notice } => {
                let mut blocks = BlocksScreen::new();
                blocks.set(names);
                self.screen = Screen::Blocks(blocks);
                if let Some(text) = notice {
                    self.set_status(text, false);
                }
            }
            Response::PasswordChanged(Ok(notice)) => {
                self.screen = Screen::Menu;
                self.set_status(notice, false);
            }
            Response::PasswordChanged(Err(message)) => {
                if let Screen::Password(form) = &mut self.screen {
                    form.fail(message);
                }
            }
            Response::ChatJoined(snapshot) => {
                if let Screen::Chat(chat) = &mut self.screen {
                    chat.set_snapshot(snapshot);
                }
            }
            Response::ChatRejected(message) => {
                if let Screen::Chat(chat) = &mut self.screen {
                    chat.add_system(message);
                }
            }
            Response::Nothing => {}
            Response::PostFailed(message) => {
                if let Screen::Compose(screen) = &mut self.screen {
                    screen.compose.fail(message);
                }
            }
            Response::SysopActionFailed(message) => match &mut self.screen {
                Screen::Boards(list) => list.fail(message),
                Screen::MotdEditor(editor) => editor.fail(message),
                _ => self.set_status(message, true),
            },
            Response::Motd { text, notice } => {
                if let Screen::MotdEditor(editor) = &mut self.screen {
                    editor.set_text(&text);
                }
                if let Some(text) = notice {
                    self.set_status(text, false);
                }
            }
            Response::Error(message) => self.set_status(message, true),
            Response::Keys { keys, notice } => {
                let ok = !matches!(notice, Some(Err(_)));
                if let Screen::Keys(screen) = &mut self.screen {
                    screen.update(keys, ok);
                }
                match notice {
                    Some(Ok(text)) => self.set_status(text, false),
                    Some(Err(text)) => self.set_status(text, true),
                    None => {}
                }
            }
        }
    }

    fn set_status(&mut self, text: impl Into<String>, is_error: bool) {
        self.status = Some(Status {
            text: text.into(),
            is_error,
        });
    }

    fn handle_key(&mut self, key: Key) -> Option<Action> {
        self.status = None;
        // Only Enter dismisses the popup; every other key is swallowed while
        // it's up rather than reaching whatever screen is underneath.
        if self.motd_popup.is_some() {
            if key == Key::Enter {
                self.motd_popup = None;
            }
            return None;
        }
        let sysop = self.identity.is_sysop();
        let member = self.identity.user_id().is_some();
        match &mut self.screen {
            Screen::Menu => self.handle_menu_key(key),
            Screen::About => {
                self.screen = Screen::Menu;
                None
            }
            Screen::Register(form) => match form.handle(key) {
                FormEvent::None => None,
                FormEvent::Cancel => {
                    self.screen = Screen::Menu;
                    None
                }
                FormEvent::Submit { username, password } => {
                    Some(Action::Request(Request::Register { username, password }))
                }
            },
            Screen::Boards(list) => match list.handle(key) {
                BoardsEvent::None => None,
                BoardsEvent::Back => {
                    self.screen = Screen::Menu;
                    None
                }
                BoardsEvent::Open(board_id) => {
                    self.screen = Screen::Threads(ThreadList::loading(board_id, sysop, member));
                    Some(Action::Request(Request::ListThreads { board_id, page: 0 }))
                }
                BoardsEvent::CreateBoard { name, description } => {
                    Some(Action::Request(Request::CreateBoard { name, description }))
                }
                BoardsEvent::UpdateDescription {
                    board_id,
                    description,
                } => Some(Action::Request(Request::UpdateBoardDescription {
                    board_id,
                    description,
                })),
            },
            Screen::Threads(list) => match list.handle(key) {
                ThreadsEvent::None => None,
                ThreadsEvent::MarkBoardRead { board_id, page } => {
                    Some(Action::Request(Request::MarkBoardRead { board_id, page }))
                }
                ThreadsEvent::DeleteThread { thread_id, page } => {
                    Some(Action::Request(Request::DeleteThread { thread_id, page }))
                }
                // The list stays up until the requested page arrives.
                ThreadsEvent::Page { board_id, page } => {
                    Some(Action::Request(Request::ListThreads { board_id, page }))
                }
                ThreadsEvent::Back => {
                    self.screen = Screen::Boards(BoardList::new(sysop));
                    Some(Action::Request(Request::ListBoards))
                }
                ThreadsEvent::Open {
                    board_id,
                    thread_id,
                    list_page,
                } => {
                    self.screen = Screen::Thread(ThreadView::loading(
                        board_id, thread_id, sysop, list_page,
                    ));
                    Some(Action::Request(Request::OpenThread {
                        thread_id,
                        target: PageTarget::Smart,
                    }))
                }
                ThreadsEvent::New { board_id } => self.start_compose(Compose::new_thread(board_id)),
            },
            Screen::Thread(view) => match view.handle(key) {
                ThreadEvent::None => None,
                ThreadEvent::DeletePost { post_id, page } => {
                    Some(Action::Request(Request::DeletePost { post_id, page }))
                }
                // The current page stays up until the new one arrives.
                ThreadEvent::Goto { thread_id, target } => {
                    Some(Action::Request(Request::OpenThread { thread_id, target }))
                }
                ThreadEvent::Back {
                    board_id,
                    list_page,
                } => {
                    self.screen = Screen::Threads(ThreadList::loading(board_id, sysop, member));
                    Some(Action::Request(Request::ListThreads {
                        board_id,
                        page: list_page,
                    }))
                }
                ThreadEvent::Reply {
                    thread_id,
                    title,
                    quote,
                } => {
                    let quote_ref = quote.as_ref().map(|(a, c, b)| (a.as_str(), c.as_str(), b.as_str()));
                    self.start_compose(Compose::reply(thread_id, &title, quote_ref))
                }
            },
            Screen::Compose(screen) => match screen.compose.handle(key) {
                ComposeEvent::None => None,
                ComposeEvent::Cancel => {
                    if let Screen::Compose(screen) =
                        std::mem::replace(&mut self.screen, Screen::Menu)
                    {
                        self.screen = screen.back;
                    }
                    None
                }
                ComposeEvent::Submit(request) => Some(Action::Request(request)),
            },
            Screen::Mailbox(mailbox) => match mailbox.handle(key) {
                MailboxEvent::None => None,
                MailboxEvent::Back => {
                    self.screen = Screen::Menu;
                    None
                }
                // The mailbox stays up until the message arrives.
                MailboxEvent::Open(id) => Some(Action::Request(Request::ReadMessage { id })),
                MailboxEvent::Delete(id) => Some(Action::Request(Request::DeleteMessage {
                    id,
                    folder: mailbox.folder(),
                })),
                MailboxEvent::Refresh => Some(Action::Request(Request::OpenMailbox {
                    folder: mailbox.folder(),
                })),
                MailboxEvent::Compose => self.start_mail_compose(MailCompose::new(None, None, None)),
                MailboxEvent::Switch(folder) => {
                    self.screen = Screen::Mailbox(MailboxScreen::new(folder));
                    Some(Action::Request(Request::OpenMailbox { folder }))
                }
                MailboxEvent::Blocks => {
                    self.screen = Screen::Blocks(BlocksScreen::new());
                    Some(Action::Request(Request::ListBlocks))
                }
            },
            Screen::Message(message) => match message.handle(key) {
                MessageEvent::None => None,
                MessageEvent::Back(folder) => {
                    self.screen = Screen::Mailbox(MailboxScreen::new(folder));
                    Some(Action::Request(Request::OpenMailbox { folder }))
                }
                MessageEvent::Reply { to, subject, quote } => {
                    let quote_ref = quote.as_ref().map(|(a, c, b)| (a.as_str(), c.as_str(), b.as_str()));
                    self.start_mail_compose(MailCompose::new(Some(&to), Some(&subject), quote_ref))
                }
                MessageEvent::Delete(id) => Some(Action::Request(Request::DeleteMessage {
                    id,
                    folder: message.folder(),
                })),
                MessageEvent::Block(name) => Some(Action::Request(Request::BlockUser { name })),
            },
            Screen::MailCompose(screen) => match screen.form.handle(key) {
                MailComposeEvent::None => None,
                MailComposeEvent::Cancel => {
                    if let Screen::MailCompose(screen) =
                        std::mem::replace(&mut self.screen, Screen::Menu)
                    {
                        self.screen = screen.back;
                    }
                    None
                }
                MailComposeEvent::Submit { to, subject, body } => {
                    Some(Action::Request(Request::SendMail { to, subject, body }))
                }
            },
            Screen::Blocks(blocks) => match blocks.handle(key) {
                BlocksEvent::None => None,
                BlocksEvent::Back => {
                    self.screen = Screen::Mailbox(MailboxScreen::new(Folder::Inbox));
                    Some(Action::Request(Request::OpenMailbox {
                        folder: Folder::Inbox,
                    }))
                }
                BlocksEvent::Unblock(name) => Some(Action::Request(Request::UnblockUser { name })),
            },
            Screen::Password(form) => match form.handle(key) {
                PasswordEvent::None => None,
                PasswordEvent::Cancel => {
                    self.screen = Screen::Menu;
                    None
                }
                PasswordEvent::Submit { current, new } => {
                    Some(Action::Request(Request::ChangePassword { current, new }))
                }
            },
            Screen::Chat(chat) => match chat.handle(key) {
                ChatAction::None => None,
                ChatAction::Leave => {
                    self.screen = Screen::Menu;
                    Some(Action::Request(Request::LeaveChat))
                }
                ChatAction::Say { text, action } => {
                    Some(Action::Request(Request::ChatSay { text, action }))
                }
            },
            Screen::Online(screen) => match screen.handle(key) {
                OnlineEvent::None => None,
                OnlineEvent::Back => {
                    self.screen = Screen::Menu;
                    None
                }
                OnlineEvent::Refresh => Some(Action::Request(Request::WhoIsOnline)),
            },
            Screen::Keys(screen) => match screen.handle(key) {
                KeysEvent::None => None,
                KeysEvent::Back => {
                    self.screen = Screen::Menu;
                    None
                }
                KeysEvent::Add(line) => Some(Action::Request(Request::AddKey(line))),
                KeysEvent::Delete(id) => Some(Action::Request(Request::DeleteKey(id))),
            },
            Screen::SysopMenu(menu) => match menu.handle(key) {
                SysopMenuEvent::None => None,
                SysopMenuEvent::Back => {
                    self.screen = Screen::Menu;
                    None
                }
                SysopMenuEvent::EditMotd => {
                    self.screen = Screen::MotdEditor(MotdEditor::loading());
                    Some(Action::Request(Request::GetMotd))
                }
            },
            Screen::MotdEditor(editor) => match editor.handle(key) {
                MotdEvent::None => None,
                MotdEvent::Cancel => {
                    self.screen = Screen::SysopMenu(SysopMenu::new());
                    None
                }
                MotdEvent::Save(text) => Some(Action::Request(Request::SetMotd { text })),
            },
        }
    }

    /// Opens the editor on top of the current screen. Guests can read but
    /// not post.
    fn start_compose(&mut self, compose: Compose) -> Option<Action> {
        if matches!(self.identity, Identity::Guest) {
            self.set_status(
                "Guests can't post. Register an account from the main menu first.",
                true,
            );
            return None;
        }
        let back = std::mem::replace(&mut self.screen, Screen::Menu);
        self.screen = Screen::Compose(Box::new(ComposeScreen { compose, back }));
        None
    }

    fn start_mail_compose(&mut self, form: MailCompose) -> Option<Action> {
        let back = std::mem::replace(&mut self.screen, Screen::Menu);
        self.screen = Screen::MailCompose(Box::new(MailComposeScreen { form, back }));
        None
    }

    fn handle_menu_key(&mut self, key: Key) -> Option<Action> {
        let count = self.menu_items().len();
        match key {
            Key::Char('q') => return Some(Action::Quit),
            Key::Up | Key::Char('k') => self.selected = (self.selected + count - 1) % count,
            Key::Down | Key::Char('j') => self.selected = (self.selected + 1) % count,
            Key::Enter => return self.activate(),
            Key::Char(c @ '1'..='9') => {
                let index = c as usize - '1' as usize;
                if index < count {
                    self.selected = index;
                    return self.activate();
                }
            }
            _ => {}
        }
        None
    }

    fn activate(&mut self) -> Option<Action> {
        match self.menu_items()[self.selected] {
            MenuItem::Board => {
                self.screen = Screen::Boards(BoardList::new(self.identity.is_sysop()));
                return Some(Action::Request(Request::ListBoards));
            }
            MenuItem::Chat => {
                if matches!(self.identity, Identity::Guest) {
                    self.set_status(
                        "The chat is for registered users. Register an account first.",
                        true,
                    );
                } else {
                    self.screen = Screen::Chat(ChatScreen::new(self.identity.display_name()));
                    return Some(Action::Request(Request::JoinChat));
                }
            }
            MenuItem::Online => {
                self.screen = Screen::Online(OnlineScreen::new());
                return Some(Action::Request(Request::WhoIsOnline));
            }
            MenuItem::Register => self.screen = Screen::Register(RegisterForm::new()),
            MenuItem::Mail => {
                self.screen = Screen::Mailbox(MailboxScreen::new(Folder::Inbox));
                return Some(Action::Request(Request::OpenMailbox {
                    folder: Folder::Inbox,
                }));
            }
            MenuItem::Password => {
                self.screen = Screen::Password(PasswordForm::new(self.identity.display_name()));
            }
            MenuItem::Keys => {
                self.screen = Screen::Keys(KeysScreen::new());
                return Some(Action::Request(Request::ListKeys));
            }
            MenuItem::SysopTools => self.screen = Screen::SysopMenu(SysopMenu::new()),
            MenuItem::About => self.screen = Screen::About,
            MenuItem::LogOff => return Some(Action::Quit),
        }
        None
    }

    pub fn draw(&self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(5),
                Constraint::Length(4),
            ])
            .split(frame.area());

        self.draw_header(frame, chunks[0]);
        let body = chunks[1];
        match &self.screen {
            Screen::Menu => self.draw_menu(frame, body),
            Screen::About => Self::draw_about(frame, body),
            Screen::Register(form) => form.draw(frame, body),
            Screen::Keys(screen) => screen.draw(frame, body),
            Screen::Boards(list) => list.draw(frame, body),
            Screen::Threads(list) => list.draw(frame, body),
            Screen::Thread(view) => view.draw(frame, body),
            Screen::Compose(screen) => screen.compose.draw(frame, body),
            Screen::Online(screen) => screen.draw(frame, body),
            Screen::Chat(chat) => chat.draw(frame, body),
            Screen::Password(form) => form.draw(frame, body),
            Screen::Mailbox(mailbox) => mailbox.draw(frame, body),
            Screen::Message(message) => message.draw(frame, body),
            Screen::MailCompose(screen) => screen.form.draw(frame, body),
            Screen::Blocks(blocks) => blocks.draw(frame, body),
            Screen::SysopMenu(menu) => menu.draw(frame, body),
            Screen::MotdEditor(editor) => editor.draw(frame, body),
        }
        self.draw_status(frame, chunks[2]);

        // Drawn last, on top of everything above: a popup over whatever
        // screen is showing, not a screen of its own.
        if let Some(text) = &self.motd_popup {
            Self::draw_motd_popup(text, frame);
        }
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let who = match self.identity {
            Identity::Guest => "guest (register to get your own account)".to_string(),
            Identity::User { ref name, .. } => name.clone(),
        };
        let header = Paragraph::new(format!("rust-bbs — welcome, {who}"))
            .alignment(Alignment::Center)
            .style(Style::default().add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(header, area);
    }

    fn draw_menu(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .menu_items()
            .iter()
            .enumerate()
            .map(|(i, item)| ListItem::new(format!("[{}] {}", i + 1, item.label(self.unread_threads, self.unread_mail))))
            .collect();

        let list = List::new(items)
            .block(Block::default().title("Main menu").borders(Borders::ALL))
            .highlight_style(
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("> ");

        let mut state = ListState::default();
        state.select(Some(self.selected));
        frame.render_stateful_widget(list, area, &mut state);
    }

    /// A popup centered over the whole frame, sized to the message (within
    /// reasonable bounds) rather than a fixed fraction of the screen, so a
    /// one-line MOTD doesn't look like an oversized empty box.
    fn draw_motd_popup(text: &str, frame: &mut Frame) {
        let full = frame.area();
        let width = (full.width * 4 / 5)
            .max(50.min(full.width))
            .min(full.width.saturating_sub(4).max(1));
        let wrapped = text::wrap(text, width.saturating_sub(2).max(1) as usize).len() as u16;
        let height = (wrapped + 3) // borders (2) + the hint line (1)
            .max(12.min(full.height))
            .min((full.height * 4 / 5).max(1))
            .min(full.height);
        let area = Rect {
            x: full.x + full.width.saturating_sub(width) / 2,
            y: full.y + full.height.saturating_sub(height) / 2,
            width,
            height,
        };

        // An explicit, solid background (unlike the rest of the UI, which
        // never sets one) so the popup reads as a panel raised over the
        // menu instead of blending into the same terminal background.
        // Explicit fg on top of the explicit bg: relying on the terminal's
        // default text color here would risk unreadable low-contrast text
        // (e.g. a light default fg with a light terminal theme) now that
        // the background is forced rather than inherited. `.style()` covers
        // the interior, but border glyphs and the title text each have
        // their own separate style in ratatui's Block and don't inherit it.
        let panel = Style::default().bg(Color::Black).fg(Color::White);
        frame.render_widget(Clear, area);
        let block = Block::default()
            .title(" Message of the day ")
            .style(panel)
            .border_style(panel)
            .title_style(panel)
            .borders(Borders::ALL);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(inner);
        frame.render_widget(
            Paragraph::new(text.to_string()).style(panel).wrap(Wrap { trim: true }),
            rows[0],
        );
        frame.render_widget(
            Paragraph::new("Press Enter to close")
                .alignment(Alignment::Center)
                .style(panel.fg(Color::Gray)),
            rows[1],
        );
    }

    fn draw_about(frame: &mut Frame, area: Rect) {
        let about = Paragraph::new(
            "rust-bbs: a small BBS served entirely over SSH, built with russh + ratatui.\n\n\
             Press any key to return to the menu.",
        )
        .wrap(Wrap { trim: true })
        .block(Block::default().title("About").borders(Borders::ALL));
        frame.render_widget(about, area);
    }

    fn draw_status(&self, frame: &mut Frame, area: Rect) {
        let (text, color) = match &self.status {
            Some(Status {
                text,
                is_error: true,
            }) => (text.as_str(), Color::Red),
            Some(Status { text, .. }) => (text.as_str(), Color::Yellow),
            None => ("", Color::Yellow),
        };
        let status = Paragraph::new(text)
            .wrap(Wrap { trim: true })
            .style(Style::default().fg(color))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(status, area);
    }
}
