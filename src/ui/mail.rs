use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use super::compose::TextArea;
use super::input::{Key, TextInput};
use super::text::{Counter, wrap};
use crate::content;
use crate::db::{Folder, MailItem, MailMessage};

const HIGHLIGHT: Style = Style::new()
    .bg(Color::Blue)
    .fg(Color::White)
    .add_modifier(Modifier::BOLD);
const NEW: Style = Style::new()
    .fg(Color::Green)
    .add_modifier(Modifier::BOLD);
const DIM: Style = Style::new().fg(Color::Gray);

fn split_help(area: Rect) -> (Rect, Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);
    (rows[0], rows[1])
}

fn help_line(frame: &mut Frame, area: Rect, text: &str) {
    frame.render_widget(Paragraph::new(text.to_string()).style(DIM), area);
}

fn prompt_line(frame: &mut Frame, area: Rect, text: String) {
    frame.render_widget(
        Paragraph::new(text).style(Style::new().fg(Color::Yellow)),
        area,
    );
}

/// "Re: subject", without stacking up "Re: Re: Re:" and within the limit.
pub fn reply_subject(subject: &str) -> String {
    let base = subject.strip_prefix("Re: ").unwrap_or(subject);
    format!("Re: {base}")
        .chars()
        .take(content::SUBJECT_MAX)
        .collect()
}

// --------------------------------------------------------------- mailbox

pub enum MailboxEvent {
    None,
    Back,
    Open(i64),
    Compose,
    Delete(i64),
    Refresh,
    Switch(Folder),
    Blocks,
}

pub struct MailboxScreen {
    folder: Folder,
    items: Vec<MailItem>,
    selected: usize,
    loaded: bool,
    unread: i64,
    confirm_delete: bool,
}

impl MailboxScreen {
    pub fn new(folder: Folder) -> Self {
        Self {
            folder,
            items: Vec::new(),
            selected: 0,
            loaded: false,
            unread: 0,
            confirm_delete: false,
        }
    }

    pub fn folder(&self) -> Folder {
        self.folder
    }

    pub fn set(&mut self, items: Vec<MailItem>, unread: i64) {
        self.items = items;
        self.unread = unread;
        self.loaded = true;
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
    }

    pub fn handle(&mut self, key: Key) -> MailboxEvent {
        if self.confirm_delete {
            self.confirm_delete = false;
            if let (Key::Char('y'), Some(item)) = (key, self.items.get(self.selected)) {
                return MailboxEvent::Delete(item.id);
            }
            return MailboxEvent::None;
        }
        let last = self.items.len().saturating_sub(1);
        match key {
            Key::Esc | Key::Char('q') => return MailboxEvent::Back,
            Key::Enter => {
                if let Some(item) = self.items.get(self.selected) {
                    return MailboxEvent::Open(item.id);
                }
            }
            Key::Char('c') => return MailboxEvent::Compose,
            Key::Char('r') => return MailboxEvent::Refresh,
            Key::Char('B') => return MailboxEvent::Blocks,
            Key::Tab => {
                return MailboxEvent::Switch(match self.folder {
                    Folder::Inbox => Folder::Sent,
                    Folder::Sent => Folder::Inbox,
                });
            }
            Key::Char('d') | Key::Delete if !self.items.is_empty() => self.confirm_delete = true,
            Key::Up | Key::Char('k') => self.selected = self.selected.saturating_sub(1),
            Key::Down | Key::Char('j') => self.selected = (self.selected + 1).min(last),
            Key::Home | Key::Char('g') => self.selected = 0,
            Key::End | Key::Char('G') => self.selected = last,
            Key::PageUp => self.selected = self.selected.saturating_sub(10),
            Key::PageDown => self.selected = (self.selected + 10).min(last),
            _ => {}
        }
        MailboxEvent::None
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let (body, help) = split_help(area);
        let (title, empty, who) = match self.folder {
            Folder::Inbox => (
                if self.unread > 0 {
                    format!("Mail · Inbox ({} unread)", self.unread)
                } else {
                    "Mail · Inbox".to_string()
                },
                "(no messages — press 'c' to write one)",
                "From",
            ),
            Folder::Sent => ("Mail · Sent".to_string(), "(nothing sent yet)", "To"),
        };
        let items: Vec<ListItem> = if !self.loaded {
            vec![ListItem::new("Loading…")]
        } else if self.items.is_empty() {
            vec![ListItem::new(empty)]
        } else {
            self.items
                .iter()
                .map(|m| {
                    ListItem::new(Line::from(vec![
                        Span::styled(if m.unread { "* " } else { "  " }, NEW),
                        Span::styled(
                            format!("{:<16}", m.other),
                            if m.unread {
                                Style::new().add_modifier(Modifier::BOLD)
                            } else {
                                Style::new()
                            },
                        ),
                        Span::raw(m.subject.clone()),
                        Span::styled(format!("  {}", m.created), DIM),
                    ]))
                })
                .collect()
        };
        let list = List::new(items)
            .block(
                Block::default()
                    .title(format!("{title}  [{who} · subject · date]"))
                    .borders(Borders::ALL),
            )
            .highlight_style(HIGHLIGHT)
            .highlight_symbol("> ");
        let mut state = ListState::default();
        if !self.items.is_empty() {
            state.select(Some(self.selected));
        }
        frame.render_stateful_widget(list, body, &mut state);

        if self.confirm_delete {
            let subject = self
                .items
                .get(self.selected)
                .map_or("", |m| m.subject.as_str());
            prompt_line(
                frame,
                help,
                format!("Delete \"{subject}\"? y = yes, any other key = no"),
            );
        } else {
            help_line(
                frame,
                help,
                "Enter: read · c: compose · d: delete · Tab: inbox/sent · r: refresh · B: blocked · Esc: back",
            );
        }
    }
}

