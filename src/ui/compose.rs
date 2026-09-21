use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::Request;
use super::input::{Key, TextInput};
use crate::content;

// ------------------------------------------------------------- TextArea

/// A small multi-line editor. Like classic BBS editors it word-wraps as you
/// type at `WRAP_COLS`, so what you write is what gets stored.
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
        // Word-wrap when typing at the end of an overlong line.
        let line = &self.lines[self.row];
        if self.col == line.len() && line.len() > content::WRAP_COLS {
            let break_at = line.iter().rposition(|&ch| ch == ' ').filter(|&i| i > 0);
            match break_at {
                Some(i) => {
                    let tail = self.lines[self.row].split_off(i + 1);
                    self.lines[self.row].pop(); // the space we broke at
                    self.col = tail.len();
                    self.lines.insert(self.row + 1, tail);
                    self.row += 1;
                }
                None => {
                    // No space to break at: start a new line with this char.
                    self.lines[self.row].pop();
                    self.lines.insert(self.row + 1, vec![c]);
                    self.row += 1;
                    self.col = 1;
                }
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

    pub fn reply(thread_id: i64, thread_title: &str) -> Self {
        Self {
            kind: Kind::Reply { thread_id },
            heading: format!("Reply to: {thread_title}"),
            title: TextInput::new(0, false),
            body: TextArea::new(content::BODY_MAX),
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
    fn long_word_is_split() {
        let mut area = TextArea::new(1000);
        type_str(&mut area, &"x".repeat(content::WRAP_COLS + 10));
        assert!(
            area.value()
                .lines()
                .all(|l| l.chars().count() <= content::WRAP_COLS)
        );
        assert_eq!(
            area.value().replace('\n', "").len(),
            content::WRAP_COLS + 10
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
