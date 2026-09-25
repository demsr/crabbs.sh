use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};

use super::input::{Key, TextInput};
use super::text::{Counter, wrap};
use super::{BoardInfo, PageTarget, ThreadDetail, ThreadInfo};
use crate::content;
use crate::db::POSTS_PER_PAGE;

const HIGHLIGHT: Style = Style::new()
    .bg(Color::Blue)
    .fg(Color::White)
    .add_modifier(Modifier::BOLD);
const NEW: Style = Style::new()
    .fg(Color::Green)
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
    CreateBoard { name: String, description: String },
    UpdateDescription { board_id: i64, description: String },
}

/// What a board create/edit form does when Enter is pressed on its last
/// field or Esc is pressed.
enum FormOutcome {
    None,
    Cancel,
    Submit,
}

/// "Create a board": name + description. Fields only; validation and the
/// actual request are assembled by `BoardList`, which knows which event to
/// return.
struct NewBoardForm {
    name: TextInput,
    description: TextInput,
    focus: usize,
    error: Option<String>,
    busy: bool,
}

impl NewBoardForm {
    fn new() -> Self {
        Self {
            name: TextInput::new(content::TITLE_MAX, false),
            description: TextInput::new(content::TITLE_MAX, false),
            focus: 0,
            error: None,
            busy: false,
        }
    }

    fn field(&mut self) -> &mut TextInput {
        match self.focus {
            0 => &mut self.name,
            _ => &mut self.description,
        }
    }

    fn fail(&mut self, message: String) {
        self.busy = false;
        self.error = Some(message);
    }

    fn handle(&mut self, key: Key) -> FormOutcome {
        if self.busy {
            return FormOutcome::None;
        }
        match key {
            Key::Esc => return FormOutcome::Cancel,
            Key::Tab | Key::Down | Key::BackTab | Key::Up => self.focus = 1 - self.focus,
            Key::Enter if self.focus == 0 => self.focus = 1,
            Key::Enter => {
                if self.name.value().trim().is_empty() {
                    self.focus = 0;
                    self.error = Some("Give the board a name.".into());
                    return FormOutcome::None;
                }
                self.error = None;
                self.busy = true;
                return FormOutcome::Submit;
            }
            other => {
                self.field().handle(other);
            }
        }
        FormOutcome::None
    }

    fn draw(&self, frame: &mut Frame, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Length(3), Constraint::Length(2)])
            .split(area);
        self.name.render(frame, rows[0], "Board name", self.focus == 0);
        self.description
            .render(frame, rows[1], "Description (optional)", self.focus == 1);
        let (text, color) = if self.busy {
            ("Creating…".to_string(), Color::Yellow)
        } else if let Some(err) = &self.error {
            (err.clone(), Color::Red)
        } else {
            ("Tab/Enter: next field · Enter on description: create · Esc: cancel".to_string(), Color::Gray)
        };
        frame.render_widget(Paragraph::new(text).style(Style::new().fg(color)), rows[2]);
    }
}

/// "Edit board description": one field, prefilled.
struct EditBoardForm {
    board_id: i64,
    board_name: String,
    description: TextInput,
    error: Option<String>,
    busy: bool,
}

impl EditBoardForm {
    fn new(board_id: i64, board_name: String, current_description: &str) -> Self {
        let mut description = TextInput::new(content::TITLE_MAX, false);
        description.set(current_description);
        Self {
            board_id,
            board_name,
            description,
            error: None,
            busy: false,
        }
    }

    fn fail(&mut self, message: String) {
        self.busy = false;
        self.error = Some(message);
    }

    fn handle(&mut self, key: Key) -> FormOutcome {
        if self.busy {
            return FormOutcome::None;
        }
        match key {
            Key::Esc => return FormOutcome::Cancel,
            Key::Enter => {
                self.error = None;
                self.busy = true;
                return FormOutcome::Submit;
            }
            other => {
                self.description.handle(other);
            }
        }
        FormOutcome::None
    }

