use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::Request;
use super::input::{Key, TextInput};
use crate::content;

// ------------------------------------------------------------- TextArea

/// A small multi-line editor. Like classic BBS editors it word-wraps as you
/// type at `WRAP_COLS`, so what you write is what gets stored - except for a
/// single token with no space in it (a URL, typically), which is left
/// intact rather than split with an injected line break that would corrupt
/// it; display-time wrapping (console-width-aware, non-destructive) takes
/// care of showing it readably instead.
pub struct TextArea {
    lines: Vec<Vec<char>>,
    row: usize,
    col: usize,
    max_chars: usize,
}

impl TextArea {
    pub fn new(max_chars: usize) -> Self {
        Self {
            lines: vec![Vec::new()],
            row: 0,
            col: 0,
            max_chars,
        }
    }

    pub fn value(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Replaces the contents, e.g. to prefill a reply with a quote. Goes
    /// through the same per-character path as interactive typing, so it
    /// gets the same word-wrap (and the same URL exemption) rather than
    /// dropping in one giant unwrapped line.
    pub fn set_text(&mut self, text: &str) {
        self.lines = vec![Vec::new()];
        self.row = 0;
        self.col = 0;
        for c in text.chars() {
            self.handle(if c == '\n' { Key::Enter } else { Key::Char(c) });
        }
    }

    fn len(&self) -> usize {
        self.lines.iter().map(|l| l.len() + 1).sum::<usize>() - 1
    }

    fn insert_newline(&mut self) {
        let rest = self.lines[self.row].split_off(self.col);
        self.lines.insert(self.row + 1, rest);
        self.row += 1;
        self.col = 0;
    }

    fn insert_char(&mut self, c: char) {
        self.lines[self.row].insert(self.col, c);
        self.col += 1;
        // Word-wrap at the last space once a line runs long. A single
        // unbreakable token - a URL, typically - is left alone rather than
        // split with an injected line break: that would corrupt it in
        // storage (and stay corrupted for every reader), not just look odd
        // while typing. It just runs past WRAP_COLS instead (the render
        // below already scrolls horizontally to follow the cursor), and a
        // reader's own thread view wraps it for *display* based on their
        // actual terminal width, without touching what's stored.
        let line = &self.lines[self.row];
        if self.col == line.len() && line.len() > content::WRAP_COLS {
            if let Some(i) = line.iter().rposition(|&ch| ch == ' ').filter(|&i| i > 0) {
                let tail = self.lines[self.row].split_off(i + 1);
                self.lines[self.row].pop(); // the space we broke at
                self.col = tail.len();
                self.lines.insert(self.row + 1, tail);
                self.row += 1;
            }
        }
    }

    pub fn handle(&mut self, key: Key) -> bool {
        match key {
            Key::Char(c) if !c.is_control() => {
                if self.len() < self.max_chars {
                    self.insert_char(c);
                }
            }
            Key::Enter => {
                if self.len() < self.max_chars {
                    self.insert_newline();
                }
            }
            Key::Tab => {
                for _ in 0..4 {
                    if self.len() < self.max_chars {
                        self.insert_char(' ');
                    }
                }
            }
            Key::Backspace => {
                if self.col > 0 {
                    self.col -= 1;
                    self.lines[self.row].remove(self.col);
                } else if self.row > 0 {
                    let line = self.lines.remove(self.row);
                    self.row -= 1;
                    self.col = self.lines[self.row].len();
                    self.lines[self.row].extend(line);
                }
            }
            Key::Delete => {
                if self.col < self.lines[self.row].len() {
                    self.lines[self.row].remove(self.col);
                } else if self.row + 1 < self.lines.len() {
                    let next = self.lines.remove(self.row + 1);
                    self.lines[self.row].extend(next);
                }
            }
            Key::Left => {
                if self.col > 0 {
                    self.col -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.col = self.lines[self.row].len();
                }
            }
            Key::Right => {
                if self.col < self.lines[self.row].len() {
                    self.col += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = 0;
                }
            }
            Key::Up => {
                if self.row > 0 {
                    self.row -= 1;
                    self.col = self.col.min(self.lines[self.row].len());
                }
            }
            Key::Down => {
                if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.col = self.col.min(self.lines[self.row].len());
                }
            }
            Key::Home | Key::Ctrl('a') => self.col = 0,
            Key::End | Key::Ctrl('e') => self.col = self.lines[self.row].len(),
            _ => return false,
        }
        true
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, title: &str, focused: bool) {
        let border = if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default()
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border);
        let inner = block.inner(area);
        let (w, h) = (inner.width as usize, inner.height as usize);

        // Keep the cursor in view, scrolling vertically and horizontally.
        let top = (self.row + 1).saturating_sub(h.max(1));
        let left = (self.col + 1).saturating_sub(w.max(1));
        let text: Vec<String> = self
            .lines
            .iter()
            .skip(top)
            .take(h)
            .map(|l| l.iter().skip(left).take(w).collect())
            .collect();
        frame.render_widget(Paragraph::new(text.join("\n")).block(block), area);

