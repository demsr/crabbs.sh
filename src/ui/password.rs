use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{Paragraph, Wrap};

use super::input::{Key, TextInput};
use crate::auth;

pub enum PasswordEvent {
    None,
    Cancel,
    Submit { current: String, new: String },
}

/// "Change password": current password, new password, repeat.
pub struct PasswordForm {
    username: String,
    current: TextInput,
    new: TextInput,
    confirm: TextInput,
    focus: usize,
    error: Option<String>,
    /// True while a submitted change is being processed.
    busy: bool,
}

impl PasswordForm {
    pub fn new(username: &str) -> Self {
        Self {
            username: username.to_string(),
            current: TextInput::new(auth::PASSWORD_MAX, true),
            new: TextInput::new(auth::PASSWORD_MAX, true),
            confirm: TextInput::new(auth::PASSWORD_MAX, true),
            focus: 0,
            error: None,
            busy: false,
        }
    }

    fn field(&mut self) -> &mut TextInput {
        match self.focus {
            0 => &mut self.current,
            1 => &mut self.new,
            _ => &mut self.confirm,
        }
    }

    /// The server refused the change. Nothing typed is kept, so a rejected
    /// attempt doesn't leave passwords sitting in the fields.
    pub fn fail(&mut self, message: String) {
        self.busy = false;
        self.error = Some(message);
        self.current.clear();
        self.new.clear();
        self.confirm.clear();
        self.focus = 0;
    }

    pub fn handle(&mut self, key: Key) -> PasswordEvent {
        if self.busy {
            return PasswordEvent::None;
        }
        match key {
            Key::Esc => return PasswordEvent::Cancel,
            Key::Tab | Key::Down => self.focus = (self.focus + 1) % 3,
            Key::BackTab | Key::Up => self.focus = (self.focus + 2) % 3,
            Key::Enter if self.focus < 2 => self.focus += 1,
            Key::Enter => return self.submit(),
            other => {
                self.field().handle(other);
            }
        }
        PasswordEvent::None
    }

    /// Checks everything that doesn't need the server, so obvious mistakes
    /// don't cost a round trip (or a rate-limited attempt).
    fn check(&self) -> Result<(), (usize, String)> {
        if self.current.is_empty() {
            return Err((0, "Enter your current password.".into()));
        }
        let new = self.new.value();
        auth::validate_password(&self.username, &new).map_err(|e| (1, e))?;
        if new == self.current.value() {
            return Err((1, "The new password must differ from the current one.".into()));
        }
        if new != self.confirm.value() {
            return Err((2, "The two new passwords don't match.".into()));
        }
        Ok(())
    }

    fn submit(&mut self) -> PasswordEvent {
        match self.check() {
            Ok(()) => {
                self.error = None;
                self.busy = true;
                PasswordEvent::Submit {
                    current: self.current.value(),
                    new: self.new.value(),
                }
            }
            Err((focus, message)) => {
                self.focus = focus;
                self.error = Some(message);
                PasswordEvent::None
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
            "Enter your current password, then a new one ({}-{} characters).\n\
             Your other sessions will be signed out. Tab/Enter: next field · Esc: cancel",
            auth::PASSWORD_MIN,
            auth::PASSWORD_MAX
        ))
        .wrap(Wrap { trim: true })
        .style(Style::default().fg(Color::Gray));
        frame.render_widget(hint, rows[0]);

        self.current
            .render(frame, rows[1], "Current password", self.focus == 0);
        self.new
            .render(frame, rows[2], "New password", self.focus == 1);
        self.confirm
            .render(frame, rows[3], "Repeat new password", self.focus == 2);

        let note = if self.busy {
            Some(("Changing password…".to_string(), Color::Yellow))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(form: &mut PasswordForm, current: &str, new: &str, confirm: &str) -> PasswordEvent {
        for (i, text) in [current, new, confirm].iter().enumerate() {
            form.focus = i;
            for c in text.chars() {
                form.handle(Key::Char(c));
            }
        }
        form.focus = 2;
        form.handle(Key::Enter)
    }

    #[test]
    fn valid_input_is_submitted_once() {
        let mut f = PasswordForm::new("alice");
        let event = fill(&mut f, "old-password", "new-password-1", "new-password-1");
        assert!(matches!(event, PasswordEvent::Submit { ref current, ref new }
            if current == "old-password" && new == "new-password-1"));
        // While the server works, further keys are ignored.
        assert!(matches!(f.handle(Key::Enter), PasswordEvent::None));
        assert!(f.busy);
    }

    #[test]
    fn mistakes_are_caught_before_the_server_and_focus_the_bad_field() {
        let mut f = PasswordForm::new("alice");
        assert!(matches!(fill(&mut f, "", "new-password-1", "new-password-1"), PasswordEvent::None));
        assert_eq!((f.focus, f.error.as_deref()), (0, Some("Enter your current password.")));

        let mut f = PasswordForm::new("alice");
        assert!(matches!(fill(&mut f, "old-password", "short", "short"), PasswordEvent::None));
        assert_eq!(f.focus, 1);

        let mut f = PasswordForm::new("alice");
        assert!(matches!(fill(&mut f, "same-password", "same-password", "same-password"), PasswordEvent::None));
        assert!(f.error.unwrap().contains("differ"));

        let mut f = PasswordForm::new("alice");
        assert!(matches!(fill(&mut f, "old-password", "new-password-1", "new-password-2"), PasswordEvent::None));
        assert_eq!(f.focus, 2);
        assert!(!f.busy);
    }

    #[test]
    fn a_rejected_attempt_wipes_the_fields() {
        let mut f = PasswordForm::new("alice");
        fill(&mut f, "old-password", "new-password-1", "new-password-1");
        f.fail("Current password is wrong.".into());
        assert!(f.current.is_empty() && f.new.is_empty() && f.confirm.is_empty());
        assert!(!f.busy);
        assert_eq!(f.focus, 0);
    }
}
