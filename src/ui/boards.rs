use std::sync::atomic::{AtomicUsize, Ordering};

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use super::input::Key;
use super::text::wrap;
use super::{BoardInfo, ThreadDetail, ThreadInfo};

/// Interior-mutable counter with `Cell`'s API. Drawing only has `&self`, but
/// needs to record layout results; unlike `Cell` this keeps `App` `Sync`,
/// which the SSH handler's async methods require.
struct Counter(AtomicUsize);

impl Counter {
    fn new(v: usize) -> Self {
        Self(AtomicUsize::new(v))
    }
    fn get(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
    fn set(&self, v: usize) {
        self.0.store(v, Ordering::Relaxed);
    }
}

const HIGHLIGHT: Style = Style::new()
    .bg(Color::Blue)
    .fg(Color::White)
    .add_modifier(Modifier::BOLD);
const DIM: Style = Style::new().fg(Color::Gray);

fn move_selection(selected: &mut usize, len: usize, key: Key) {
    match key {
        Key::Up | Key::Char('k') => *selected = selected.saturating_sub(1),
        Key::Down | Key::Char('j') => *selected = (*selected + 1).min(len.saturating_sub(1)),
        Key::Home | Key::Char('g') => *selected = 0,
        Key::End | Key::Char('G') => *selected = len.saturating_sub(1),
        Key::PageUp => *selected = selected.saturating_sub(10),
        Key::PageDown => *selected = (*selected + 10).min(len.saturating_sub(1)),
        _ => {}
    }
}

fn draw_help(frame: &mut Frame, area: Rect, text: &str) {
    frame.render_widget(Paragraph::new(text).style(DIM), area);
}

fn split_help(area: Rect) -> (Rect, Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(1)])
        .split(area);
    (rows[0], rows[1])
}

// ---------------------------------------------------------------- boards

pub enum BoardsEvent {
    None,
    Back,
    Open(i64),
}

pub struct BoardList {
    boards: Vec<BoardInfo>,
    selected: usize,
    loaded: bool,
}

impl BoardList {
    pub fn new() -> Self {
        Self {
            boards: Vec::new(),
            selected: 0,
            loaded: false,
        }
    }

    pub fn set(&mut self, boards: Vec<BoardInfo>) {
        self.boards = boards;
        self.loaded = true;
        self.selected = self.selected.min(self.boards.len().saturating_sub(1));
    }

    pub fn handle(&mut self, key: Key) -> BoardsEvent {
        match key {
            Key::Esc | Key::Char('q') => return BoardsEvent::Back,
            Key::Enter => {
                if let Some(board) = self.boards.get(self.selected) {
                    return BoardsEvent::Open(board.id);
                }
            }
            other => move_selection(&mut self.selected, self.boards.len(), other),
        }
        BoardsEvent::None
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let (body, help) = split_help(area);
        let items: Vec<ListItem> = if !self.loaded {
            vec![ListItem::new("Loading…")]
        } else if self.boards.is_empty() {
            vec![ListItem::new("(no boards)")]
        } else {
            self.boards
                .iter()
                .map(|b| {
                    ListItem::new(Line::from(vec![
                        Span::raw(format!("{:<14}", b.name)),
                        Span::styled(
                            format!("{}  ({} threads)", b.description, b.thread_count),
                            DIM,
                        ),
                    ]))
                })
                .collect()
        };
        let list = List::new(items)
            .block(
                Block::default()
                    .title("Message boards")
                    .borders(Borders::ALL),
            )
            .highlight_style(HIGHLIGHT)
            .highlight_symbol("> ");
        let mut state = ListState::default();
        if !self.boards.is_empty() {
            state.select(Some(self.selected));
        }
        frame.render_stateful_widget(list, body, &mut state);
        draw_help(frame, help, "Enter: open board · ↑/↓: move · Esc: back");
    }
}

// --------------------------------------------------------------- threads

pub enum ThreadsEvent {
    None,
    Back,
    DeleteThread { thread_id: i64 },
    Open { board_id: i64, thread_id: i64 },
    New { board_id: i64 },
}

pub struct ThreadList {
    board_id: i64,
    board_name: String,
    threads: Vec<ThreadInfo>,
    selected: usize,
    loaded: bool,
    /// Shows moderation keys. Purely cosmetic: the server decides.
    sysop: bool,
    confirm_delete: bool,
}