// --------------------------------------------------------------- message

pub enum MessageEvent {
    None,
    Back(Folder),
    Reply { to: String, subject: String },
    Delete(i64),
    Block(String),
}

enum Confirm {
    Delete,
    Block,
}

pub struct MessageScreen {
    message: MailMessage,
    confirm: Option<Confirm>,
    scroll: Counter,
    total_lines: Counter,
    page_height: Counter,
}

impl MessageScreen {
    pub fn new(message: MailMessage) -> Self {
        Self {
            message,
            confirm: None,
            scroll: Counter::new(0),
            total_lines: Counter::new(0),
            page_height: Counter::new(10),
        }
    }

    /// Which folder the message was opened from.
    pub fn folder(&self) -> Folder {
        if self.message.is_recipient {
            Folder::Inbox
        } else {
            Folder::Sent
        }
    }

    fn scroll_by(&self, delta: isize) {
        let max = self.total_lines.get().saturating_sub(self.page_height.get());
        let now = self.scroll.get().min(max);
        self.scroll.set(now.saturating_add_signed(delta).min(max));
    }

    pub fn handle(&mut self, key: Key) -> MessageEvent {
        if let Some(confirm) = self.confirm.take() {
            if key == Key::Char('y') {
                return match confirm {
                    Confirm::Delete => MessageEvent::Delete(self.message.id),
                    Confirm::Block => MessageEvent::Block(self.message.from.clone()),
                };
            }
            return MessageEvent::None;
        }
        let page = self.page_height.get().saturating_sub(1).max(1) as isize;
        let can_reply = self.message.is_recipient;
        match key {
            Key::Esc | Key::Char('q') => return MessageEvent::Back(self.folder()),
            Key::Char('r') if can_reply => {
                return MessageEvent::Reply {
                    to: self.message.from.clone(),
                    subject: reply_subject(&self.message.subject),
                };
            }
            Key::Char('d') | Key::Delete => self.confirm = Some(Confirm::Delete),
            // Blocking yourself makes no sense, so it isn't offered.
            Key::Char('b') if can_reply && self.message.from != self.message.to => {
                self.confirm = Some(Confirm::Block);
            }
            Key::Up | Key::Char('k') => self.scroll_by(-1),
            Key::Down | Key::Char('j') | Key::Enter => self.scroll_by(1),
            Key::PageUp => self.scroll_by(-page),
            Key::PageDown | Key::Char(' ') => self.scroll_by(page),
            Key::Home | Key::Char('g') => self.scroll.set(0),
            Key::End | Key::Char('G') => self.scroll.set(usize::MAX),
            _ => {}
        }
        MessageEvent::None
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let (body, help) = split_help(area);
        let block = Block::default().title("Message").borders(Borders::ALL);
        let inner = block.inner(body);
        let m = &self.message;

        let label = |name: &str, value: &str| {
            Line::from(vec![
                Span::styled(format!("{name:<9}"), DIM),
                Span::styled(value.to_string(), Style::new().add_modifier(Modifier::BOLD)),
            ])
        };
        let mut lines = vec![
            label("From:", &m.from),
            label("To:", &m.to),
            label("Date:", &m.created),
            label("Subject:", &m.subject),
            Line::default(),
        ];
        lines.extend(
            wrap(&m.body, inner.width as usize)
                .into_iter()
                .map(Line::from),
        );

        self.total_lines.set(lines.len());
        self.page_height.set(inner.height as usize);
        let max = lines.len().saturating_sub(inner.height as usize);
        let scroll = self.scroll.get().min(max);
        self.scroll.set(scroll);
        frame.render_widget(
            Paragraph::new(lines)
                .block(block)
                .scroll((scroll.min(u16::MAX as usize) as u16, 0)),
            body,
        );

        match self.confirm {
            Some(Confirm::Delete) => prompt_line(
                frame,
                help,
                "Delete this message? y = yes, any other key = no".into(),
            ),
            Some(Confirm::Block) => prompt_line(
                frame,
                help,
                format!(
                    "Block {}? They won't be able to send you mail. y = yes, any other key = no",
                    m.from
                ),
            ),
            None => {
                let mut text = String::from("↑/↓/PgUp/PgDn: scroll");
                if m.is_recipient {
                    text.push_str(" · r: reply");
                }
                text.push_str(" · d: delete");
                if m.is_recipient && m.from != m.to {
                    text.push_str(" · b: block sender");
                }
                text.push_str(" · Esc: back");
                help_line(frame, help, &text);
            }
        }
    }
}