    fn draw(&self, frame: &mut Frame, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(3), Constraint::Length(2)])
            .split(area);
        self.description.render(
            frame,
            rows[0],
            &format!("Description of \"{}\"", self.board_name),
            true,
        );
        let (text, color) = if self.busy {
            ("Saving…".to_string(), Color::Yellow)
        } else if let Some(err) = &self.error {
            (err.clone(), Color::Red)
        } else {
            ("Enter: save · Esc: cancel".to_string(), Color::Gray)
        };
        frame.render_widget(Paragraph::new(text).style(Style::new().fg(color)), rows[1]);
    }
}

enum Mode {
    List,
    New(NewBoardForm),
    Edit(EditBoardForm),
}

pub struct BoardList {
    boards: Vec<BoardInfo>,
    selected: usize,
    loaded: bool,
    /// Shows board-management keys. Purely cosmetic: the server decides.
    sysop: bool,
    mode: Mode,
}

impl BoardList {
    pub fn new(sysop: bool) -> Self {
        Self {
            boards: Vec::new(),
            selected: 0,
            loaded: false,
            sysop,
            mode: Mode::List,
        }
    }

    pub fn set(&mut self, boards: Vec<BoardInfo>) {
        self.boards = boards;
        self.loaded = true;
        self.selected = self.selected.min(self.boards.len().saturating_sub(1));
        // A fresh list means whatever create/edit action was pending (if
        // any) has already been handled - either it succeeded and this is
        // the result, or something else refreshed the list.
        self.mode = Mode::List;
    }

    /// The server rejected a create/edit; let the open form show why.
    pub fn fail(&mut self, message: String) {
        match &mut self.mode {
            Mode::New(form) => form.fail(message),
            Mode::Edit(form) => form.fail(message),
            Mode::List => {}
        }
    }

    pub fn handle(&mut self, key: Key) -> BoardsEvent {
        match &mut self.mode {
            Mode::List => self.handle_list(key),
            Mode::New(form) => match form.handle(key) {
                FormOutcome::None => BoardsEvent::None,
                FormOutcome::Cancel => {
                    self.mode = Mode::List;
                    BoardsEvent::None
                }
                FormOutcome::Submit => {
                    let Mode::New(form) = &self.mode else {
                        unreachable!()
                    };
                    BoardsEvent::CreateBoard {
                        name: form.name.value(),
                        description: form.description.value(),
                    }
                }
            },
            Mode::Edit(form) => match form.handle(key) {
                FormOutcome::None => BoardsEvent::None,
                FormOutcome::Cancel => {
                    self.mode = Mode::List;
                    BoardsEvent::None
                }
                FormOutcome::Submit => {
                    let Mode::Edit(form) = &self.mode else {
                        unreachable!()
                    };
                    BoardsEvent::UpdateDescription {
                        board_id: form.board_id,
                        description: form.description.value(),
                    }
                }
            },
        }
    }

    fn handle_list(&mut self, key: Key) -> BoardsEvent {
        match key {
            Key::Esc | Key::Char('q') => return BoardsEvent::Back,
            Key::Enter => {
                if let Some(board) = self.boards.get(self.selected) {
                    return BoardsEvent::Open(board.id);
                }
            }
            Key::Char('n') if self.sysop => self.mode = Mode::New(NewBoardForm::new()),
            Key::Char('e') if self.sysop => {
                if let Some(board) = self.boards.get(self.selected) {
                    self.mode = Mode::Edit(EditBoardForm::new(
                        board.id,
                        board.name.clone(),
                        &board.description,
                    ));
                }
            }
            other => move_selection(&mut self.selected, self.boards.len(), other),
        }
        BoardsEvent::None
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        match &self.mode {
            Mode::New(form) => {
                form.draw(frame, area);
                return;
            }
            Mode::Edit(form) => {
                form.draw(frame, area);
                return;
            }
            Mode::List => {}
        }

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
                        Span::styled(
                            if b.unread_threads > 0 {
                                format!("  {} unread", b.unread_threads)
                            } else {
                                String::new()
                            },
                            NEW,
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
        if self.sysop {
            draw_help(
                frame,
                help,
                "Enter: open board · n: new board · e: edit description (sysop) · ↑/↓: move · Esc: back",
            );
        } else {
            draw_help(frame, help, "Enter: open board · ↑/↓: move · Esc: back");
        }
    }
}

// --------------------------------------------------------------- threads

pub enum ThreadsEvent {
    None,
    Back,
    DeleteThread { thread_id: i64, page: usize },
    /// `list_page` is the list page we came from, so Back returns to it.
    Open { board_id: i64, thread_id: i64, list_page: usize },
    New { board_id: i64 },
    MarkBoardRead { board_id: i64, page: usize },
    /// Show another page of this board's thread list.
    Page { board_id: i64, page: usize },
}

pub struct ThreadList {
    board_id: i64,
    board_name: String,
    threads: Vec<ThreadInfo>,
    selected: usize,
    loaded: bool,
    /// Shows moderation keys. Purely cosmetic: the server decides.
    sysop: bool,
    /// Logged-in user (guests have no read markers).
    member: bool,
    confirm_delete: bool,
    /// 0-based page, page count and total threads on the board.
    page: usize,
    pages: usize,
    total: usize,
}

impl ThreadList {
    pub fn loading(board_id: i64, sysop: bool, member: bool) -> Self {
        Self {
            sysop,
            member,
            confirm_delete: false,
            page: 0,
            pages: 1,
            total: 0,
            board_id,
            board_name: String::new(),
            threads: Vec::new(),
            selected: 0,
            loaded: false,
        }
    }

