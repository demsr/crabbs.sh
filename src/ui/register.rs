use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Paragraph, Wrap};

use super::input::{Key, TextInput};
use crate::auth;

pub enum FormEvent {
    None,
    Cancel,
    Submit { username: String, password: String },
}

pub struct RegisterForm {
    username: TextInput,
    password: TextInput,
    confirm: TextInput,
    focus: usize,
    error: Option<String>,
    /// True while a submitted registration is being processed.
    busy: bool,
}

impl RegisterForm {
    pub fn new() -> Self {
        Self {
            username: TextInput::new(auth::USERNAME_MAX, false),
            password: TextInput::new(auth::PASSWORD_MAX, true),
            confirm: TextInput::new(auth::PASSWORD_MAX, true),
            focus: 0,
            error: None,
            busy: false,
        }
    }

    fn field(&mut self) -> &mut TextInput {
        match self.focus {
            0 => &mut self.username,
            1 => &mut self.password,
            _ => &mut self.confirm,
        }
    }

    /// Called when the server rejected the registration.
    pub fn fail(&mut self, message: String) {
        self.busy = false;
        self.error = Some(message);
        self.password.clear();
        self.confirm.clear();
        self.focus = 1;
    }

    pub fn handle(&mut self, key: Key) -> FormEvent {
        if self.busy {
            return FormEvent::None;
        }
        match key {
            Key::Esc => return FormEvent::Cancel,
            Key::Tab | Key::Down => self.focus = (self.focus + 1) % 3,
            Key::BackTab | Key::Up => self.focus = (self.focus + 2) % 3,
            Key::Enter if self.focus < 2 => self.focus += 1,
            Key::Enter => return self.submit(),
            other => {
                self.field().handle(other);
            }
        }
        FormEvent::None
    }

    fn submit(&mut self) -> FormEvent {
        let username = self.username.value();
        let password = self.password.value();
        let problem = auth::validate_username(&username)
            .map_err(|e| (0, e))
            .and_then(|_| auth::validate_password(&username, &password).map_err(|e| (1, e)))
            .and_then(|_| {
                if password == self.confirm.value() {
                    Ok(())
                } else {
                    Err((2, "The two passwords don't match.".to_string()))
                }
            });
        match problem {
            Ok(()) => {
                self.error = None;
                self.busy = true;
                FormEvent::Submit { username, password }
            }
            Err((focus, message)) => {
                self.focus = focus;
                self.error = Some(message);
                FormEvent::None
            }
        }
    }

    pub fn draw(&self, frame: &mut Frame, area: Rect) {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(3),
                Constraint::Length(2),
                Constraint::Min(0),
            ])
            .split(area);

        let hint = Paragraph::new(format!(
            "Pick a username ({}-{} letters, digits, '_' or '-') and a password of at least {} characters.\n\
             Tab/Enter: next field · Esc: cancel",
            auth::USERNAME_MIN,
            auth::USERNAME_MAX,
            auth::PASSWORD_MIN
        ))
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(Color::Gray));
        frame.render_widget(hint, rows[0]);

        self.username
            .render(frame, rows[1], "Username", self.focus == 0);
        self.password
            .render(frame, rows[2], "Password", self.focus == 1);
        self.confirm
            .render(frame, rows[3], "Repeat password", self.focus == 2);

        let note = if self.busy {
            Some(("Creating account…".to_string(), Color::Yellow))
        } else {
            self.error.clone().map(|e| (e, Color::Red))
        };
        if let Some((text, color)) = note {
            frame.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: true })
                    .style(Style::default().fg(color)),
                rows[4],
            );
        }
    }
}
