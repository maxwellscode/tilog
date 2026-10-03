//! Selecting text with the mouse and copying it.

use super::{App, Areas};
use crate::clipboard;
use crate::select::Point;

impl App {
    /// The place in the text of tile `index` that screen position (`column`, `row`) is over.
    /// A position outside the tile counts as the nearest edge, so a drag may overshoot.
    pub(super) fn point_at(
        &self,
        index: usize,
        areas: &Areas,
        column: u16,
        row: u16,
    ) -> Option<Point> {
        let rect = areas.tiles.get(index)?;
        let (height, width) = (areas.tile_height(index), areas.tile_width(index));
        let tile = self.tile_ref(index)?;
        let last = tile.last_seq()?;
        if height == 0 || width == 0 {
            return None;
        }
        // +1: the border is not text.
        let row_in_tile = usize::from(row.saturating_sub(rect.y + 1)).min(height - 1);
        let column_in_tile = usize::from(column.saturating_sub(rect.x + 1)).min(width - 1);
        Some(Point {
            seq: (tile.top_seq(height) + row_in_tile as u64).min(last),
            col: tile.display_hscroll(height, width) + column_in_tile,
        })
    }

    /// Puts the selected text on the clipboard.
    pub(super) fn copy_selection(&mut self) {
        let Some(selection) = self.selection else {
            return;
        };
        let (first, last) = selection.ordered();
        let Some(tile) = self.tile_ref(selection.tile) else {
            return;
        };
        let text = selection.extract(&tile.text_between(first.seq, last.seq));

        match clipboard::copy(&text) {
            Ok(via) => {
                let lines = text.lines().count().max(1);
                let noun = if lines == 1 { "line" } else { "lines" };
                self.info(format!("copied {lines} {noun} to the clipboard ({via})"));
            }
            Err(err) => {
                self.error(format!(
                    "not copied: {err:#} (Shift+drag selects with the terminal)"
                ));
            }
        }
    }
}