    pub fn set(
        &mut self,
        board_name: String,
        threads: Vec<ThreadInfo>,
        page: usize,
        pages: usize,
        total: usize,
    ) {
        self.board_name = board_name;
        self.threads = threads;
        self.page = page;
        self.pages = pages;
        self.total = total;
        self.loaded = true;
        self.selected = self.selected.min(self.threads.len().saturating_sub(1));
    }

    pub fn handle(&mut self, key: Key) -> ThreadsEvent {
        if self.confirm_delete {
            self.confirm_delete = false;
            if let (Key::Char('y'), Some(thread)) = (key, self.threads.get(self.selected)) {
                return ThreadsEvent::DeleteThread {
                    thread_id: thread.id,
                    page: self.page,
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
            Key::Char('m') if self.member => {
                return ThreadsEvent::MarkBoardRead {
                    board_id: self.board_id,
                    page: self.page,
                };
            }
            Key::Enter => {
                if let Some(thread) = self.threads.get(self.selected) {
                    return ThreadsEvent::Open {
                        board_id: self.board_id,
                        thread_id: thread.id,
                        list_page: self.page,
                    };
                }
            }
            // Page keys, plus PgDn/PgUp continuing past the ends of a page.
            Key::Right | Key::Char('>') | Key::Char('.') => return self.goto(self.page + 1),
            Key::Left | Key::Char('<') | Key::Char(',') => {
                return self.goto(self.page.wrapping_sub(1));
            }
            Key::PageDown if self.selected + 1 >= self.threads.len() => {
                return self.goto(self.page + 1);
            }
            Key::PageUp if self.selected == 0 => return self.goto(self.page.wrapping_sub(1)),
            other => move_selection(&mut self.selected, self.threads.len(), other),
        }
        ThreadsEvent::None
    }

    /// Asks for another page if it exists (`usize::MAX` from a wrapped
    /// subtraction counts as "before the first").
    fn goto(&self, page: usize) -> ThreadsEvent {
        if page < self.pages && page != self.page {
            ThreadsEvent::Page {
                board_id: self.board_id,
                page,
            }
        } else {
            ThreadsEvent::None
        }
    }

    fn title(&self) -> String {
        if self.pages > 1 {
            format!(
                "Board: {} · page {}/{} · {} threads",
                self.board_name,
                self.page + 1,
                self.pages,
                self.total
            )
        } else {
            format!("Board: {} · {} threads", self.board_name, self.total)
        }
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
                    let unread = t.unread > 0;
                    ListItem::new(Line::from(vec![
                        Span::styled(if unread { "* " } else { "  " }, NEW),
                        Span::styled(
                            t.title.clone(),
                            if unread {
                                Style::new().add_modifier(Modifier::BOLD)
                            } else {
                                Style::new()
                            },
                        ),
                        Span::styled(
                            format!("  by {} · {replies} replies · {}", t.author, t.last_post),
                            DIM,
                        ),
                        Span::styled(
                            if unread {
                                format!("  [{} new]", t.unread)
                            } else {
                                String::new()
                            },
                            NEW,
                        ),
                    ]))
                })
                .collect()
        };
        let list = List::new(items)
            .block(
                Block::default()
                    .title(self.title())
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
        } else {
            let mut text = String::from("Enter: read · n: new thread");
            if self.pages > 1 {
                text.push_str(" · ←/→: page");
            }
            if self.member {
                text.push_str(" · m: mark all read");
            }
            if self.sysop {
                text.push_str(" · x: delete thread (sysop)");
            }
            text.push_str(" · Esc: back");
            draw_help(frame, help, &text);
        }
    }
}