        if focused && w > 0 && h > 0 {
            frame.set_cursor_position((
                inner.x + (self.col - left) as u16,
                inner.y + (self.row - top) as u16,
            ));
        }
    }
}

// ---------------------------------------------------------------- quote

/// Classic BBS/email reply quoting: an attribution line, then each line of
/// the original prefixed with "> " (a bare ">" for a blank line, so
/// paragraph breaks stay visible), then a blank line to write into.
pub fn quote(author: &str, created: &str, body: &str) -> String {
    let mut out = format!("On {created}, {author} wrote:\n");
    for line in body.lines() {
        // A blank line in the original stays blank in the quote - no bare
        // ">" marker for it - so a multi-paragraph quote doesn't read as
        // if something got dropped in between.
        if !line.is_empty() {
            out.push_str("> ");
            out.push_str(line);
        }
        out.push('\n');
    }
    // A second '\n': the first ends the last quoted line, this one opens a
    // blank separator line, and the cursor lands on the line right after
    // that - so there's one clear empty line between the quote and where
    // the reply starts, not the reply running on immediately after ">...".
    out.push('\n');
    out
}

// -------------------------------------------------------------- Compose

#[derive(Clone, Copy, PartialEq, Eq)]
enum Focus {
    Title,
    Body,
}

enum Kind {
    NewThread { board_id: i64 },
    Reply { thread_id: i64 },
}

pub enum ComposeEvent {
    None,
    Cancel,
    Submit(Request),
}

pub struct Compose {
    kind: Kind,
    heading: String,
    title: TextInput,
    body: TextArea,
    focus: Focus,
    error: Option<String>,
    busy: bool,
}

impl Compose {
    pub fn new_thread(board_id: i64) -> Self {
        Self {
            kind: Kind::NewThread { board_id },
            heading: "New thread".into(),
            title: TextInput::new(content::TITLE_MAX, false),
            body: TextArea::new(content::BODY_MAX),
            focus: Focus::Title,
            error: None,
            busy: false,
        }
    }

    /// `quoted` is the post being replied to (author, when, body), if any -
    /// absent only if the thread turned out to have no posts, which
    /// shouldn't normally happen.
    pub fn reply(thread_id: i64, thread_title: &str, quoted: Option<(&str, &str, &str)>) -> Self {
        let mut body = TextArea::new(content::BODY_MAX);
        if let Some((author, created, text)) = quoted {
            body.set_text(&quote(author, created, text));
        }
        Self {
            kind: Kind::Reply { thread_id },
            heading: format!("Reply to: {thread_title}"),
            title: TextInput::new(0, false),
            body,
            focus: Focus::Body,
            error: None,
            busy: false,
        }
    }

    /// The server rejected the post; keep what was typed so it can be fixed.
    pub fn fail(&mut self, message: String) {
        self.busy = false;
        self.error = Some(message);
    }

    pub fn handle(&mut self, key: Key) -> ComposeEvent {
        if self.busy {
            return ComposeEvent::None;
        }
        match (key, self.focus) {
            (Key::Esc, _) => return ComposeEvent::Cancel,
            (Key::Ctrl('d'), _) => return self.submit(),
            (Key::BackTab, Focus::Body) if matches!(self.kind, Kind::NewThread { .. }) => {
                self.focus = Focus::Title;
            }
            (Key::Tab, Focus::Title) => self.focus = Focus::Body,
            (Key::Enter | Key::Down, Focus::Title) => self.focus = Focus::Body,
            (other, Focus::Title) => {
                self.title.handle(other);
            }
            (other, Focus::Body) => {
                self.body.handle(other);
            }
        }
        ComposeEvent::None
    }