impl ThreadList {
    pub fn loading(board_id: i64, sysop: bool) -> Self {
        Self {
            sysop,
            confirm_delete: false,
            board_id,
            board_name: String::new(),
            threads: Vec::new(),
            selected: 0,
            loaded: false,
        }
    }

    pub fn set(&mut self, board_name: String, threads: Vec<ThreadInfo>) {
        self.board_name = board_name;
        self.threads = threads;
        self.loaded = true;
        self.selected = self.selected.min(self.threads.len().saturating_sub(1));
    }

    pub fn handle(&mut self, key: Key) -> ThreadsEvent {
        if self.confirm_delete {
            self.confirm_delete = false;
            if let (Key::Char('y'), Some(thread)) = (key, self.threads.get(self.selected)) {
                return ThreadsEvent::DeleteThread {
                    thread_id: thread.id,
                };
            }
            return ThreadsEvent::None;
        }
        match key {
            Key::Char('x') if self.sysop && !self.threads.is_empty() => {
                self.confirm_delete = true;
            }
            Key::Esc | Key::Char('q') => return ThreadsEvent::Back,
            Key::Char('n') => {
                return ThreadsEvent::New {
                    board_id: self.board_id,
                };
            }
            Key::Enter => {
                if let Some(thread) = self.threads.get(self.selected) {
                    return ThreadsEvent::Open {
                        board_id: self.board_id,
                        thread_id: thread.id,
                    };
                }
            }
            other => move_selection(&mut self.selected, self.threads.len(), other),
        }
        ThreadsEvent::None
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let (body, help) = split_help(area);
        let items: Vec<ListItem> = if !self.loaded {
            vec![ListItem::new("Loading…")]
        } else if self.threads.is_empty() {
            vec![ListItem::new("(no threads yet — press 'n' to start one)")]
        } else {
            self.threads
                .iter()
                .map(|t| {
                    let replies = t.post_count.saturating_sub(1);
                    ListItem::new(Line::from(vec![
                        Span::raw(t.title.clone()),
                        Span::styled(
                            format!("  by {} · {replies} replies · {}", t.author, t.last_post),
                            DIM,
                        ),
                    ]))
                })
                .collect()
        };
        let list = List::new(items)
            .block(
                Block::default()
                    .title(format!("Board: {}", self.board_name))
                    .borders(Borders::ALL),
            )
            .highlight_style(HIGHLIGHT)
            .highlight_symbol("> ");
        let mut state = ListState::default();
        if !self.threads.is_empty() {
            state.select(Some(self.selected));
        }
        frame.render_stateful_widget(list, body, &mut state);
        if self.confirm_delete {
            let title = self
                .threads
                .get(self.selected)
                .map_or("", |t| t.title.as_str());
            frame.render_widget(
                Paragraph::new(format!(
                    "Delete thread \"{title}\" and all its posts? y = yes, any other key = no"
                ))
                .style(Style::new().fg(Color::Yellow)),
                help,
            );
        } else if self.sysop {
            draw_help(
                frame,
                help,
                "Enter: read · n: new thread · x: delete thread (sysop) · Esc: back",
            );
        } else {
            draw_help(
                frame,
                help,
                "Enter: read · n: new thread · ↑/↓: move · Esc: back",
            );
        }
    }
}

// ----------------------------------------------------------- thread view

pub enum ThreadEvent {
    None,
    DeletePost { post_id: i64 },
    Back { board_id: i64 },
    Reply { thread_id: i64, title: String },
}

pub struct ThreadView {
    board_id: i64,
    thread_id: i64,
    board_name: String,
    title: String,
    posts: Vec<super::PostInfo>,
    loaded: bool,
    sysop: bool,
    /// Digits typed so far while asking which post to delete.
    delete_prompt: Option<String>,
    /// First visible line. A `Counter` (interior mutability) because drawing clamps it to the
    /// content height, which is only known at draw time.
    scroll: Counter,
    total_lines: Counter,
    page_height: Counter,
}

