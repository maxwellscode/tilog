//! `NO_COLOR` (<https://no-color.org>): the screen without any color.
//!
//! The normal screen is drawn as usual, and every color is taken out afterwards. What colors
//! carried is kept as far as a monochrome terminal allows: text that was drawn on a colored
//! background (the FOLLOW label, the marked row, search hits, the selection) becomes reverse
//! video, and bold stays bold.

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

/// Is color switched off by the environment? An empty `NO_COLOR` counts as not set, as the
/// convention says.
pub fn requested() -> bool {
    std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty())
}

/// Takes the colors out of everything drawn in `buffer`.
pub fn strip_colors(buffer: &mut Buffer) {
    for cell in &mut buffer.content {
        if cell.bg != Color::Reset {
            cell.modifier.insert(Modifier::REVERSED);
        }
        cell.fg = Color::Reset;
        cell.bg = Color::Reset;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    #[test]
    fn colors_go_but_emphasis_stays() {
        let mut buffer = Buffer::empty(Rect::new(0, 0, 4, 1));
        buffer.set_string(0, 0, "a", Style::new().red().bold());
        buffer.set_string(1, 0, "b", Style::new().black().on_green());
        buffer.set_string(2, 0, "c", Style::new());
        strip_colors(&mut buffer);

        let cell = |x: u16| buffer[(x, 0)].clone();
        assert_eq!(cell(0).fg, Color::Reset);
        assert!(cell(0).modifier.contains(Modifier::BOLD));
        assert!(
            !cell(0).modifier.contains(Modifier::REVERSED),
            "a foreground alone is not emphasis"
        );
        assert!(
            cell(1).modifier.contains(Modifier::REVERSED),
            "a background becomes reverse video"
        );
        assert_eq!((cell(1).fg, cell(1).bg), (Color::Reset, Color::Reset));
        assert_eq!(cell(2).modifier, Modifier::empty());
    }
}
