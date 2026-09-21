use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Word-wraps `text` to `width` display columns. Lines that already fit are
/// returned untouched, so indentation and spacing (ASCII art, code) survive.
/// Longer lines are broken at spaces and continuation lines keep the
/// original indent; words longer than a line are split.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for paragraph in text.split('\n') {
        if paragraph.width() <= width {
            out.push(paragraph.to_string());
            continue;
        }
        let content = paragraph.trim_start_matches(' ');
        let indent_width = (paragraph.len() - content.len()).min(width / 2);
        let indent = " ".repeat(indent_width);

        let mut line = indent.clone();
        let mut line_width = indent_width;
        for word in content.split(' ') {
            let word_width = word.width();
            if line_width > indent_width && line_width + 1 + word_width > width {
                out.push(std::mem::replace(&mut line, indent.clone()));
                line_width = indent_width;
            }
            if line_width > indent_width {
                line.push(' ');
                line_width += 1;
            }
            if line_width + word_width > width {
                for c in word.chars() {
                    let cw = c.width().unwrap_or(0);
                    if line_width + cw > width {
                        out.push(std::mem::replace(&mut line, indent.clone()));
                        line_width = indent_width;
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

    #[test]
    fn keeps_short_lines_exactly() {
        assert_eq!(wrap("   /\\_/\\\n  ( o.o )\n a  b", 78), vec!["   /\\_/\\", "  ( o.o )", " a  b"]);
    }

    #[test]
    fn wrapped_lines_keep_indent() {
        assert_eq!(wrap("  aaa bbb ccc", 9), vec!["  aaa bbb", "  ccc"]);
    }
}