// ---------------------------------------------------------------- compose

pub enum MailComposeEvent {
    None,
    Cancel,
    Submit {
        to: String,
        subject: String,
        body: String,
    },
}

pub struct MailCompose {
    to: TextInput,
    subject: TextInput,
    body: TextArea,
    /// 0 = to, 1 = subject, 2 = body
    focus: usize,
    error: Option<String>,
    busy: bool,
}

impl MailCompose {
    /// `to`/`subject` prefill a reply; the cursor then starts in the body.
    pub fn new(to: Option<&str>, subject: Option<&str>) -> Self {
        let mut to_field = TextInput::new(crate::auth::USERNAME_MAX, false);
        let mut subject_field = TextInput::new(content::SUBJECT_MAX, false);
        if let Some(to) = to {
            to_field.set(to);
        }
        if let Some(subject) = subject {
            subject_field.set(subject);
        }
        Self {
            focus: match (to, subject) {
                (Some(_), Some(_)) => 2,
                (Some(_), None) => 1,
                _ => 0,
            },
            to: to_field,
            subject: subject_field,
            body: TextArea::new(content::BODY_MAX),
            error: None,
            busy: false,
        }
    }

    /// The server refused the message; what was typed is kept.
    pub fn fail(&mut self, message: String) {
        self.busy = false;
        self.error = Some(message);
    }

    pub fn handle(&mut self, key: Key) -> MailComposeEvent {
        if self.busy {
            return MailComposeEvent::None;
        }
        match (key, self.focus) {
            (Key::Esc, _) => return MailComposeEvent::Cancel,
            (Key::Ctrl('d'), _) => return self.submit(),
            (Key::Tab, _) => self.focus = (self.focus + 1) % 3,
            (Key::BackTab, _) => self.focus = (self.focus + 2) % 3,
            (Key::Enter | Key::Down, 0 | 1) => self.focus += 1,
            (Key::Up, 1 | 2) if self.focus == 1 => self.focus = 0,
            (other, 0) => {
                self.to.handle(other);
            }
            (other, 1) => {
                self.subject.handle(other);
            }
            (other, _) => {
                self.body.handle(other);
            }
        }
        MailComposeEvent::None
    }