// ----------------------------------------------------------- thread view

pub enum ThreadEvent {
    None,
    DeletePost { post_id: i64, page: usize },
    Back { board_id: i64, list_page: usize },
    Reply {
        thread_id: i64,
        title: String,
        /// The post being replied to (author, when, body) - the one marked
        /// with "> " in the view, i.e. whichever is at the top of what's
        /// currently on screen. `None` for a reply with no quote at all.
        quote: Option<(String, String, String)>,
    },
    /// Show another page of this thread.
    Goto { thread_id: i64, target: PageTarget },
}

pub struct ThreadView {
    board_id: i64,
    thread_id: i64,
    board_name: String,
    title: String,
    posts: Vec<super::PostInfo>,
    loaded: bool,
    /// 0-based page of the thread being shown, page count, total posts.
    page: usize,
    pages: usize,
    total: usize,
    /// The thread-list page this thread was opened from (for Back).
    list_page: usize,
    sysop: bool,
    /// Digits typed so far while asking which post to delete.
    delete_prompt: Option<String>,
    /// First visible line. A `Counter` (interior mutability) because drawing clamps it to the
    /// content height, which is only known at draw time.
    scroll: Counter,
    total_lines: Counter,
    page_height: Counter,
    /// Post (1-based index) to scroll to on the next draw, 0 for none. Set
    /// when opening a thread with unread posts.
    jump: Counter,
    /// Index into `posts` of whichever post is at (or just above) the top
    /// of the current view - the one 'r' quotes. Recomputed on every draw
    /// from the scroll position, so it tracks reading without needing a
    /// separate selection gesture.
    quote_index: Counter,
}

impl ThreadView {
    pub fn loading(board_id: i64, thread_id: i64, sysop: bool, list_page: usize) -> Self {
        Self {
            page: 0,
            pages: 1,
            total: 0,
            list_page,
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
            jump: Counter::new(0),
            quote_index: Counter::new(0),
        }
    }

    pub fn list_page(&self) -> usize {
        self.list_page
    }

