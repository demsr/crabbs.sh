use std::collections::VecDeque;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

use super::input::{Key, TextInput};
use super::text::{Counter, wrap};
use crate::chat::{ChatEvent, ChatKind, ChatSnapshot, Member};
use crate::content::CHAT_MAX;

/// Lines kept on screen; older ones scroll away for good.
const MAX_LINES: usize = 500;
/// Events that arrived before the join snapshot, replayed once it lands.
const MAX_PENDING: usize = 256;
/// Below this width the member list is hidden.
const MEMBER_LIST_MIN_WIDTH: u16 = 60;
const MEMBER_LIST_WIDTH: u16 = 22;

const DIM: Style = Style::new().fg(Color::Gray);

pub enum ChatAction {
    None,
    Leave,
    Say { text: String, action: bool },
}

enum ChatLine {
    Message {
        at: u64,
        from: String,
        is_sysop: bool,
        text: String,
        action: bool,
    },
    Join { at: u64, name: String },
    Leave { at: u64, name: String },
    /// Local notice, never sent to anyone else.
    System(String),
}

pub struct ChatScreen {
    me: String,
    lines: VecDeque<ChatLine>,
    members: Vec<Member>,
    last_seq: u64,
    loaded: bool,
    pending: Vec<ChatEvent>,
    input: TextInput,
    /// How many wrapped lines the view is scrolled up from the bottom.
    offset: usize,
    // Layout facts recorded while drawing, used to keep the view anchored.
    width: Counter,
    height: Counter,
    total: Counter,
}

fn hhmm(at: u64) -> String {
    let secs = at % 86_400;
    format!("{:02}:{:02}", secs / 3600, secs % 3600 / 60)
}

impl ChatScreen {
    pub fn new(me: &str) -> Self {
        Self {
            me: me.to_string(),
            lines: VecDeque::new(),
            members: Vec::new(),
            last_seq: 0,
            loaded: false,
            pending: Vec::new(),
            input: TextInput::new(CHAT_MAX, false),
            offset: 0,
            width: Counter::new(80),
            height: Counter::new(10),
            total: Counter::new(0),
        }
    }

    /// Replaces the room state with a snapshot (on joining, and again if the
    /// listener fell behind). Events that raced ahead of it are replayed.
    pub fn set_snapshot(&mut self, snapshot: ChatSnapshot) {
        self.lines.clear();
        self.members = snapshot.members;
        self.last_seq = 0;
        self.offset = 0;
        for event in snapshot.history {
            self.apply(event);
        }
        self.last_seq = snapshot.seq;
        self.loaded = true;
        for event in std::mem::take(&mut self.pending) {
            self.on_event(event);
        }
    }

    /// Applies a live event. Returns whether the screen changed.
    pub fn on_event(&mut self, event: ChatEvent) -> bool {
        if !self.loaded {
            if self.pending.len() < MAX_PENDING {
                self.pending.push(event);
            }
            return false;
        }
        if event.seq <= self.last_seq {
            return false; // already covered by the snapshot
        }
        self.last_seq = event.seq;
        self.apply(event);
        true
    }

    fn apply(&mut self, event: ChatEvent) {
        match event.kind {
            ChatKind::Message {
                from,
                is_sysop,
                text,
                action,
            } => self.push(ChatLine::Message {
                at: event.at,
                from,
                is_sysop,
                text,
                action,
            }),
            ChatKind::Join { name, is_sysop } => {
                if !self.members.iter().any(|m| m.name == name) {
                    self.members.push(Member {
                        name: name.clone(),
                        is_sysop,
                    });
                    self.members.sort_by_key(|m| m.name.to_lowercase());
                }
                self.push(ChatLine::Join { at: event.at, name });
            }
            ChatKind::Leave { name } => {
                self.members.retain(|m| m.name != name);
                self.push(ChatLine::Leave { at: event.at, name });
            }
        }
    }

    pub fn add_system(&mut self, text: impl Into<String>) {
        self.push(ChatLine::System(text.into()));
    }

    /// Adds a line. If the reader has scrolled up, the view is kept on the
    /// same text instead of being pushed along by the new line.
    fn push(&mut self, line: ChatLine) {
        if self.offset > 0 {
            let height = self.render_line(&line, self.width.get()).len();
            self.offset += height;
        }
        self.lines.push_back(line);
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
    }

    fn scroll(&mut self, up: bool, amount: usize) {
        let max = self.total.get().saturating_sub(self.height.get());
        self.offset = if up {
            (self.offset + amount).min(max)
        } else {
            self.offset.saturating_sub(amount)
        };
    }

    pub fn handle(&mut self, key: Key) -> ChatAction {
        let page = self.height.get().saturating_sub(1).max(1);
        match key {
            Key::Esc => return ChatAction::Leave,
            Key::Enter => return self.submit(),
            Key::Up => self.scroll(true, 1),
            Key::Down => self.scroll(false, 1),
            Key::PageUp => self.scroll(true, page),
            Key::PageDown => self.scroll(false, page),
            Key::Home if self.input.is_empty() => self.scroll(true, usize::MAX / 2),
            Key::End if self.input.is_empty() => self.offset = 0,
            other => {
                self.input.handle(other);
            }
        }
        ChatAction::None
    }

