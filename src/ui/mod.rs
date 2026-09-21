mod input;
mod keys;
mod register;

use std::collections::VecDeque;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::state::Identity;
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
}

pub enum Response {
    Registered(Result<Identity, String>),
    Keys {
        keys: Vec<KeyInfo>,
        /// Outcome of the add/delete that triggered this refresh, if any.
        notice: Option<Result<String, String>>,
    },
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
            MenuItem::Board => "Message board (coming soon)",
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
            MenuItem::Board | MenuItem::Online => {
                self.set_status("Not implemented yet.", false);
            }
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