    pub fn from_detail(detail: ThreadDetail, to_end: bool, sysop: bool, list_page: usize) -> Self {
        let mut view = Self::loading(detail.board_id, detail.id, sysop, list_page);
        view.page = detail.page;
        view.pages = detail.pages;
        view.total = detail.total;
        view.board_name = detail.board_name;
        view.title = detail.title;
        view.posts = detail.posts;
        view.loaded = true;
        if to_end {
            // Clamped to the real maximum on the next draw.
            view.scroll.set(usize::MAX);
        } else if let Some(first_unread) = view.posts.iter().position(|p| p.unread) {
            view.jump.set(first_unread + 1);
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

    /// Number of the first post on this page, minus one (posts are numbered
    /// across the whole thread, not per page).
    fn first_index(&self) -> usize {
        self.page * POSTS_PER_PAGE
    }

    fn max_scroll(&self) -> usize {
        self.total_lines.get().saturating_sub(self.page_height.get())
    }

    fn at_bottom(&self) -> bool {
        self.scroll.get().min(self.max_scroll()) == self.max_scroll()
    }

    fn at_top(&self) -> bool {
        self.scroll.get() == 0
    }

    fn goto(&self, target: PageTarget) -> ThreadEvent {
        ThreadEvent::Goto {
            thread_id: self.thread_id,
            target,
        }
    }

    fn handle_delete_prompt(&mut self, mut input: String, key: Key) -> ThreadEvent {
        match key {
            Key::Esc => return ThreadEvent::None,
            Key::Enter => {
                // The number is thread-wide; only posts on this page can be picked.
                let number: usize = input.parse().unwrap_or(0);
                let index = number.checked_sub(1 + self.first_index());
                return match index.and_then(|i| self.posts.get(i)) {
                    Some(post) => ThreadEvent::DeletePost {
                        post_id: post.id,
                        page: self.page,
                    },
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
                    list_page: self.list_page,
                };
            }
            Key::Char('x') if self.sysop && self.loaded => {
                self.delete_prompt = Some(String::new());
            }
            Key::Char('r') if self.loaded => {
                return ThreadEvent::Reply {
                    thread_id: self.thread_id,
                    title: self.title.clone(),
                    quote: self
                        .posts
                        .get(self.quote_index.get())
                        .map(|p| (p.author.clone(), p.created.clone(), p.body.clone())),
                };
            }
            // No quote at all, regardless of what's marked.
            Key::Char('R') if self.loaded => {
                return ThreadEvent::Reply {
                    thread_id: self.thread_id,
                    title: self.title.clone(),
                    quote: None,
                };
            }
            Key::Up | Key::Char('k') => self.scroll_by(-1),
            Key::Down | Key::Char('j') | Key::Enter => self.scroll_by(1),
            // Reading on past the end of a page continues on the next one.
            Key::PageDown | Key::Char(' ') => {
                if self.at_bottom() && self.page + 1 < self.pages {
                    return self.goto(PageTarget::Page(self.page + 1));
                }
                self.scroll_by(page);
            }
            Key::PageUp | Key::Char('b') => {
                if self.at_top() && self.page > 0 {
                    return self.goto(PageTarget::PageEnd(self.page - 1));
                }
                self.scroll_by(-page);
            }
            Key::Right | Key::Char('>') | Key::Char('.') if self.page + 1 < self.pages => {
                return self.goto(PageTarget::Page(self.page + 1));
            }
            Key::Left | Key::Char('<') | Key::Char(',') if self.page > 0 => {
                return self.goto(PageTarget::Page(self.page - 1));
            }
            // First / last page of the thread (or top / bottom if single-page).
            Key::Home | Key::Char('g') => {
                if self.page > 0 {
                    return self.goto(PageTarget::Page(0));
                }
                self.scroll.set(0);
            }
            Key::End | Key::Char('G') => {
                if self.page + 1 < self.pages {
                    return self.goto(PageTarget::Last);
                }
                self.scroll.set(usize::MAX);
            }
            _ => {}
        }
        ThreadEvent::None
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let (body, help) = split_help(area);
        let block = Block::default()
            .title(if self.pages > 1 {
                format!(
                    "{} › {} · page {}/{} · {} posts",
                    self.board_name,
                    self.title,
                    self.page + 1,
                    self.pages,
                    self.total
                )
            } else {
                format!("{} › {}", self.board_name, self.title)
            })
            .borders(Borders::ALL);
        let inner = block.inner(body);

        // First pass: wrap every post's body once, and use the resulting
        // line counts to work out where each post starts - needed both for
        // the existing scroll math and, new, to work out which post ends up
        // at the top of the view once scroll is settled.
        // Reserve two columns for a possible "> " marker up front, for every
        // post, not just whichever one turns out to be marked - so wrapping
        // doesn't reflow (and the scroll position it's keyed off doesn't
        // jump) as the marked post changes while scrolling, and a body line
        // that fills the reserved width never overflows the box once
        // marked.
        let wrap_width = (inner.width as usize).saturating_sub(2);
        let wrapped: Vec<Vec<String>> = self
            .posts
            .iter()
            .map(|p| wrap(&p.body, wrap_width))
            .collect();
        let mut post_starts: Vec<usize> = Vec::new();
        let mut total = usize::from(!self.loaded); // "Loading…" takes one line
        for w in &wrapped {
            post_starts.push(total);
            total += 1 + w.len() + 1; // header + body + trailing spacer
        }

        self.total_lines.set(total);
        self.page_height.set(inner.height as usize);
        let max = total.saturating_sub(inner.height as usize);
        // Opening a thread with unread posts starts at the first of them.
        if let Some(&start) = self.jump.get().checked_sub(1).and_then(|i| post_starts.get(i)) {
            self.scroll.set(start);
        }
        self.jump.set(0);
        let scroll = self.scroll.get().min(max);
        self.scroll.set(scroll);

        // Whichever post is at (or just above) that scroll position is the
        // one 'r' will quote; mark every one of its lines with "> " so it's
        // obvious which one that is, live as you scroll.
        let quote_index = post_starts.iter().rposition(|&start| start <= scroll);
        if let Some(i) = quote_index {
            self.quote_index.set(i);
        }

        // Second pass: the actual styled lines, now that we know which post
        // (if any) gets marked.
        let mut lines: Vec<Line> = Vec::new();
        if !self.loaded {
            lines.push(Line::from("Loading…"));
        }
        for (i, (post, body_lines)) in self.posts.iter().zip(&wrapped).enumerate() {
            let marked = quote_index == Some(i);
            let mark = |mut spans: Vec<Span<'static>>| -> Line<'static> {
                if marked {
                    let mut prefixed = vec![Span::styled("> ", Style::default().fg(Color::Cyan))];
                    prefixed.append(&mut spans);
                    Line::from(prefixed)
                } else {
                    Line::from(spans)
                }
            };
            lines.push(mark(vec![
                Span::styled(
                    format!("#{} {}", self.first_index() + i + 1, post.author),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    if post.author_is_sysop { " [sysop]" } else { "" },
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(format!("  {}", post.created), DIM),
                Span::styled(if post.unread { "  NEW" } else { "" }, NEW),
            ]));
            for line in body_lines {
                lines.push(mark(vec![Span::raw(line.clone())]));
            }
            lines.push(Line::default()); // spacer between posts, never marked
        }

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
        } else {
            let mut text = String::from("↑/↓: scroll · Space/PgDn: read on");
            if self.pages > 1 {
                text.push_str(" · ←/→: page · g/G: first/last");
            }
            text.push_str(" · r: reply (quotes marked >) · R: reply, no quote");
            if self.sysop {
                text.push_str(" · x: delete post");
            }
            text.push_str(" · Esc: back");
            draw_help(frame, help, &text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::PostInfo;

    fn threads(n: usize) -> Vec<ThreadInfo> {
        (0..n as i64)
            .map(|id| ThreadInfo {
                id,
                title: format!("T{id}"),
                author: "a".into(),
                post_count: 1,
                last_post: String::new(),
                unread: 0,
            })
            .collect()
    }

    fn list(page: usize, pages: usize, rows: usize) -> ThreadList {
        let mut l = ThreadList::loading(1, false, true);
        l.set("General".into(), threads(rows), page, pages, pages * 25);
        l
    }

    fn view(page: usize, pages: usize, sysop: bool) -> ThreadView {
        let posts = (0..POSTS_PER_PAGE as i64)
            .map(|i| PostInfo {
                id: 1000 + page as i64 * 100 + i,
                author: "a".into(),
                author_is_sysop: false,
                body: "x".into(),
                created: String::new(),
                unread: false,
            })
            .collect();
        let v = ThreadView::from_detail(
            ThreadDetail {
                id: 9,
                board_id: 1,
                board_name: "General".into(),
                title: "T".into(),
                posts,
                page,
                pages,
                total: pages * POSTS_PER_PAGE,
            },
            false,
            sysop,
            2,
        );
        // Pretend a draw happened: 100 lines of content in a 20-line window.
        v.total_lines.set(100);
        v.page_height.set(20);
        v
    }

    fn goto(target: PageTarget) -> impl Fn(&ThreadEvent) -> bool {
        move |e| matches!(e, ThreadEvent::Goto { thread_id: 9, target: t } if *t == target)
    }

    /// A thread of `n` posts, each a single short line, so each post takes
    /// exactly 3 rendered lines (header + body + spacer). Renders it at
    /// `height` rows starting at `scroll`, returning the view for further
    /// assertions.
    fn rendered(n: usize, height: u16, scroll: usize) -> ThreadView {
        let posts = (0..n)
            .map(|i| PostInfo {
                id: 100 + i as i64,
                author: format!("author{i}"),
                author_is_sysop: false,
                body: format!("post{i}"),
                created: String::new(),
                unread: false,
            })
            .collect();
        let mut v = ThreadView::from_detail(
            ThreadDetail {
                id: 9,
                board_id: 1,
                board_name: "General".into(),
                title: "T".into(),
                posts,
                page: 0,
                pages: 1,
                total: n,
            },
            false,
            false,
            0,
        );
        v.scroll.set(scroll);
        let backend = ratatui::backend::TestBackend::new(40, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                v.draw(f, area);
            })
            .unwrap();
        v
    }

    #[test]
    fn quote_target_tracks_whichever_post_is_at_the_top_of_the_view() {
        // 5 posts, viewport tall enough to show a bit more than one post's
        // worth of lines (header + body + spacer = 3 lines) at a time.
        let v = rendered(5, 6, 0);
        assert_eq!(v.quote_index.get(), 0, "scrolled to the very top: post 0");

        let v = rendered(5, 6, 3); // exactly post 1's first line
        assert_eq!(v.quote_index.get(), 1);

        let v = rendered(5, 6, 4); // partway into post 1
        assert_eq!(v.quote_index.get(), 1);

        let v = rendered(5, 6, 6); // exactly post 2's first line
        assert_eq!(v.quote_index.get(), 2);

        // Scrolled well past the end: clamped scroll still lands on a real,
        // valid post (the last one), never past the end of `posts`.
        let v = rendered(5, 6, 9999);
        assert!(v.quote_index.get() < 5);
    }

    #[test]
    fn r_quotes_the_tracked_post_and_big_r_never_quotes_anything() {
        let mut v = rendered(3, 6, 3); // top of view is post 1
        let event = v.handle(Key::Char('r'));
        assert!(matches!(&event, ThreadEvent::Reply { quote: Some((a, _, b)), .. }
            if a == "author1" && b == "post1"));

        let mut v = rendered(3, 6, 3);
        let event = v.handle(Key::Char('R'));
        assert!(matches!(event, ThreadEvent::Reply { quote: None, .. }));
    }

    #[test]
    fn thread_list_page_keys_stay_within_bounds() {
        let mut l = list(1, 3, 25);
        assert!(matches!(l.handle(Key::Right), ThreadsEvent::Page { board_id: 1, page: 2 }));
        assert!(matches!(l.handle(Key::Left), ThreadsEvent::Page { board_id: 1, page: 0 }));

        let mut first = list(0, 3, 25);
        assert!(matches!(first.handle(Key::Left), ThreadsEvent::None), "nothing before page 1");
        assert!(matches!(first.handle(Key::PageUp), ThreadsEvent::None));

        let mut last = list(2, 3, 25);
        assert!(matches!(last.handle(Key::Right), ThreadsEvent::None), "nothing after the last page");
        assert!(matches!(last.handle(Key::End), ThreadsEvent::None));
        assert!(matches!(last.handle(Key::PageDown), ThreadsEvent::None), "already at the end");

        let mut single = list(0, 1, 5);
        assert!(matches!(single.handle(Key::Right), ThreadsEvent::None));
    }

    #[test]
    fn thread_list_pgdn_and_pgup_continue_across_pages_only_at_the_edges() {
        let mut l = list(0, 3, 25);
        assert!(matches!(l.handle(Key::PageDown), ThreadsEvent::None), "moves the selection first");
        assert_eq!(l.selected, 10);
        l.handle(Key::PageDown);
        l.handle(Key::PageDown);
        assert_eq!(l.selected, 24);
        assert!(matches!(l.handle(Key::PageDown), ThreadsEvent::Page { page: 1, .. }));

        let mut mid = list(1, 3, 25);
        assert!(matches!(mid.handle(Key::PageUp), ThreadsEvent::Page { page: 0, .. }), "already at the top row");
    }

    #[test]
    fn opening_and_actions_remember_the_list_page() {
        let mut l = list(2, 3, 25);
        assert!(matches!(l.handle(Key::Enter), ThreadsEvent::Open { thread_id: 0, list_page: 2, .. }));
        assert!(matches!(l.handle(Key::Char('m')), ThreadsEvent::MarkBoardRead { page: 2, .. }));
    }

    #[test]
    fn reading_on_continues_onto_the_next_page_at_the_bottom() {
        let mut v = view(0, 3, false);
        // Mid-page: Space just scrolls.
        assert!(matches!(v.handle(Key::Char(' ')), ThreadEvent::None));
        assert!(v.scroll.get() > 0);
        // Scroll to the bottom, then Space moves on.
        v.scroll.set(usize::MAX);
        assert!(goto(PageTarget::Page(1))(&v.handle(Key::Char(' '))));
        // On the last page there is nowhere to go.
        let mut last = view(2, 3, false);
        last.scroll.set(usize::MAX);
        assert!(matches!(last.handle(Key::Char(' ')), ThreadEvent::None));
    }

    #[test]
    fn paging_back_lands_at_the_bottom_of_the_previous_page() {
        let mut v = view(1, 3, false);
        v.scroll.set(5);
        assert!(matches!(v.handle(Key::PageUp), ThreadEvent::None), "scrolls up first");
        v.scroll.set(0);
        assert!(goto(PageTarget::PageEnd(0))(&v.handle(Key::PageUp)));
        let mut first = view(0, 3, false);
        assert!(matches!(first.handle(Key::PageUp), ThreadEvent::None));
    }

    #[test]
    fn explicit_page_keys_and_first_last() {
        let mut v = view(1, 3, false);
        assert!(goto(PageTarget::Page(2))(&v.handle(Key::Right)));
        assert!(goto(PageTarget::Page(0))(&v.handle(Key::Left)));
        assert!(goto(PageTarget::Page(0))(&v.handle(Key::Char('g'))));
        assert!(goto(PageTarget::Last)(&v.handle(Key::Char('G'))));

        let mut first = view(0, 3, false);
        assert!(matches!(first.handle(Key::Left), ThreadEvent::None));
        first.scroll.set(40);
        assert!(matches!(first.handle(Key::Char('g')), ThreadEvent::None), "on page 1, g scrolls to the top");
        assert_eq!(first.scroll.get(), 0);

        let mut last = view(2, 3, false);
        assert!(matches!(last.handle(Key::Right), ThreadEvent::None));
        assert!(matches!(last.handle(Key::Char('G')), ThreadEvent::None), "on the last page, G scrolls to the bottom");
        assert_eq!(last.scroll.get(), usize::MAX);
    }

    #[test]
    fn back_and_reply_carry_the_right_context() {
        let mut v = view(1, 3, false);
        assert!(matches!(v.handle(Key::Esc), ThreadEvent::Back { board_id: 1, list_page: 2 }));
        let event = v.handle(Key::Char('r'));
        assert!(matches!(&event, ThreadEvent::Reply { thread_id: 9, .. }));
        assert!(matches!(&event, ThreadEvent::Reply { quote: Some((a, c, b)), .. }
            if a == "a" && c.is_empty() && b == "x"));
    }

    // Superseded by quote_target_tracks_whichever_post_is_at_the_top_of_the_view
    // and r_quotes_the_tracked_post_and_big_r_never_quotes_anything: which
    // post 'r' quotes now follows the scroll position (tracked live via a
    // real draw()), not simply "whichever post is last in the array".

    #[test]
    fn delete_prompt_uses_thread_wide_numbers_on_this_page_only() {
        // Page 2 of 3 shows posts #26..#50 (0-based page 1).
        let mut v = view(1, 3, true);
        let ids: Vec<i64> = v.posts.iter().map(|p| p.id).collect();

        v.handle(Key::Char('x'));
        for c in "27".chars() {
            v.handle(Key::Char(c));
        }
        assert!(matches!(v.handle(Key::Enter), ThreadEvent::DeletePost { post_id, page: 1 } if post_id == ids[1]));

        // #3 lives on page 1, so it can't be picked from here.
        v.handle(Key::Char('x'));
        v.handle(Key::Char('3'));
        assert!(matches!(v.handle(Key::Enter), ThreadEvent::None));
        // Neither can #60.
        v.handle(Key::Char('x'));
        for c in "60".chars() {
            v.handle(Key::Char(c));
        }
        assert!(matches!(v.handle(Key::Enter), ThreadEvent::None));
    }
}
