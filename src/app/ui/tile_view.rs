//! One tile: its border, text, scrollbars.

use super::text::ellipsize_start;
use crate::app::{App, BORDER_ROWS};
use crate::theme::{self, Theme};
use crate::tile::Tile;
use ratatui::Frame;
use ratatui::layout::Margin;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Scrollbar;
use ratatui::widgets::ScrollbarOrientation;
use ratatui::widgets::ScrollbarState;

impl App {
    /// A question `ssh` is asking is drawn at the bottom of the tile, as in a terminal: the
    /// question, and what has been typed after it on the same row. The log stays above it.
    fn with_question<'a>(
        &self,
        tile: &Tile,
        mut text: Vec<Line<'a>>,
        height: usize,
    ) -> Vec<Line<'a>> {
        let Some(question) = tile.question() else {
            return text;
        };
        if height == 0 {
            return text;
        }
        // As many lines of the question as fit; the last one carries the answer.
        let lines: Vec<&str> = question.prompt.trim_end_matches('\n').lines().collect();
        let lines = &lines[lines.len().saturating_sub(height)..];
        let asked = if self.no_color {
            Style::new().bold()
        } else {
            Style::new().yellow().bold()
        };
        text.truncate(height - lines.len().min(height));
        let (last, earlier) = lines.split_last().unwrap_or((&"", &[]));
        for line in earlier {
            text.push(Line::from(Span::styled((*line).to_string(), asked)));
        }
        text.push(Line::from(vec![
            Span::styled((*last).to_string(), asked),
            Span::raw(question.shown),
            Span::styled(" ", Style::new().reversed()),
        ]));
        text
    }

    pub(super) fn render_tile(
        &self,
        frame: &mut Frame,
        index: usize,
        tile: &Tile,
        name: &str,
        area: Rect,
        selected: bool,
    ) {
        let height = usize::from(area.height.saturating_sub(BORDER_ROWS));
        let width = usize::from(area.width.saturating_sub(BORDER_ROWS));

        let rows = tile.visible(height);
        let longest = rows
            .iter()
            .map(|row| row.chars().count())
            .max()
            .unwrap_or(0);
        let max_x = longest.saturating_sub(width);
        let x = tile.hscroll().min(max_x);
        let highlight = self.active_highlight();

        // A long name (a path) is cut at the start, where the end is the part that identifies
        // the file, to leave room for the rest of the title and the FOLLOW label on the right.
        let extras = tile.title("").chars().count().saturating_sub(2);
        let name_room = usize::from(area.width)
            .saturating_sub(2 + 8 + 2 + extras)
            .max(8);
        let name = ellipsize_start(name, name_room);

        // The text selected with the mouse in this tile, row by row.
        let selection = self.selection.filter(|selection| selection.tile == index);
        let top = tile.top_seq(height);
        let current = tile.marked_rows();
        let text: Vec<Line> = rows
            .iter()
            .enumerate()
            .map(|(row, text)| {
                let marked = selection.and_then(|selection| selection.columns_in(top + row as u64));
                let line = theme::render_row(
                    text,
                    &self.theme,
                    highlight,
                    tile.label_cols(),
                    x,
                    width,
                    marked,
                );
                // The entry `n` / `N` is at stands out from its surroundings: the stepped
                // entry of a filter, or else the match of the search that is on.
                let seq = top + row as u64;
                let searched = current.is_none()
                    && tile.match_row() == Some(seq)
                    && highlight.is_some_and(|highlight| highlight.is_match(text));
                if searched || current.as_ref().is_some_and(|rows| rows.contains(&seq)) {
                    // The background runs to the edge, not just under the text.
                    let used: usize = line
                        .spans
                        .iter()
                        .map(|span| span.content.chars().count())
                        .sum();
                    let mut line = line;
                    for span in &mut line.spans {
                        span.style = Theme::on_marked(span.style);
                    }
                    line.spans
                        .push(Span::raw(" ".repeat(width.saturating_sub(used))));
                    line.style(self.theme.marked_style())
                } else {
                    line
                }
            })
            .collect();
        let text = self.with_question(tile, text, height);

        // Whether the view follows the newest line sits in the top right corner, where the eye
        // looks for it: bright when following, plain when paused.
        let mode = if tile.is_history_view() {
            Span::styled(" HISTORY ", Style::new().black().on_magenta().bold())
        } else if tile.is_following() {
            Span::styled(" FOLLOW ", Style::new().black().on_green().bold())
        } else {
            // Without color, PAUSED is reverse video like FOLLOW: both are as easy to spot.
            let style = if self.no_color {
                Style::new().bold().reversed()
            } else {
                Style::new().yellow()
            };
            Span::styled(" PAUSED ", style)
        };
        // Which tile is selected is also shown without color: bold border against a dim one.
        let border = if selected {
            if self.no_color {
                Style::new().bold()
            } else {
                Style::new().cyan()
            }
        } else if self.no_color {
            Style::new().dim()
        } else {
            Style::new().dark_gray()
        };
        let block = Block::bordered()
            .title(tile.title(&name))
            .title_top(Line::from(mode).right_aligned())
            .border_style(border);
        frame.render_widget(Paragraph::new(text).block(block), area);

        // Scrollbars sit on the border, so they cost no space. They only appear when the
        // content doesn't fit, and only the thumb is drawn, in plain white so it stands out
        // from the border: no arrows, no track.
        if let Some((positions, position)) = tile.vertical_extent(height) {
            let mut state = ScrollbarState::new(positions)
                .position(position)
                .viewport_content_length(height);
            // Box-drawing glyphs fill the whole cell and join the border, so the thumb has no
            // gaps between rows, even where the terminal adds line spacing (a `█` column does).
            let bar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .track_symbol(None)
                .end_symbol(None)
                .thumb_symbol("┃")
                .thumb_style(Style::new().fg(Color::White));
            frame.render_stateful_widget(
                bar,
                area.inner(Margin {
                    vertical: 1,
                    horizontal: 0,
                }),
                &mut state,
            );
        }
        if max_x > 0 {
            let mut state = ScrollbarState::new(max_x + 1)
                .position(x)
                .viewport_content_length(width);
            let bar = Scrollbar::new(ScrollbarOrientation::HorizontalBottom)
                .begin_symbol(None)
                .track_symbol(None)
                .end_symbol(None)
                .thumb_symbol("━")
                .thumb_style(Style::new().fg(Color::White));
            frame.render_stateful_widget(
                bar,
                area.inner(Margin {
                    vertical: 0,
                    horizontal: 1,
                }),
                &mut state,
            );
        }
    }

    // ---- the status line ---------------------------------------------------------------
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::askpass::{Ask, Kind};
    use crate::source::Source;

    fn plain(line: &Line) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn the_question_sits_at_the_bottom_with_the_answer_after_it_and_the_log_above() {
        let app = App::new();
        let mut source = Source::fake_stdin();
        let tile = source.tile_mut(0).unwrap();
        let (ask, _answers) = Ask::for_test("deploy@web1's password: ", Kind::Secret);
        tile.ask_for_test(ask);
        for c in "abc".chars() {
            tile.type_char(c);
        }

        let log = vec![
            Line::from("first"),
            Line::from("second"),
            Line::from("third"),
        ];
        let shown = app.with_question(source.main(), log, 4);
        let rows: Vec<String> = shown.iter().map(plain).collect();
        assert_eq!(
            rows,
            ["first", "second", "third", "deploy@web1's password: ••• "]
        );

        // In a tile only three rows tall the log gives way: the question always shows.
        let shown = app.with_question(
            source.main(),
            vec![Line::from("a"), Line::from("b"), Line::from("c")],
            2,
        );
        assert_eq!(shown.len(), 2);
        assert!(plain(&shown[1]).starts_with("deploy@web1's password: "));
    }

    #[test]
    fn a_long_question_shows_its_last_lines_and_keeps_the_answer_on_the_last() {
        let app = App::new();
        let mut source = Source::fake_stdin();
        let tile = source.tile_mut(0).unwrap();
        let text = "The authenticity of host 'web1' can't be established.\nED25519 key fingerprint is SHA256:abc.\nContinue (yes/no)? ";
        let (ask, _answers) = Ask::for_test(text, Kind::Confirm);
        tile.ask_for_test(ask);
        tile.type_char('y');

        let shown = app.with_question(source.main(), Vec::new(), 10);
        let rows: Vec<String> = shown.iter().map(plain).collect();
        assert_eq!(rows.len(), 3);
        assert!(rows[0].starts_with("The authenticity"));
        assert_eq!(rows[2], "Continue (yes/no)? y ", "typed in the open");

        // Not enough room for all of it: the end of the question is what is kept.
        let shown = app.with_question(source.main(), Vec::new(), 2);
        assert!(plain(&shown[0]).starts_with("ED25519"));
        assert!(plain(&shown[1]).starts_with("Continue"));
    }
}
