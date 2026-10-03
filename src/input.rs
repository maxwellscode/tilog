use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A single-line text box.
///
/// Stored as `Vec<char>` rather than `String`: a Rust `String` is UTF-8 bytes, and one visible
/// character can take 1 to 4 bytes. Indexing by byte would put the cursor in the middle of a
/// character. A `char` is one Unicode scalar value, so "cursor position" is a plain index.
#[derive(Default)]
pub struct InputBox {
    chars: Vec<char>,
    /// Index of the character the cursor is in front of (0..=chars.len()).
    cursor: usize,
}

impl InputBox {
    pub fn text(&self) -> String {
        self.chars.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Returns the current text and leaves the box empty.
    pub fn take(&mut self) -> String {
        let text = self.text();
        self.clear();
        text
    }

    /// Replaces the text and puts the cursor at the end.
    pub fn set(&mut self, text: &str) {
        self.chars = text.chars().collect();
        self.cursor = self.chars.len();
    }

    pub fn clear(&mut self) {
        self.chars.clear();
        self.cursor = 0;
    }

    /// Applies an editing key. Keys that aren't editing keys are ignored.
    pub fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            // A match *guard* (`if ...`): only plain typing, not Ctrl+x or Alt+x shortcuts.
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.chars.insert(self.cursor, c);
                self.cursor += 1;
            }
            KeyCode::Backspace if self.cursor > 0 => {
                self.cursor -= 1;
                self.chars.remove(self.cursor);
            }
            KeyCode::Delete if self.cursor < self.chars.len() => {
                self.chars.remove(self.cursor);
            }
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.chars.len()),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(input: &mut InputBox, code: KeyCode) {
        input.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn edits_in_the_middle_with_multibyte_characters() {
        let mut input = InputBox::default();
        for c in "/gäto".chars() {
            press(&mut input, KeyCode::Char(c));
        }
        press(&mut input, KeyCode::Left);
        press(&mut input, KeyCode::Left);
        press(&mut input, KeyCode::Backspace); // removes 'ä'
        press(&mut input, KeyCode::Char('o'));
        assert_eq!(input.text(), "/goto");
        assert_eq!(input.cursor(), 3);
        assert_eq!(input.take(), "/goto");
        assert_eq!(input.text(), "");
    }

    #[test]
    fn ctrl_combinations_do_not_type() {
        let mut input = InputBox::default();
        input.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(input.text(), "");
    }
}