    fn submit(&mut self) -> ComposeEvent {
        let body = match content::clean_body(&self.body.value()) {
            Ok(body) => body,
            Err(e) => {
                self.focus = Focus::Body;
                self.error = Some(e);
                return ComposeEvent::None;
            }
        };
        let request = match self.kind {
            Kind::Reply { thread_id } => Request::Reply { thread_id, body },
            Kind::NewThread { board_id } => match content::clean_title(&self.title.value()) {
                Ok(title) => Request::CreateThread {
                    board_id,
                    title,
                    body,
                },
                Err(e) => {
                    self.focus = Focus::Title;
                    self.error = Some(e);
                    return ComposeEvent::None;
                }
            },
        };
        self.error = None;
        self.busy = true;
        ComposeEvent::Submit(request)
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let is_new = matches!(self.kind, Kind::NewThread { .. });
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(if is_new { 3 } else { 0 }),
                Constraint::Min(4),
                Constraint::Length(2),
            ])
            .split(area);

        frame.render_widget(
            Paragraph::new(self.heading.clone()).style(Style::default().fg(Color::Cyan)),
            rows[0],
        );
        if is_new {
            self.title
                .render(frame, rows[1], "Title", self.focus == Focus::Title);
        }
        self.body
            .render(frame, rows[2], "Message", self.focus == Focus::Body);

        let (text, color) = if self.busy {
            ("Posting…".to_string(), Color::Yellow)
        } else if let Some(err) = &self.error {
            (err.clone(), Color::Red)
        } else {
            (
                "Ctrl-D: post · Esc: cancel · Tab: indent · Shift-Tab: back to title".to_string(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn type_str(area: &mut TextArea, s: &str) {
        for c in s.chars() {
            area.handle(if c == '\n' { Key::Enter } else { Key::Char(c) });
        }
    }

    #[test]
    fn quote_formats_attribution_and_prefixes_every_line() {
        let q = quote("alice", "2026-09-25 12:00", "hello\n\nworld");
        assert_eq!(
            q,
            "On 2026-09-25 12:00, alice wrote:\n> hello\n\n> world\n\n"
        );
        // A blank line in the original stays blank in the quote - no bare
        // ">" marker for it, which would otherwise look like a stray line.
        assert!(!q.lines().any(|l| l == ">"));
        // Ends with a blank line separating the quote from where the
        // reply's own text will start, not running on right after it.
        assert!(q.ends_with("world\n\n"));
    }

    #[test]
    fn reply_prefills_the_body_with_a_quote_ready_to_type_after() {
        let c = Compose::reply(9, "Some thread", Some(("alice", "2026-09-25 12:00", "hi there")));
        assert_eq!(
            c.body.value(),
            "On 2026-09-25 12:00, alice wrote:\n> hi there\n\n"
        );
        assert!(matches!(c.focus, Focus::Body));
    }

    #[test]
    fn reply_without_a_quote_leaves_the_body_empty() {
        let c = Compose::reply(9, "Some thread", None);
        assert!(c.body.value().is_empty());
    }

    #[test]
    fn word_wraps_at_limit() {
        let mut area = TextArea::new(1000);
        let word = "word ";
        type_str(&mut area, &word.repeat(30));
        let value = area.value();
        assert!(
            value
                .lines()
                .all(|l| l.chars().count() <= content::WRAP_COLS)
        );
        assert!(value.lines().count() >= 2);
        assert_eq!(value.split_whitespace().count(), 30);
    }

    #[test]
    fn long_unbreakable_token_is_not_split() {
        // A URL (or anything else with no space) must not come out of the
        // editor with an injected line break in the middle of it: that
        // would be stored, corrupting it for every future reader, not just
        // a display artifact. It stays exactly as typed, however long.
        let mut area = TextArea::new(1000);
        let long_token = "x".repeat(content::WRAP_COLS + 10);
        type_str(&mut area, &long_token);
        assert_eq!(area.value(), long_token);
        assert_eq!(area.value().lines().count(), 1);
    }

    #[test]
    fn url_survives_intact_among_ordinary_prose() {
        let mut area = TextArea::new(1000);
        let url = "https://store.rockstargames.com/de/merchandise/gtavi-goodtime-state-vice-city-collection";
        type_str(&mut area, &format!("check this out:\n{url}\nlooks cool"));
        let value = area.value();
        assert!(value.contains(url), "the URL must appear byte-for-byte unbroken: {value:?}");
        // Ordinary short lines around it still wrap/behave normally.
        assert!(value.contains("check this out:"));
        assert!(value.contains("looks cool"));
    }

    #[test]
    fn wrapping_resumes_normally_right_after_an_unbreakable_token() {
        // Once a natural break point (a space) appears again, normal
        // word-wrap picks back up - the exemption is only for the run that
        // has no space in it, not "everything from here on".
        let mut area = TextArea::new(1000);
        let long_token = "x".repeat(content::WRAP_COLS + 5);
        type_str(&mut area, &format!("{long_token} {}", "word ".repeat(20)));
        let value = area.value();
        assert!(value.starts_with(&long_token));
        assert!(
            value.lines().skip(1).all(|l| l.chars().count() <= content::WRAP_COLS),
            "wrapping resumed normally after the token: {value:?}"
        );
    }

    #[test]
    fn editing_across_lines() {
        let mut area = TextArea::new(100);
        type_str(&mut area, "ab\ncd");
        area.handle(Key::Home);
        area.handle(Key::Backspace); // joins the lines
        assert_eq!(area.value(), "abcd");
        area.handle(Key::Left);
        area.handle(Key::Enter);
        assert_eq!(area.value(), "a\nbcd");
    }

    #[test]
    fn respects_max_chars() {
        let mut area = TextArea::new(5);
        type_str(&mut area, "abcdefgh");
        assert_eq!(area.value(), "abcde");
    }
}
