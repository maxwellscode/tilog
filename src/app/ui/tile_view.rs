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

        // Whether the view follows the newest line sits in the top right corner, where the eye
        // looks for it: bright when following, plain when paused.
        let mode = if tile.is_history_view() {
            Span::styled(" HISTORY ", Style::new().black().on_magenta().bold())
        } else if tile.is_following() {
            Span::styled(" FOLLOW ", Style::new().black().on_green().bold())
        } else {
            Span::styled(" PAUSED ", Style::new().yellow())
        };
        let border = if selected {
            Style::new().cyan()
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