    fn submit(&mut self) -> ChatAction {
        let text = self.input.value();
        let text = text.trim();
        if text.is_empty() {
            return ChatAction::None;
        }
        self.input.clear();
        self.offset = 0;

        let Some(command) = text.strip_prefix('/') else {
            return ChatAction::Say {
                text: text.to_string(),
                action: false,
            };
        };
        let (name, rest) = command.split_once(' ').unwrap_or((command, ""));
        match name.to_lowercase().as_str() {
            "me" if !rest.trim().is_empty() => ChatAction::Say {
                text: rest.trim().to_string(),
                action: true,
            },
            "me" => {
                self.add_system("Usage: /me <what you do>");
                ChatAction::None
            }
            "who" => {
                let names: Vec<&str> = self.members.iter().map(|m| m.name.as_str()).collect();
                self.add_system(format!("In chat ({}): {}", names.len(), names.join(", ")));
                ChatAction::None
            }
            "quit" | "exit" | "leave" => ChatAction::Leave,
            "help" => {
                self.add_system("Commands: /me <action> · /who · /quit (or Esc) · /help");
                self.add_system("PgUp/PgDn or ↑/↓ scroll the history, End jumps to the newest.");
                ChatAction::None
            }
            other => {
                self.add_system(format!("Unknown command /{other}. Try /help."));
                ChatAction::None
            }
        }
    }

    // ---------------------------------------------------------- drawing

