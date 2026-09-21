use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Enter,
    Backspace,
    Delete,
    Tab,
    BackTab,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
}

/// Turns the raw byte stream from the SSH channel into `Key`s.
///
/// Escape sequences and UTF-8 characters can be split across reads, so an
/// incomplete tail is kept until the next `feed`. The exception is a lone
/// ESC at the very end of a read, which is reported as the Esc key.
#[derive(Default)]
pub struct KeyParser {
    pending: Vec<u8>,
    last_was_cr: bool,
}

enum Escape {
    Incomplete,
    Done(Option<Key>, usize),
}

/// Longest escape sequence we'll wait on before giving up on it.
const MAX_PENDING: usize = 32;

fn parse_escape(s: &[u8]) -> Escape {
    // s[0] == ESC
    match s.get(1) {
        None => Escape::Incomplete,
        Some(b'[') => {
            // CSI: parameter bytes 0x30-0x3f, intermediates 0x20-0x2f, final 0x40-0x7e
            for (i, &b) in s.iter().enumerate().skip(2) {
                if (0x40..=0x7e).contains(&b) {
                    let params = &s[2..i];
                    let key = match (b, params) {
                        (b'A', _) => Some(Key::Up),
                        (b'B', _) => Some(Key::Down),
                        (b'C', _) => Some(Key::Right),
                        (b'D', _) => Some(Key::Left),
                        (b'H', _) => Some(Key::Home),
                        (b'F', _) => Some(Key::End),
                        (b'Z', _) => Some(Key::BackTab),
                        (b'~', b"1" | b"7") => Some(Key::Home),
                        (b'~', b"4" | b"8") => Some(Key::End),
                        (b'~', b"3") => Some(Key::Delete),
                        (b'~', b"5") => Some(Key::PageUp),
                        (b'~', b"6") => Some(Key::PageDown),
                        _ => None,
                    };
                    return Escape::Done(key, i + 1);
                }
                if !(0x20..=0x3f).contains(&b) {
                    // Malformed: drop what we've seen so far.
                    return Escape::Done(None, i);
                }
            }
            if s.len() > MAX_PENDING {
                Escape::Done(None, s.len())
            } else {
                Escape::Incomplete
            }
        }
        Some(b'O') => match s.get(2) {
            None => Escape::Incomplete,
            Some(&b) => Escape::Done(
                match b {
                    b'A' => Some(Key::Up),
                    b'B' => Some(Key::Down),
                    b'C' => Some(Key::Right),
                    b'D' => Some(Key::Left),
                    b'H' => Some(Key::Home),
                    b'F' => Some(Key::End),
                    _ => None,
                },
                3,
            ),
        },
        // ESC followed by something else (Alt+key): report Esc, keep the rest.
        Some(_) => Escape::Done(Some(Key::Esc), 1),
    }
}

impl KeyParser {
    pub fn feed(&mut self, data: &[u8]) -> Vec<Key> {
        let mut buf = std::mem::take(&mut self.pending);
        buf.extend_from_slice(data);

        let mut keys = Vec::new();
        let mut i = 0;
        while i < buf.len() {
            let b = buf[i];
            let is_lf_after_cr = b == b'\n' && self.last_was_cr;
            self.last_was_cr = b == b'\r';
            match b {
                0x1b => match parse_escape(&buf[i..]) {
                    Escape::Incomplete if buf.len() - i == 1 => {
                        keys.push(Key::Esc);
                        i += 1;
                    }
                    Escape::Incomplete => break,
                    Escape::Done(key, len) => {
                        keys.extend(key);
                        i += len;
                    }
                },
                b'\r' | b'\n' => {
                    if !is_lf_after_cr {
                        keys.push(Key::Enter);
                    }
                    i += 1;
                }
                b'\t' => {
                    keys.push(Key::Tab);
                    i += 1;
                }
                0x08 | 0x7f => {
                    keys.push(Key::Backspace);
                    i += 1;
                }
                0x01..=0x1a => {
                    keys.push(Key::Ctrl((b'a' + b - 1) as char));
                    i += 1;
                }
                0x20..=0x7e => {
                    keys.push(Key::Char(b as char));
                    i += 1;
                }
                _ => {
                    let len = match b {
                        0xc2..=0xdf => 2,
                        0xe0..=0xef => 3,
                        0xf0..=0xf4 => 4,
                        _ => {
                            i += 1; // stray/invalid byte
                            continue;
                        }
                    };
                    if buf.len() - i < len {
                        break; // wait for the rest of the character
                    }
                    if let Ok(s) = std::str::from_utf8(&buf[i..i + len]) {
                        keys.extend(s.chars().next().map(Key::Char));
                        i += len;
                    } else {
                        i += 1;
                    }
                }
            }
        }

        if buf.len() - i <= MAX_PENDING {
            self.pending = buf.split_off(i);
        }
        keys
    }
}

