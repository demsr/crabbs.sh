use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Word-wraps `text` to `width` display columns. Existing newlines are kept
/// (blank lines stay blank); words longer than a line are split.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        let mut line_width = 0;
        for word in paragraph.split(' ') {
            let word_width = word.width();
            if line_width > 0 && line_width + 1 + word_width > width {
                out.push(std::mem::take(&mut line));
                line_width = 0;
            }
            if line_width > 0 {
                line.push(' ');
                line_width += 1;
            }
            if word_width > width {
                for c in word.chars() {
                    let cw = c.width().unwrap_or(0);
                    if line_width + cw > width {
                        out.push(std::mem::take(&mut line));
                        line_width = 0;
                    }
                    line.push(c);
                    line_width += cw;
                }
            } else {
                line.push_str(word);
                line_width += word_width;
            }
        }
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::wrap;

    #[test]
    fn wraps_words_and_keeps_blank_lines() {
        assert_eq!(wrap("aaa bbb ccc", 7), vec!["aaa bbb", "ccc"]);
        assert_eq!(wrap("a\n\nb", 10), vec!["a", "", "b"]);
    }

    #[test]
    fn splits_overlong_words() {
        assert_eq!(wrap("abcdefgh", 3), vec!["abc", "def", "gh"]);
    }
}
