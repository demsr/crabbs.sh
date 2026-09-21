//! Validation and sanitising of user-written text (thread titles, posts).
//!
//! Everything a user submits goes through here on the server before it is
//! stored, whatever the UI already checked.

pub const TITLE_MIN: usize = 3;
pub const TITLE_MAX: usize = 80;
pub const BODY_MAX: usize = 2000;
/// Column at which the post editor word-wraps, so posts read well on an
/// 80-column terminal.
pub const WRAP_COLS: usize = 76;

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

/// Multi-line text: newlines kept, tabs become spaces, other control
/// characters dropped, trailing whitespace trimmed and runs of blank lines
/// limited to one.
pub fn clean_body(raw: &str) -> Result<String, String> {
    let normalized = raw.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<String> = Vec::new();
    let mut blank_run = 0;
    for line in normalized.split('\n') {
        let cleaned: String = line
            .chars()
            .filter(|&c| !is_deceptive(c))
            .map(|c| if c == '\t' { ' ' } else { c })
            .filter(|c| !c.is_control())
            .collect();
        let cleaned = cleaned.trim_end().to_string();
        if cleaned.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
        } else {
            blank_run = 0;
        }
        lines.push(cleaned);
    }
    let body = lines.join("\n").trim().to_string();
    if body.is_empty() {
        return Err("The message is empty.".into());
    }
    if body.chars().count() > BODY_MAX {
        return Err(format!("Messages can be at most {BODY_MAX} characters."));
    }
    Ok(body)
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
        assert_eq!(
            clean_body("hi\x1b[2J\r\n\r\n\r\n\r\nthere  \n").unwrap(),
            "hi[2J\n\nthere"
        );
        assert!(clean_body(" \n\t\n").is_err());
        assert!(clean_body(&"x".repeat(BODY_MAX + 1)).is_err());
    }
}
