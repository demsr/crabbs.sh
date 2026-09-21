mod boards;
mod compose;
mod input;
mod keys;
mod register;
mod text;

use std::collections::VecDeque;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::state::Identity;
use boards::{
    BoardList, BoardsEvent, ThreadEvent, ThreadList, ThreadView, ThreadsEvent,
};
use compose::{Compose, ComposeEvent};
use input::{Key, KeyParser};
use keys::{KeysEvent, KeysScreen};
use register::{FormEvent, RegisterForm};

/// Cap on buffered, not-yet-processed keystrokes.
const MAX_QUEUED_KEYS: usize = 4096;

/// Work the UI needs the server to do (database, hashing, ...). The UI itself
/// is synchronous; the SSH handler runs the request and feeds the answer back
/// through `App::on_response`.
pub enum Request {
    Register { username: String, password: String },
    ListKeys,
    AddKey(String),
    DeleteKey(i64),
    ListBoards,
    ListThreads { board_id: i64 },
    OpenThread { thread_id: i64 },
    /// `title` and `body` are already sanitised by the UI, and are
    /// sanitised again by the server.
    CreateThread { board_id: i64, title: String, body: String },
    Reply { thread_id: i64, body: String },
}

pub enum Response {
    Registered(Result<Identity, String>),
    Keys {
        keys: Vec<KeyInfo>,
        /// Outcome of the add/delete that triggered this refresh, if any.
        notice: Option<Result<String, String>>,
    },
    Boards(Vec<BoardInfo>),
    Threads {
        board_name: String,
        threads: Vec<ThreadInfo>,
    },
    /// `to_end` scrolls to the newest post (used after posting).
    Thread { detail: ThreadDetail, to_end: bool },
    /// A post was rejected; the compose screen stays open.
    PostFailed(String),
    /// Something couldn't be loaded.
    Error(String),
}

pub struct BoardInfo {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub thread_count: i64,
}

pub struct ThreadInfo {
    pub id: i64,
    pub title: String,
    pub author: String,
    pub post_count: i64,
    pub last_post: String,
}

pub struct PostInfo {
    pub author: String,
    pub body: String,
    pub created: String,
}

pub struct ThreadDetail {
    pub id: i64,
    pub board_id: i64,
    pub board_name: String,
    pub title: String,
    pub posts: Vec<PostInfo>,
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
    Online,
    Register,
    Keys,
    About,
    LogOff,
}

impl MenuItem {
    fn label(self) -> &'static str {
        match self {
            MenuItem::Board => "Message boards",
            MenuItem::Online => "Who's online (coming soon)",
            MenuItem::Register => "Register an account",
            MenuItem::Keys => "SSH keys",
            MenuItem::About => "About this BBS",
            MenuItem::LogOff => "Log off",
        }
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
}

impl App {
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
        }
    }

    fn menu_items(&self) -> Vec<MenuItem> {
        let account_item = match self.identity {
            Identity::Guest => MenuItem::Register,
            Identity::User { .. } => MenuItem::Keys,
        };
        vec![
            MenuItem::Board,
            MenuItem::Online,
            account_item,
            MenuItem::About,
            MenuItem::LogOff,
        ]
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
            Response::Boards(boards) => {
                if let Screen::Boards(list) = &mut self.screen {
                    list.set(boards);
                }
            }
            Response::Threads {
                board_name,
                threads,
            } => {
                if let Screen::Threads(list) = &mut self.screen {
                    list.set(board_name, threads);
                }
            }
            Response::Thread { detail, to_end } => {
                // Also arrives after a successful post, when the compose
                // screen is still up; the thread replaces it.
                if to_end {
                    self.set_status("Posted.", false);
                }
                self.screen = Screen::Thread(ThreadView::from_detail(detail, to_end));
            }
            Response::PostFailed(message) => {
                if let Screen::Compose(screen) = &mut self.screen {
                    screen.compose.fail(message);
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
                    self.screen = Screen::Threads(ThreadList::loading(board_id));
                    Some(Action::Request(Request::ListThreads { board_id }))
                }
            },
            Screen::Threads(list) => match list.handle(key) {
                ThreadsEvent::None => None,
                ThreadsEvent::Back => {
                    self.screen = Screen::Boards(BoardList::new());
                    Some(Action::Request(Request::ListBoards))
                }
                ThreadsEvent::Open {
                    board_id,
                    thread_id,
                } => {
                    self.screen = Screen::Thread(ThreadView::loading(board_id, thread_id));
                    Some(Action::Request(Request::OpenThread { thread_id }))
                }
                ThreadsEvent::New { board_id } => self.start_compose(Compose::new_thread(board_id)),
            },
            Screen::Thread(view) => match view.handle(key) {
                ThreadEvent::None => None,
                ThreadEvent::Back { board_id } => {
                    self.screen = Screen::Threads(ThreadList::loading(board_id));
                    Some(Action::Request(Request::ListThreads { board_id }))
                }
                ThreadEvent::Reply { thread_id, title } => {
                    self.start_compose(Compose::reply(thread_id, &title))
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
            Screen::Keys(screen) => match screen.handle(key) {
                KeysEvent::None => None,
                KeysEvent::Back => {
                    self.screen = Screen::Menu;
                    None
                }
                KeysEvent::Add(line) => Some(Action::Request(Request::AddKey(line))),
                KeysEvent::Delete(id) => Some(Action::Request(Request::DeleteKey(id))),
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
                self.screen = Screen::Boards(BoardList::new());
                return Some(Action::Request(Request::ListBoards));
            }
            MenuItem::Online => self.set_status("Not implemented yet.", false),
            MenuItem::Register => self.screen = Screen::Register(RegisterForm::new()),
            MenuItem::Keys => {
                self.screen = Screen::Keys(KeysScreen::new());
                return Some(Action::Request(Request::ListKeys));
            }
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
        }
        self.draw_status(frame, chunks[2]);
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
            .map(|(i, item)| ListItem::new(format!("[{}] {}", i + 1, item.label())))
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
            Some(Status { text, is_error: true }) => (text.as_str(), Color::Red),
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