    fn submit(&mut self) -> MailComposeEvent {
        let to = self.to.value().trim().to_string();
        if to.is_empty() {
            self.focus = 0;
            self.error = Some("Who should this go to?".into());
            return MailComposeEvent::None;
        }
        let subject = match content::clean_subject(&self.subject.value()) {
            Ok(s) => s,
            Err(e) => {
                self.focus = 1;
                self.error = Some(e);
                return MailComposeEvent::None;
            }
        };
        let body = match content::clean_body(&self.body.value()) {
            Ok(b) => b,
            Err(e) => {
                self.focus = 2;
                self.error = Some(e);
                return MailComposeEvent::None;
            }
        };
        self.error = None;
        self.busy = true;
        MailComposeEvent::Submit { to, subject, body }
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Min(4),
                Constraint::Length(2),
            ])
            .split(area);
        self.to.render(frame, rows[0], "To (username)", self.focus == 0);
        self.subject.render(frame, rows[1], "Subject", self.focus == 1);
        self.body.render(frame, rows[2], "Message", self.focus == 2);

        let (text, color) = if self.busy {
            ("Sending…".to_string(), Color::Yellow)
        } else if let Some(err) = &self.error {
            (err.clone(), Color::Red)
        } else {
            (
                "Ctrl-D: send · Esc: cancel · Tab: next field".to_string(),
                Color::Gray,
            )
        };
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(color)),
            rows[3],
        );
    }
}

// ------------------------------------------------------------ blocked list

pub enum BlocksEvent {
    None,
    Back,
    Unblock(String),
}

pub struct BlocksScreen {
    names: Vec<String>,
    selected: usize,
    loaded: bool,
}

impl BlocksScreen {
    pub fn new() -> Self {
        Self {
            names: Vec::new(),
            selected: 0,
            loaded: false,
        }
    }

    pub fn set(&mut self, names: Vec<String>) {
        self.names = names;
        self.loaded = true;
        self.selected = self.selected.min(self.names.len().saturating_sub(1));
    }

    pub fn handle(&mut self, key: Key) -> BlocksEvent {
        match key {
            Key::Esc | Key::Char('q') => return BlocksEvent::Back,
            Key::Up | Key::Char('k') => self.selected = self.selected.saturating_sub(1),
            Key::Down | Key::Char('j') => {
                self.selected = (self.selected + 1).min(self.names.len().saturating_sub(1));
            }
            Key::Char('x') | Key::Char('u') | Key::Delete => {
                if let Some(name) = self.names.get(self.selected) {
                    return BlocksEvent::Unblock(name.clone());
                }
            }
            _ => {}
        }
        BlocksEvent::None
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let (body, help) = split_help(area);
        let items: Vec<ListItem> = if !self.loaded {
            vec![ListItem::new("Loading…")]
        } else if self.names.is_empty() {
            vec![ListItem::new(
                "(nobody blocked — press 'b' while reading a message to block its sender)",
            )]
        } else {
            self.names.iter().map(|n| ListItem::new(n.clone())).collect()
        };
        let list = List::new(items)
            .block(
                Block::default()
                    .title("Blocked users (they can't send you mail)")
                    .borders(Borders::ALL),
            )
            .highlight_style(HIGHLIGHT)
            .highlight_symbol("> ");
        let mut state = ListState::default();
        if !self.names.is_empty() {
            state.select(Some(self.selected));
        }
        frame.render_stateful_widget(list, body, &mut state);
        help_line(frame, help, "x: unblock selected · ↑/↓: move · Esc: back");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: i64) -> MailItem {
        MailItem {
            id,
            other: "alice".into(),
            subject: "Hi".into(),
            created: "2026-01-01 00:00".into(),
            unread: true,
        }
    }

    fn message(is_recipient: bool) -> MailMessage {
        MailMessage {
            id: 7,
            from: "alice".into(),
            to: "bob".into(),
            subject: "Hi".into(),
            body: "text".into(),
            created: "2026-01-01 00:00".into(),
            is_recipient,
        }
    }

    fn type_text(field: &mut MailCompose, text: &str) {
        for c in text.chars() {
            field.handle(if c == '\n' { Key::Enter } else { Key::Char(c) });
        }
    }

    #[test]
    fn reply_subjects_dont_stack() {
        assert_eq!(reply_subject("Hello"), "Re: Hello");
        assert_eq!(reply_subject("Re: Hello"), "Re: Hello");
        assert!(reply_subject(&"x".repeat(200)).chars().count() <= content::SUBJECT_MAX);
    }

