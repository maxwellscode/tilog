//! Moving the view of a tile: up, down, sideways, to a line.

use super::{Content, Tile};

impl Tile {
    /// For a vertical scrollbar: how many positions the view can take (`total`), and which
    /// one it is at. `height` is the number of visible rows. `None` if everything fits.
    pub fn vertical_extent(&self, height: usize) -> Option<(usize, usize)> {
        let lines = self.content.lines();
        let total = usize::try_from(lines.end_seq() - lines.first_seq()).ok()?;
        if total <= height {
            return None;
        }
        let position = usize::try_from(
            self.view
                .top(lines, height)
                .saturating_sub(lines.first_seq()),
        )
        .ok()?;
        Some((total - height + 1, position))
    }

    pub fn hscroll(&self) -> usize {
        self.hscroll
    }

    /// How far the text is really scrolled sideways: what was asked for, but never past the
    /// end of the longest visible line.
    pub fn display_hscroll(&self, height: usize, width: usize) -> usize {
        self.hscroll.min(self.max_hscroll(height, width))
    }

    /// The most the text can be scrolled sideways: the longest *visible* line, minus the width.
    /// (Lines that are scrolled off screen vertically can't be measured, for a filter tile
    /// they'd have to be read from disk first.)
    pub fn max_hscroll(&self, height: usize, width: usize) -> usize {
        let longest = self
            .visible(height)
            .iter()
            .map(|row| row.chars().count())
            .max();
        longest.unwrap_or(0).saturating_sub(width)
    }

    // `self.view` (mutable) and `self.content` (shared) are different fields, so borrowing
    // both at once is fine.
    pub fn scroll_up(&mut self, height: usize, n: u64) {
        // About to scroll past the oldest loaded line: fetch older ones from disk first.
        let lines = self.content.lines();
        if self
            .view
            .top(lines, height)
            .saturating_sub(lines.first_seq())
            < n
        {
            self.load_older();
        }
        self.view.scroll_up(self.content.lines(), height, n);
    }

    pub fn scroll_down(&mut self, height: usize, n: u64) {
        self.extend_window(height, n);
        self.view.scroll_down(self.content.lines(), height, n);
        if self.view.is_following() {
            self.leave_window(); // scrolled down to the end of the file: back to the live view
        }
    }

    pub fn scroll_left(&mut self, n: usize) {
        self.hscroll = self.hscroll.saturating_sub(n);
    }

    pub fn scroll_right(&mut self, height: usize, width: usize, n: usize) {
        self.hscroll = (self.hscroll + n).min(self.max_hscroll(height, width));
    }

    pub fn jump_to_start(&mut self) {
        self.view.jump_to_start(self.content.lines());
    }

    /// Stops following, with the view where it is now.
    pub fn pause(&mut self) {
        let height = self.shown_height.get();
        self.view.scroll_up(self.content.lines(), height, 0);
    }

    pub fn jump_to_end(&mut self) {
        self.leave_window();
        self.marked = None;
        self.view.jump_to_end();
    }

    /// 1-based line number, as users count: of the part of the file first loaded for the main
    /// tile (its end, for a big file), of the matches for a filter tile.
    pub fn jump_to_line(&mut self, line: u64) {
        let seq = match &self.content {
            Content::Source { lines, .. } | Content::Merged { lines, .. } => {
                lines.seq_of_line(line)
            }
            Content::Filter(_) | Content::StreamFilter(_) => line.saturating_sub(1),
        };
        let lines = self.content.lines();
        if (lines.first_seq()..lines.end_seq()).contains(&seq) {
            self.show_row(seq, self.shown_height.get());
        } else {
            self.marked = None;
            self.view.jump_to(seq); // past either end: the view is clamped to the nearest edge
        }
    }

    /// Scrolls to `row`, a third of the way down so there is context above it (as a search
    /// does), and marks it.
    pub(super) fn show_row(&mut self, row: u64, height: usize) {
        self.marked = Some(row..row + 1);
        self.view.jump_to(row.saturating_sub(height as u64 / 3));
    }
}