impl ThreadView {
    pub fn loading(board_id: i64, thread_id: i64, sysop: bool) -> Self {
        Self {
            sysop,
            delete_prompt: None,
            board_id,
            thread_id,
            board_name: String::new(),
            title: String::new(),
            posts: Vec::new(),
            loaded: false,
            scroll: Counter::new(0),
            total_lines: Counter::new(0),
            page_height: Counter::new(10),
        }
    }

    pub fn from_detail(detail: ThreadDetail, to_end: bool, sysop: bool) -> Self {
        let mut view = Self::loading(detail.board_id, detail.id, sysop);
        view.board_name = detail.board_name;
        view.title = detail.title;
        view.posts = detail.posts;
        view.loaded = true;
        if to_end {
            // Clamped to the real maximum on the next draw.
            view.scroll.set(usize::MAX);
        }
        view
    }

    fn scroll_by(&self, delta: isize) {
        let max = self
            .total_lines
            .get()
            .saturating_sub(self.page_height.get());
        let now = self.scroll.get().min(max);
        self.scroll.set(now.saturating_add_signed(delta).min(max));
    }

    fn handle_delete_prompt(&mut self, mut input: String, key: Key) -> ThreadEvent {
        match key {
            Key::Esc => return ThreadEvent::None,
            Key::Enter => {
                let number: usize = input.parse().unwrap_or(0);
                return match number.checked_sub(1).and_then(|i| self.posts.get(i)) {
                    Some(post) => ThreadEvent::DeletePost { post_id: post.id },
                    None => ThreadEvent::None,
                };
            }
            Key::Backspace => {
                input.pop();
            }
            Key::Char(c) if c.is_ascii_digit() && input.len() < 6 => input.push(c),
            _ => {}
        }
        self.delete_prompt = Some(input);
        ThreadEvent::None
    }

    pub fn handle(&mut self, key: Key) -> ThreadEvent {
        if let Some(input) = self.delete_prompt.take() {
            return self.handle_delete_prompt(input, key);
        }
        let page = self.page_height.get().saturating_sub(1).max(1) as isize;
        match key {
            Key::Esc | Key::Char('q') => {
                return ThreadEvent::Back {
                    board_id: self.board_id,
                };
            }
            Key::Char('x') if self.sysop && self.loaded => {
                self.delete_prompt = Some(String::new());
            }
            Key::Char('r') if self.loaded => {
                return ThreadEvent::Reply {
                    thread_id: self.thread_id,
                    title: self.title.clone(),
                };
            }
            Key::Up | Key::Char('k') => self.scroll_by(-1),
            Key::Down | Key::Char('j') | Key::Enter => self.scroll_by(1),
            Key::PageUp | Key::Char('b') => self.scroll_by(-page),
            Key::PageDown | Key::Char(' ') => self.scroll_by(page),
            Key::Home | Key::Char('g') => self.scroll.set(0),
            Key::End | Key::Char('G') => self.scroll.set(usize::MAX),
            _ => {}
        }
        ThreadEvent::None
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let (body, help) = split_help(area);
        let block = Block::default()
            .title(format!("{} › {}", self.board_name, self.title))
            .borders(Borders::ALL);
        let inner = block.inner(body);

        let mut lines: Vec<Line> = Vec::new();
        if !self.loaded {
            lines.push(Line::from("Loading…"));
        }
        for (i, post) in self.posts.iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("#{} {}", i + 1, post.author),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    if post.author_is_sysop { " [sysop]" } else { "" },
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(format!("  {}", post.created), DIM),
            ]));
            lines.extend(
                wrap(&post.body, inner.width as usize)
                    .into_iter()
                    .map(Line::from),
            );
            lines.push(Line::default());
        }

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
        if let Some(input) = &self.delete_prompt {
            let label = "Delete which post number? ";
            frame.render_widget(
                Paragraph::new(format!("{label}{input}   (Enter: delete · Esc: cancel)"))
                    .style(Style::new().fg(Color::Yellow)),
                help,
            );
            frame.set_cursor_position((help.x + (label.len() + input.len()) as u16, help.y));
        } else if self.sysop {
            draw_help(
                frame,
                help,
                "↑/↓/PgUp/PgDn: scroll · r: reply · x: delete post (sysop) · Esc: back",
            );
        } else {
            draw_help(
                frame,
                help,
                "↑/↓/PgUp/PgDn: scroll · r: reply · Esc: back to threads",
            );
        }
    }
}