    #[test]
    fn mailbox_keys() {
        let mut m = MailboxScreen::new(Folder::Inbox);
        m.set(vec![item(1), item(2)], 2);
        assert!(matches!(m.handle(Key::Down), MailboxEvent::None));
        assert!(matches!(m.handle(Key::Enter), MailboxEvent::Open(2)));
        assert!(matches!(m.handle(Key::Char('d')), MailboxEvent::None));
        assert!(matches!(m.handle(Key::Char('n')), MailboxEvent::None), "anything but y cancels");
        m.handle(Key::Char('d'));
        assert!(matches!(m.handle(Key::Char('y')), MailboxEvent::Delete(2)));
        assert!(matches!(m.handle(Key::Tab), MailboxEvent::Switch(Folder::Sent)));
        assert!(matches!(m.handle(Key::Char('c')), MailboxEvent::Compose));
        assert!(matches!(m.handle(Key::Char('B')), MailboxEvent::Blocks));
        assert!(matches!(m.handle(Key::Esc), MailboxEvent::Back));
    }

    #[test]
    fn message_actions_depend_on_which_copy_you_hold() {
        let mut inbox = MessageScreen::new(message(true));
        assert!(matches!(inbox.handle(Key::Char('r')), MessageEvent::Reply { ref to, ref subject } if to == "alice" && subject == "Re: Hi"));
        inbox.handle(Key::Char('b'));
        assert!(matches!(inbox.handle(Key::Char('y')), MessageEvent::Block(ref n) if n == "alice"));
        assert!(matches!(inbox.handle(Key::Esc), MessageEvent::Back(Folder::Inbox)));

        let mut sent = MessageScreen::new(message(false));
        assert!(matches!(sent.handle(Key::Char('r')), MessageEvent::None), "can't reply to your own sent copy");
        sent.handle(Key::Char('b'));
        assert!(matches!(sent.handle(Key::Char('y')), MessageEvent::None), "can't block from the sent folder");
        sent.handle(Key::Char('d'));
        assert!(matches!(sent.handle(Key::Char('y')), MessageEvent::Delete(7)));
        assert!(matches!(sent.handle(Key::Esc), MessageEvent::Back(Folder::Sent)));
    }

    #[test]
    fn compose_validates_before_sending() {
        let mut c = MailCompose::new(None, None);
        assert!(matches!(c.handle(Key::Ctrl('d')), MailComposeEvent::None));
        assert_eq!(c.focus, 0, "recipient missing");

        type_text(&mut c, "bob\n");          // Enter moves to the subject
        assert!(matches!(c.handle(Key::Ctrl('d')), MailComposeEvent::None));
        assert_eq!(c.focus, 1, "subject missing");

        type_text(&mut c, "Hi\n");
        assert!(matches!(c.handle(Key::Ctrl('d')), MailComposeEvent::None));
        assert_eq!(c.focus, 2, "message empty");

        type_text(&mut c, "hello there");
        let event = c.handle(Key::Ctrl('d'));
        assert!(matches!(event, MailComposeEvent::Submit { ref to, ref subject, ref body }
            if to == "bob" && subject == "Hi" && body == "hello there"));
        assert!(c.busy);
        assert!(matches!(c.handle(Key::Esc), MailComposeEvent::None), "locked while sending");
        c.fail("nope".into());
        assert!(!c.busy && c.error.as_deref() == Some("nope"));
    }

    #[test]
    fn replies_start_in_the_body_with_fields_prefilled() {
        let c = MailCompose::new(Some("alice"), Some("Re: Hi"));
        assert_eq!((c.to.value().as_str(), c.subject.value().as_str(), c.focus), ("alice", "Re: Hi", 2));
    }

    #[test]
    fn blocked_list_unblocks_the_selected_name() {
        let mut b = BlocksScreen::new();
        b.set(vec!["alice".into(), "bob".into()]);
        b.handle(Key::Down);
        assert!(matches!(b.handle(Key::Char('x')), BlocksEvent::Unblock(ref n) if n == "bob"));
        assert!(matches!(b.handle(Key::Esc), BlocksEvent::Back));
    }
}