    /// One logical line as display lines: a coloured prefix on the first,
    /// continuation lines indented under the text.
    fn render_line(&self, line: &ChatLine, width: usize) -> Vec<Line<'static>> {
        let (prefix, prefix_style, text, text_style) = match line {
            ChatLine::Message {
                at,
                from,
                is_sysop,
                text,
                action,
            } => {
                let name_style = if *is_sysop {
                    Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD)
                } else if *from == self.me {
                    Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)
                } else {
                    Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD)
                };
                if *action {
                    (
                        format!("[{}] * {from} ", hhmm(*at)),
                        Style::new().fg(Color::Magenta),
                        text.clone(),
                        Style::new().fg(Color::Magenta),
                    )
                } else {
                    (
                        format!("[{}] <{from}> ", hhmm(*at)),
                        name_style,
                        text.clone(),
                        Style::new(),
                    )
                }
            }
            ChatLine::Join { at, name } => (
                format!("[{}] → {name} joined", hhmm(*at)),
                DIM,
                String::new(),
                DIM,
            ),
            ChatLine::Leave { at, name } => (
                format!("[{}] ← {name} left", hhmm(*at)),
                DIM,
                String::new(),
                DIM,
            ),
            ChatLine::System(text) => (
                "* ".to_string(),
                Style::new().fg(Color::Yellow),
                text.clone(),
                Style::new().fg(Color::Yellow),
            ),
        };

        let prefix_width = prefix.chars().count();
        // Narrow terminals: don't indent, just wrap the whole thing.
        let indent = if width > prefix_width + 10 { prefix_width } else { 0 };
        let wrapped = wrap(&text, width.saturating_sub(indent).max(1));
        let mut out = Vec::new();
        for (i, part) in wrapped.into_iter().enumerate() {
            if i == 0 {
                out.push(Line::from(vec![
                    Span::styled(prefix.clone(), prefix_style),
                    Span::styled(part, text_style),
                ]));
            } else {
                out.push(Line::from(Span::styled(
                    format!("{}{part}", " ".repeat(indent)),
                    text_style,
                )));
            }
        }
        out
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(3)])
            .split(area);
        let top = if area.width >= MEMBER_LIST_MIN_WIDTH {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(20), Constraint::Length(MEMBER_LIST_WIDTH)])
                .split(rows[0])
        } else {
            Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(1)])
                .split(rows[0])
        };

        // messages
        let block = Block::default()
            .title(format!("Chat · {} here · times are UTC", self.members.len()))
            .borders(Borders::ALL);
        let inner = block.inner(top[0]);
        let (w, h) = (inner.width as usize, inner.height as usize);
        let mut wrapped: Vec<Line> = Vec::new();
        if !self.loaded {
            wrapped.push(Line::from(Span::styled("Joining chat…", DIM)));
        }
        for line in &self.lines {
            wrapped.extend(self.render_line(line, w));
        }
        self.width.set(w);
        self.height.set(h);
        self.total.set(wrapped.len());
        let max_offset = wrapped.len().saturating_sub(h);
        let offset = self.offset.min(max_offset);
        let end = wrapped.len() - offset;
        let start = end.saturating_sub(h);
        let mut visible: Vec<Line> = wrapped.drain(start..end).collect();
        if offset > 0 {
            if let Some(last) = visible.last_mut() {
                *last = Line::from(Span::styled(
                    format!("— {offset} newer lines below · End to jump down —"),
                    DIM,
                ));
            }
        }
        frame.render_widget(Paragraph::new(visible).block(block), top[0]);

        // members
        if let Some(&area) = top.get(1) {
            let items: Vec<ListItem> = self
                .members
                .iter()
                .map(|m| {
                    if m.is_sysop {
                        ListItem::new(Line::from(vec![
                            Span::raw(m.name.clone()),
                            Span::styled(" (sysop)", Style::new().fg(Color::Yellow)),
                        ]))
                    } else {
                        ListItem::new(m.name.clone())
                    }
                })
                .collect();
            frame.render_widget(
                List::new(items).block(Block::default().title("In chat").borders(Borders::ALL)),
                area,
            );
        }

        self.input.render(
            frame,
            rows[1],
            "Message · Enter: send · /help · Esc: leave",
            true,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(seq: u64, from: &str, text: &str) -> ChatEvent {
        ChatEvent {
            seq,
            at: 12 * 3600 + 34 * 60,
            kind: ChatKind::Message {
                from: from.into(),
                is_sysop: false,
                text: text.into(),
                action: false,
            },
        }
    }

    fn snapshot(seq: u64, history: Vec<ChatEvent>) -> ChatSnapshot {
        ChatSnapshot {
            seq,
            history,
            members: vec![Member {
                name: "alice".into(),
                is_sysop: false,
            }],
        }
    }

    fn type_and_enter(screen: &mut ChatScreen, text: &str) -> ChatAction {
        for c in text.chars() {
            screen.handle(Key::Char(c));
        }
        screen.handle(Key::Enter)
    }

    #[test]
    fn events_covered_by_the_snapshot_are_ignored() {
        let mut s = ChatScreen::new("alice");
        s.set_snapshot(snapshot(2, vec![message(1, "bob", "a"), message(2, "bob", "b")]));
        assert!(!s.on_event(message(2, "bob", "b")), "duplicate");
        assert!(s.on_event(message(3, "bob", "c")));
        assert_eq!(s.lines.len(), 3);
    }

    #[test]
    fn events_arriving_before_the_snapshot_are_replayed_not_lost() {
        let mut s = ChatScreen::new("alice");
        // seq 3 raced ahead of the snapshot that only covers up to 2.
        assert!(!s.on_event(message(2, "bob", "b")));
        assert!(!s.on_event(message(3, "bob", "c")));
        s.set_snapshot(snapshot(2, vec![message(1, "bob", "a"), message(2, "bob", "b")]));
        let texts: Vec<String> = s
            .lines
            .iter()
            .filter_map(|l| match l {
                ChatLine::Message { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["a", "b", "c"]);
    }

    #[test]
    fn join_and_leave_update_the_member_list() {
        let mut s = ChatScreen::new("alice");
        s.set_snapshot(snapshot(0, vec![]));
        s.on_event(ChatEvent {
            seq: 1,
            at: 0,
            kind: ChatKind::Join { name: "bob".into(), is_sysop: true },
        });
        assert_eq!(s.members.len(), 2);
        s.on_event(ChatEvent {
            seq: 2,
            at: 0,
            kind: ChatKind::Leave { name: "alice".into() },
        });
        assert_eq!(s.members.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["bob"]);
    }

    #[test]
    fn commands() {
        let mut s = ChatScreen::new("alice");
        s.set_snapshot(snapshot(0, vec![]));
        assert!(matches!(type_and_enter(&mut s, "hello"), ChatAction::Say { ref text, action: false } if text == "hello"));
        assert!(matches!(type_and_enter(&mut s, "/me waves"), ChatAction::Say { ref text, action: true } if text == "waves"));
        assert!(matches!(type_and_enter(&mut s, "/me"), ChatAction::None));
        assert!(matches!(type_and_enter(&mut s, "/who"), ChatAction::None));
        assert!(matches!(type_and_enter(&mut s, "/nope"), ChatAction::None));
        assert!(matches!(type_and_enter(&mut s, "/quit"), ChatAction::Leave));
        assert!(matches!(type_and_enter(&mut s, "   "), ChatAction::None));
        // local notices were added for /me, /who and the unknown command
        assert_eq!(s.lines.iter().filter(|l| matches!(l, ChatLine::System(_))).count(), 3);
        assert!(matches!(s.handle(Key::Esc), ChatAction::Leave));
    }

    #[test]
    fn scrolled_view_stays_on_the_same_text() {
        let mut s = ChatScreen::new("alice");
        s.set_snapshot(snapshot(0, vec![]));
        s.width.set(80);
        s.offset = 5;
        s.on_event(message(1, "bob", "hi"));
        assert_eq!(s.offset, 6, "one new line pushes the anchored view by one");
        s.offset = 0;
        s.on_event(message(2, "bob", "hi"));
        assert_eq!(s.offset, 0, "following the bottom stays at the bottom");
    }

    #[test]
    fn long_messages_wrap_with_an_indent() {
        let s = ChatScreen::new("alice");
        let line = ChatLine::Message {
            at: 0,
            from: "bob".into(),
            is_sysop: false,
            text: "word ".repeat(20).trim().to_string(),
            action: false,
        };
        let out = s.render_line(&line, 40);
        assert!(out.len() > 1);
        let second: String = out[1].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(second.starts_with("          "), "continuation is indented under the text: {second:?}");
    }
}
