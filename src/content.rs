//! Validation and sanitising of user-written text (thread titles, posts).
//!
//! Everything a user submits goes through here on the server before it is
//! stored, whatever the UI already checked.

pub const TITLE_MIN: usize = 3;
pub const TITLE_MAX: usize = 80;
pub const BODY_MAX: usize = 2000;
pub const MOTD_MAX: usize = 4000;
pub const CHAT_MAX: usize = 300;
pub const SUBJECT_MAX: usize = 80;
/// Column at which the post editor word-wraps: the widest a line can be and
/// still fit inside the borders of an 80-column terminal, so full-width
/// ASCII art survives intact.
pub const WRAP_COLS: usize = 78;
/// Longest run of blank lines kept in a post.
const MAX_BLANK_LINES: usize = 3;
const TAB_WIDTH: usize = 4;

/// Characters that can reorder or hide text and be used to spoof other
/// users' content.
fn is_deceptive(c: char) -> bool {
    matches!(c,
        '\u{202A}'..='\u{202E}' // bidi embeddings / overrides
        | '\u{2066}'..='\u{2069}' // bidi isolates
        | '\u{200B}'..='\u{200F}' // zero-width and directional marks
        | '\u{2028}' | '\u{2029}'
        | '\u{FEFF}')
}

/// Single-line text: control characters removed, whitespace collapsed.
pub fn clean_title(raw: &str) -> Result<String, String> {
    let title = raw
        .chars()
        .filter(|&c| !is_deceptive(c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let len = title.chars().count();
    if len < TITLE_MIN {
        return Err(format!("Title must be at least {TITLE_MIN} characters."));
    }
    if len > TITLE_MAX {
        return Err(format!("Title must be at most {TITLE_MAX} characters."));
    }
    Ok(title)
}

/// Shared by `clean_body` and `clean_motd`: normalises line endings, strips
/// control/bidi/zero-width characters, expands tabs, trims trailing
/// whitespace per line, caps consecutive blank lines, and trims blank lines
/// from the start and end. Indentation and inner spacing are otherwise kept
/// exactly, so ASCII art and code survive.
fn clean_lines(raw: &str) -> Vec<String> {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<String> = Vec::new();
    let mut blank_run = 0;
    for line in normalized.split('\n') {
        let mut cleaned = String::new();
        for c in line.chars().filter(|&c| !is_deceptive(c)) {
            if c == '\t' {
                cleaned.extend(std::iter::repeat_n(' ', TAB_WIDTH));
            } else if !c.is_control() {
                cleaned.push(c);
            }
        }
        cleaned.truncate(cleaned.trim_end().len());
        if cleaned.is_empty() {
            blank_run += 1;
            if blank_run > MAX_BLANK_LINES {
                continue;
            }
        } else {
            blank_run = 0;
        }
        lines.push(cleaned);
    }
    while lines.first().is_some_and(|l| l.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// Multi-line text for a post or message: same cleaning as `clean_motd`, but
/// empty is rejected (there's nothing sensible to post) and capped shorter.
pub fn clean_body(raw: &str) -> Result<String, String> {
    let lines = clean_lines(raw);
    if lines.is_empty() {
        return Err("The message is empty.".into());
    }
    let body = lines.join("\n");
    if body.chars().count() > BODY_MAX {
        return Err(format!("Messages can be at most {BODY_MAX} characters."));
    }
    Ok(body)
}

/// The message of the day: same cleaning as `clean_body`, but empty is fine
/// (that's how a sysop clears it) and the length budget is more generous,
/// since it's static text a sysop chose deliberately rather than a post.
pub fn clean_motd(raw: &str) -> Result<String, String> {
    let text = clean_lines(raw).join("\n");
    if text.chars().count() > MOTD_MAX {
        return Err(format!("The MOTD can be at most {MOTD_MAX} characters."));
    }
    Ok(text)
}

/// A mail subject: like a title, but a single character is enough.
pub fn clean_subject(raw: &str) -> Result<String, String> {
    let subject = raw
        .chars()
        .filter(|&c| !is_deceptive(c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if subject.is_empty() {
        return Err("Please give the message a subject.".into());
    }
    if subject.chars().count() > SUBJECT_MAX {
        return Err(format!("The subject can be at most {SUBJECT_MAX} characters."));
    }
    Ok(subject)
}

/// One line of chat: control characters become spaces, whitespace is
/// collapsed, and it must be non-empty and at most `CHAT_MAX` characters.
pub fn clean_chat(raw: &str) -> Result<String, String> {
    let text = raw
        .chars()
        .filter(|&c| !is_deceptive(c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.is_empty() {
        return Err("Nothing to send.".into());
    }
    if text.chars().count() > CHAT_MAX {
        return Err(format!("Chat messages can be at most {CHAT_MAX} characters."));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_is_collapsed_and_bounded() {
        assert_eq!(
            clean_title("  hello \t  world\x1b[31m ").unwrap(),
            "hello world [31m"
        );
        assert!(clean_title("ab").is_err());
        assert!(clean_title(&"x".repeat(TITLE_MAX + 1)).is_err());
        assert_eq!(clean_title("a\u{202E}bc").unwrap(), "abc");
    }

    #[test]
    fn body_is_sanitised() {
        // more than MAX_BLANK_LINES blank lines are cut down
        assert_eq!(clean_body("a\n\n\n\n\n\nb").unwrap(), "a\n\n\n\nb");
        assert_eq!(
            clean_body("hi\x1b[2J\r\n\r\n\r\n\r\nthere  \n").unwrap(),
            "hi[2J\n\n\n\nthere"
        );
        assert!(clean_body(" \n\t\n").is_err());
        assert!(clean_body(&"x".repeat(BODY_MAX + 1)).is_err());
    }

    #[test]
    fn body_keeps_indentation_and_spacing() {
        let art = "   /\\_/\\\n  ( o.o )\n   > ^ <\n\n\nend   here";
        assert_eq!(clean_body(art).unwrap(), art);
        // leading blank lines go, but the first real line keeps its indent
        assert_eq!(clean_body("\n\n    x\n").unwrap(), "    x");
        // tabs are expanded, not collapsed to one space
        assert_eq!(clean_body("\tx").unwrap(), "    x");
    }

    #[test]
    fn subjects_are_single_line_and_bounded() {
        assert_eq!(clean_subject("  Hi\tthere \x1b").unwrap(), "Hi there");
        assert_eq!(clean_subject("x").unwrap(), "x");
        assert!(clean_subject("   ").is_err());
        assert!(clean_subject(&"x".repeat(SUBJECT_MAX + 1)).is_err());
    }

    #[test]
    fn motd_allows_empty_and_keeps_formatting() {
        assert_eq!(clean_motd("").unwrap(), "", "empty clears the MOTD, not an error");
        assert_eq!(clean_motd("   \n\t \n").unwrap(), "");
        let banner = "  Welcome!\n  Be nice to each other.";
        assert_eq!(clean_motd(banner).unwrap(), banner);
        assert!(clean_motd(&"x".repeat(MOTD_MAX + 1)).is_err());
        assert!(clean_motd(&"x".repeat(MOTD_MAX)).is_ok());
    }

    #[test]
    fn chat_lines_are_cleaned() {
        assert_eq!(clean_chat("  hi \t there\x1b[2J ").unwrap(), "hi there [2J");
        assert!(clean_chat("  \t ").is_err());
        assert!(clean_chat(&"x".repeat(CHAT_MAX + 1)).is_err());
        assert_eq!(clean_chat("a\u{202E}b").unwrap(), "ab");
    }
}
