//! Small text helpers for the screen.

/// `1234567` becomes `1,234,567`.
pub(super) fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// Shortens `text` to at most `max` characters by cutting from the *start* (the end of a path
/// is the interesting part): `…/scratchpad/tomcat.log`.
pub(super) fn ellipsize_start(text: &str, max: usize) -> String {
    let len = text.chars().count();
    if len <= max {
        return text.to_string();
    }
    let tail: String = text.chars().skip(len - max.saturating_sub(1)).collect();
    if max == 0 { tail } else { format!("…{tail}") }
}

/// Shortens `text` to at most `max` characters by cutting from the *end*: `a long mess…`.
pub(super) fn ellipsize_end(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let head: String = text.chars().take(max - 1).collect();
    format!("{head}…")
}

/// `text` broken into lines of at most `width` characters, at spaces. A word longer than a line
/// is cut. Always at least one line.
pub(super) fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        // A word that can't fit on any line is cut into pieces that do.
        while word.chars().count() > width {
            let head: String = word.chars().take(width).collect();
            let rest: String = word.chars().skip(width).collect();
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            lines.push(head);
            word = rest;
        }
        let needed = line.chars().count() + usize::from(!line.is_empty()) + word.chars().count();
        if needed > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(&word);
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_at_spaces_and_cuts_words_that_are_too_long() {
        assert_eq!(wrap_words("aaa bbb ccc", 7), ["aaa bbb", "ccc"]);
        assert_eq!(wrap_words("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(wrap_words("", 5), [""]);
        assert_eq!(wrap_words("one  two", 20), ["one two"]);
        assert!(
            wrap_words("a b c d e f g h i j k", 5)
                .iter()
                .all(|l| l.chars().count() <= 5)
        );
    }

    #[test]
    fn formats_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(1234567), "1,234,567");
    }

    #[test]
    fn ellipsizes_from_the_start_or_the_end() {
        assert_eq!(ellipsize_start("short", 10), "short");
        assert_eq!(ellipsize_start("/var/log/app.log", 8), "…app.log");
        assert_eq!(ellipsize_start("abc", 0), "");
        assert_eq!(ellipsize_end("short", 10), "short");
        assert_eq!(ellipsize_end("cannot open /nope/x.log", 10), "cannot op…");
        assert_eq!(ellipsize_end("abc", 0), "");
    }
}
