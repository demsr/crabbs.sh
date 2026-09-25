use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use super::compose::TextArea;
use super::input::Key;
use crate::content;

const HIGHLIGHT: Style = Style::new()
    .bg(Color::Blue)
    .fg(Color::White)
    .add_modifier(Modifier::BOLD);
const DIM: Style = Style::new().fg(Color::Gray);

// ------------------------------------------------------------- hub menu

#[derive(Clone, Copy, PartialEq, Eq)]
enum Item {
    Motd,
}

const ITEMS: &[Item] = &[Item::Motd];

impl Item {
    fn label(self) -> &'static str {
        match self {
            Item::Motd => "Edit message of the day",
        }
    }
}

pub enum SysopMenuEvent {
    None,
    Back,
    EditMotd,
}

pub struct SysopMenu {
    selected: usize,
}

impl SysopMenu {
    pub fn new() -> Self {
        Self { selected: 0 }
    }

    pub fn handle(&mut self, key: Key) -> SysopMenuEvent {
        match key {
            Key::Esc | Key::Char('q') => return SysopMenuEvent::Back,
            Key::Up | Key::Char('k') => {
                self.selected = (self.selected + ITEMS.len() - 1) % ITEMS.len();
            }
            Key::Down | Key::Char('j') => self.selected = (self.selected + 1) % ITEMS.len(),
            Key::Enter => match ITEMS[self.selected] {
                Item::Motd => return SysopMenuEvent::EditMotd,
            },
            _ => {}
        }
        SysopMenuEvent::None
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .split(area);
        let items: Vec<ListItem> = ITEMS
            .iter()
            .map(|i| ListItem::new(i.label()))
            .collect();
        let list = List::new(items)
            .block(Block::default().title("Sysop tools").borders(Borders::ALL))
            .highlight_style(HIGHLIGHT)
            .highlight_symbol("> ");
        let mut state = ListState::default();
        state.select(Some(self.selected));
        frame.render_stateful_widget(list, rows[0], &mut state);
        frame.render_widget(
            Paragraph::new("Enter: open · ↑/↓: move · Esc: back").style(DIM),
            rows[1],
        );
    }
}

// ------------------------------------------------------------ motd editor

pub enum MotdEvent {
    None,
    Cancel,
    Save(String),
}

pub struct MotdEditor {
    body: TextArea,
    loaded: bool,
    error: Option<String>,
    busy: bool,
}

impl MotdEditor {
    pub fn loading() -> Self {
        Self {
            body: TextArea::new(content::MOTD_MAX),
            loaded: false,
            error: None,
            busy: false,
        }
    }

    /// Loads the current text from the server. Called once, when it arrives.
    pub fn set_text(&mut self, text: &str) {
        self.body = TextArea::new(content::MOTD_MAX);
        for c in text.chars() {
            self.body.handle(if c == '\n' { Key::Enter } else { Key::Char(c) });
        }
        self.loaded = true;
        self.busy = false;
    }

    pub fn fail(&mut self, message: String) {
        self.busy = false;
        self.error = Some(message);
    }

    pub fn handle(&mut self, key: Key) -> MotdEvent {
        if !self.loaded || self.busy {
            return MotdEvent::None;
        }
        match key {
            Key::Esc => MotdEvent::Cancel,
            Key::Ctrl('d') => {
                self.error = None;
                self.busy = true;
                MotdEvent::Save(self.body.value())
            }
            other => {
                self.body.handle(other);
                MotdEvent::None
            }
        }
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(4), Constraint::Length(2)])
            .split(area);
        frame.render_widget(
            Paragraph::new("Shown once to everyone (guests included) right after they log in.")
                .wrap(Wrap { trim: true })
                .style(DIM),
            rows[0],
        );
        self.body.render(frame, rows[1], "Message of the day", self.loaded);

        let (text, color) = if !self.loaded {
            ("Loading…".to_string(), Color::Yellow)
        } else if self.busy {
            ("Saving…".to_string(), Color::Yellow)
        } else if let Some(err) = &self.error {
            (err.clone(), Color::Red)
        } else {
            (
                "Ctrl-D: save · Esc: cancel · clear the text and save to remove the MOTD"
                    .to_string(),
                Color::Gray,
            )
        };
        frame.render_widget(
            Paragraph::new(text).wrap(Wrap { trim: true }).style(Style::new().fg(color)),
            rows[2],
        );
    }
}
