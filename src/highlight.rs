//! Search highlighting: text you typed (without a leading `/`) is marked wherever it appears in
//! the visible tiles, like the find bar of an editor.

use anyhow::{Result, anyhow};
use ratatui::style::Style;
use regex::{Regex, RegexBuilder};

use crate::theme::paint_matches;

#[derive(Clone)]
pub struct Highlight {
    regex: Regex,
    label: String,
}

impl Highlight {
    /// Plain text with "smart case": ignores case unless the text has an uppercase letter,
    /// so `error` finds `ERROR` and `Error` finds only `Error`.
    pub fn literal(text: &str) -> Self {
        let ignore_case = !text.chars().any(char::is_uppercase);
        Self::new(text, false, ignore_case).expect("an escaped literal is always a valid regex")
    }

    pub fn new(pattern: &str, is_regex: bool, ignore_case: bool) -> Result<Self> {
        let source = if is_regex {
            pattern.to_string()
        } else {
            regex::escape(pattern)
        };
        let regex = RegexBuilder::new(&source)
            .case_insensitive(ignore_case)
            .build()
            .map_err(|e| {
                anyhow!(
                    "invalid regex: {}",
                    e.to_string().lines().last().unwrap_or("").trim()
                )
            })?;
        let label = format!("{}{pattern}", if is_regex { "re:" } else { "" });
        Ok(Self { regex, label })
    }

    /// Does `text` contain a match?
    pub fn is_match(&self, text: &str) -> bool {
        self.regex.is_match(text)
    }

    /// What was searched for, for messages.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Paints `style` onto every match in `text` (see `theme::paint_matches`).
    pub fn paint(&self, text: &str, starts: &[usize], styles: &mut [Style], style: Style) {
        paint_matches(&self.regex, text, starts, styles, style, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::char_starts;

    /// Which characters of `text` get painted, as a string of `^` marks.
    fn marked(highlight: &Highlight, text: &str) -> String {
        let marker = Style::new().bold();
        let starts = char_starts(text);
        let mut styles = vec![Style::default(); starts.len()];
        highlight.paint(text, &starts, &mut styles, marker);
        styles
            .iter()
            .map(|s| if *s == marker { '^' } else { ' ' })
            .collect()
    }

    #[test]
    fn smart_case() {
        let text = "An ERROR, an Error";
        // All lowercase: ignores case, so both spellings are found.
        let both = format!("{}{}{}", "   ", "^^^^^     ", "^^^^^");
        assert_eq!(marked(&Highlight::literal("error"), text), both);
        // An uppercase letter makes it exact: only "Error" matches.
        let only_exact = format!("{}{}", " ".repeat(13), "^^^^^");
        assert_eq!(marked(&Highlight::literal("Error"), text), only_exact);
    }

    #[test]
    fn literal_text_is_not_a_regex() {
        assert_eq!(marked(&Highlight::literal("a.b"), "a.b axb"), "^^^    ");
    }

    #[test]
    fn regex_mode_and_errors() {
        let highlight = Highlight::new(r"\d+", true, false).unwrap();
        assert_eq!(marked(&highlight, "id 42, 7"), "   ^^  ^");
        assert_eq!(highlight.label(), r"re:\d+");
        assert!(Highlight::new("(", true, false).is_err());
    }
}
