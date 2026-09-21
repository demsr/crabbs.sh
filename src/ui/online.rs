use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

use super::OnlineInfo;
use super::input::Key;

pub enum OnlineEvent {
    None,
    Back,
    Refresh,
}

/// A snapshot of who is connected; `r` takes a fresh one.
pub struct OnlineScreen {
    users: Vec<OnlineInfo>,
    guests: usize,
    loaded: bool,
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m", secs / 60),
        _ => format!("{}h {:02}m", secs / 3600, secs % 3600 / 60),
    }
}

impl OnlineScreen {
    pub fn new() -> Self {
        Self {
            users: Vec::new(),
            guests: 0,
            loaded: false,
        }
    }

    pub fn set(&mut self, users: Vec<OnlineInfo>, guests: usize) {
        self.users = users;
        self.guests = guests;
        self.loaded = true;
    }

    pub fn handle(&mut self, key: Key) -> OnlineEvent {
        match key {
            Key::Esc | Key::Char('q') => OnlineEvent::Back,
            Key::Char('r') => OnlineEvent::Refresh,
            _ => OnlineEvent::None,
        }
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(1)])
            .split(area);

        let dim = Style::default().fg(Color::Gray);
        let mut items: Vec<ListItem> = Vec::new();
        if !self.loaded {
            items.push(ListItem::new("Loading…"));
        } else {
            for user in &self.users {
                let mut spans = vec![Span::styled(
                    format!("{:<16}", user.name),
                    Style::default().add_modifier(Modifier::BOLD),
                )];
                spans.push(if user.is_sysop {
                    Span::styled("[sysop] ", Style::default().fg(Color::Yellow))
                } else {
                    Span::raw("        ")
                });
                let sessions = if user.sessions > 1 {
                    format!(" ({} sessions)", user.sessions)
                } else {
                    String::new()
                };
                spans.push(Span::styled(
                    format!(
                        "{:<24}{}{sessions}",
                        user.activity,
                        format_duration(user.connected)
                    ),
                    dim,
                ));
                items.push(ListItem::new(Line::from(spans)));
            }
            if self.guests > 0 {
                let plural = if self.guests == 1 { "" } else { "s" };
                items.push(ListItem::new(Span::styled(
                    format!("+ {} guest{plural} browsing", self.guests),
                    dim,
                )));
            }
            if items.is_empty() {
                items.push(ListItem::new("Nobody else is here."));
            }
        }
        frame.render_widget(
            List::new(items).block(Block::default().title("Who's online").borders(Borders::ALL)),
            rows[0],
        );
        frame.render_widget(Paragraph::new("r: refresh · Esc: back").style(dim), rows[1]);
    }
}

#[cfg(test)]
mod tests {
    use super::format_duration;
    use std::time::Duration;

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(Duration::from_secs(5)), "just now");
        assert_eq!(format_duration(Duration::from_secs(125)), "2m");
        assert_eq!(format_duration(Duration::from_secs(3 * 3600 + 7 * 60)), "3h 07m");
    }
}
