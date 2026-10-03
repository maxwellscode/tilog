//! The bottom line: the prompt, or a message and the stats.

use super::text::{ellipsize_end, thousands};
use crate::app::App;
use crate::source::Source;
use crate::tile::Tile;
use ratatui::Frame;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::style::{Style, Stylize};
use ratatui::text::Line;
use ratatui::text::Span;

impl App {
    /// The numbers for whatever the screen is showing.
    pub(super) fn stats_text(&self) -> String {
        let session = self
            .session_name
            .as_ref()
            .map_or(String::new(), |name| format!("session {name} · "));
        match self.current_source() {
            None => {
                let lines: u64 = self.sources.iter().map(|s| s.main().total_lines()).sum();
                let rate: f64 = self.sources.iter().map(Source::rate).sum();
                format!(
                    "{session}{} sources · {} lines · {rate:.1}/s",
                    self.sources.len(),
                    thousands(lines)
                )
            }
            Some(source) => {
                let main = source.main();
                let mut text = format!(
                    "{session}{} lines · {:.1}/s · {} filters",
                    thousands(main.total_lines()),
                    source.rate(),
                    source.tiles().len() - 1,
                );
                // If a filter tile is focused, its match count too.
                if let Some(matches) = source.tiles()[source.focus()].matches() {
                    text += &format!(" · {} matches", thousands(matches as u64));
                }
                text
            }
        }
    }

    /// What the status line says when nothing else does: the keys that matter here.
    pub(super) fn hint(&self) -> String {
        if let Some(highlight) = &self.highlight {
            return format!("“{}” · n/N next/previous · Esc clears", highlight.label());
        }
        let in_filter = self.tab > 0
            && self
                .tile_ref(self.target_index())
                .is_some_and(Tile::is_filter);
        if in_filter {
            return "n/N step through matches · Tab, G: main pane back to live · ? help · q back"
                .to_string();
        }
        if self.current_source().is_some() {
            "/ search · & filter · : command · ? help · q back".to_string()
        } else {
            "/ search · : command · Enter open · ? help · q quit".to_string()
        }
    }

    /// One line, like the bottom line of `less`: the open prompt, or a message (left) and the
    /// stats (right).
    pub(super) fn render_status(&self, frame: &mut Frame, area: Rect) {
        if let Some(prompt) = self.prompt {
            // The text may be longer than the line: show the part around the cursor.
            let room = usize::from(area.width).saturating_sub(1).max(1);
            let cursor = self.input.cursor();
            let offset = (cursor + 1).saturating_sub(room);
            let text: String = self.input.text().chars().skip(offset).take(room).collect();

            let line = Line::from(vec![
                Span::from(prompt.prefix().to_string()).cyan().bold(),
                Span::raw(text),
            ]);
            frame.render_widget(line, area);
            // +1 for the prefix. Text is assumed to be one column per character.
            let x = area.x + 1 + u16::try_from(cursor - offset).unwrap_or(0);
            frame.set_cursor_position(Position::new(x, area.y));
            return;
        }

        let stats = self.stats_text();
        let width = usize::from(area.width);
        let stats_width = stats.chars().count();
        // On a narrow terminal the message matters more than the numbers.
        let show_stats = width >= stats_width + 24;
        let room = if show_stats {
            width - stats_width - 2
        } else {
            width
        };

        let left = match &self.notice {
            Some(notice) if notice.is_error => Span::from(ellipsize_end(&notice.text, room)).red(),
            Some(notice) => Span::from(ellipsize_end(&notice.text, room)).green(),
            None => Span::from(ellipsize_end(&self.hint(), room)).dark_gray(),
        };
        frame.render_widget(Line::from(left), area);
        if show_stats {
            let stats = Span::styled(stats, Style::new().dark_gray());
            frame.render_widget(Line::from(stats).right_aligned(), area);
        }
    }

    // ---- popups ------------------------------------------------------------------------
}