/// A single-line text field with a cursor.
pub struct TextInput {
    chars: Vec<char>,
    cursor: usize,
    max_len: usize,
    masked: bool,
}

impl TextInput {
    pub fn new(max_len: usize, masked: bool) -> Self {
        Self {
            chars: Vec::new(),
            cursor: 0,
            max_len,
            masked,
        }
    }

    pub fn value(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
    }

    /// Applies an editing key. Returns true if the key was consumed.
    pub fn handle(&mut self, key: Key) -> bool {
        match key {
            Key::Char(c) if !c.is_control() => {
                if self.chars.len() < self.max_len {
                    self.chars.insert(self.cursor, c);
                    self.cursor += 1;
                }
            }
            Key::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    self.chars.remove(self.cursor);
                }
            }
            Key::Delete => {
                if self.cursor < self.chars.len() {
                    self.chars.remove(self.cursor);
                }
            }
            Key::Left => self.cursor = self.cursor.saturating_sub(1),
            Key::Right => self.cursor = (self.cursor + 1).min(self.chars.len()),
            Key::Home | Key::Ctrl('a') => self.cursor = 0,
            Key::End | Key::Ctrl('e') => self.cursor = self.chars.len(),
            Key::Ctrl('u') => {
                self.chars.drain(..self.cursor);
                self.cursor = 0;
            }
            _ => return false,
        }
        true
    }

    /// Draws the field in a bordered box. When `focused`, also places the
    /// terminal cursor at the edit position.
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
        let inner_width = area.width.saturating_sub(2) as usize;

        // Scroll horizontally so the cursor is always visible.
        let start = (self.cursor + 1).saturating_sub(inner_width.max(1));
        let shown: String = self
            .chars
            .iter()
            .skip(start)
            .take(inner_width)
            .map(|&c| if self.masked { '*' } else { c })
            .collect();
        frame.render_widget(Paragraph::new(shown).block(block), area);

        if focused && area.width > 2 && area.height > 2 {
            let x = area.x + 1 + (self.cursor - start).min(inner_width.saturating_sub(1)) as u16;
            frame.set_cursor_position((x, area.y + 1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_arrows_and_split_sequences() {
        let mut p = KeyParser::default();
        assert_eq!(p.feed(b"\x1b[A\x1b[B"), vec![Key::Up, Key::Down]);
        assert_eq!(p.feed(b"\x1b["), vec![]);
        assert_eq!(p.feed(b"C"), vec![Key::Right]);
        assert_eq!(p.feed(b"\x1b"), vec![Key::Esc]);
        assert_eq!(p.feed(b"\x1bOA"), vec![Key::Up]);
        assert_eq!(p.feed(b"\x1b[3~"), vec![Key::Delete]);
        assert_eq!(p.feed(b"\x1b[5~\x1b[6~"), vec![Key::PageUp, Key::PageDown]);
    }

    #[test]
    fn parses_text_and_controls() {
        let mut p = KeyParser::default();
        assert_eq!(
            p.feed("aä\r\n\x7f\x03".as_bytes()),
            vec![
                Key::Char('a'),
                Key::Char('ä'),
                Key::Enter,
                Key::Backspace,
                Key::Ctrl('c')
            ]
        );
        // multi-byte char split across reads
        let bytes = "€".as_bytes();
        assert_eq!(p.feed(&bytes[..1]), vec![]);
        assert_eq!(p.feed(&bytes[1..]), vec![Key::Char('€')]);
    }

    #[test]
    fn text_input_edits() {
        let mut t = TextInput::new(4, false);
        for c in "abcde".chars() {
            t.handle(Key::Char(c));
        }
        assert_eq!(t.value(), "abcd"); // max_len enforced
        t.handle(Key::Left);
        t.handle(Key::Backspace);
        assert_eq!(t.value(), "abd");
    }
}
