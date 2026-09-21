use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use super::KeyInfo;
use super::input::{Key, TextInput};
use crate::db::MAX_KEYS_PER_USER;

/// Long enough for an RSA-4096 public key line.
const MAX_LINE: usize = 4096;

enum Mode {
    List,
    Adding(TextInput),
    ConfirmDelete,
}

pub enum KeysEvent {
    None,
    Back,
    Add(String),
    Delete(i64),
}

pub struct KeysScreen {
    keys: Vec<KeyInfo>,
    selected: usize,
    mode: Mode,
}

impl KeysScreen {
    pub fn new() -> Self {
        Self {
            keys: Vec::new(),
            selected: 0,
            mode: Mode::List,
        }
    }

    /// Replaces the list with fresh data from the server. `ok` is false when
    /// the last add/delete failed, in which case an open "add" box stays open
    /// so the user can correct their input.
    pub fn update(&mut self, keys: Vec<KeyInfo>, ok: bool) {
        self.keys = keys;
        self.selected = self.selected.min(self.keys.len().saturating_sub(1));
        if ok {
            self.mode = Mode::List;
        }
    }

    pub fn handle(&mut self, key: Key) -> KeysEvent {
        match &mut self.mode {
            Mode::List => match key {
                Key::Esc | Key::Char('q') => return KeysEvent::Back,
                Key::Up | Key::Char('k') => {
                    self.selected = self.selected.saturating_sub(1);
                }
                Key::Down | Key::Char('j') => {
                    self.selected = (self.selected + 1).min(self.keys.len().saturating_sub(1));
                }
                Key::Char('a') => self.mode = Mode::Adding(TextInput::new(MAX_LINE, false)),
                Key::Char('d') | Key::Delete if !self.keys.is_empty() => {
                    self.mode = Mode::ConfirmDelete;
                }
                _ => {}
            },
            Mode::Adding(input) => match key {
                Key::Esc => self.mode = Mode::List,
                Key::Enter if !input.is_empty() => return KeysEvent::Add(input.value()),
                other => {
                    input.handle(other);
                }
            },
            Mode::ConfirmDelete => {
                let target = self.keys.get(self.selected).map(|k| k.id);
                self.mode = Mode::List;
                if let (Key::Char('y'), Some(id)) = (key, target) {
                    return KeysEvent::Delete(id);
                }
            }
        }
        KeysEvent::None
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(3)])
            .split(area);

        let items: Vec<ListItem> = if self.keys.is_empty() {
            vec![ListItem::new("(no keys yet — press 'a' to add one)")]
        } else {
            self.keys
                .iter()
                .map(|k| {
                    let comment = if k.comment.is_empty() {
                        "-"
                    } else {
                        &k.comment
                    };
                    ListItem::new(format!("{}  {}", k.fingerprint, comment))
                })
                .collect()
        };
        let list = List::new(items)
            .block(
                Block::default()
                    .title(format!(
                        "Your SSH public keys ({}/{MAX_KEYS_PER_USER})",
                        self.keys.len()
                    ))
                    .borders(Borders::ALL),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("> ");
        let mut state = ListState::default();
        if !self.keys.is_empty() {
            state.select(Some(self.selected));
        }
        frame.render_stateful_widget(list, rows[0], &mut state);

        match &self.mode {
            Mode::List => {
                let help = "a: add key · d: delete selected · ↑/↓: move · Esc: back\n\
                            Keys let you log in as your user without a password.";
                frame.render_widget(
                    Paragraph::new(help)
                        .wrap(Wrap { trim: true })
                        .style(Style::default().fg(Color::Gray)),
                    rows[1],
                );
            }
            Mode::Adding(input) => {
                input.render(
                    frame,
                    rows[1],
                    "Paste public key (e.g. contents of ~/.ssh/id_ed25519.pub) · Enter: save · Esc: cancel",
                    true,
                );
            }
            Mode::ConfirmDelete => {
                frame.render_widget(
                    Paragraph::new(
                        "Delete the selected key? Press 'y' to confirm, any other key to cancel.",
                    )
                    .style(Style::default().fg(Color::Yellow)),
                    rows[1],
                );
            }
        }
    }
}
