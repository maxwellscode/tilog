//! How the screen is divided: the fixed regions and the rectangle of every tile.

use super::BORDER_ROWS;
use ratatui::layout::Constraint;
use ratatui::layout::Layout;
use ratatui::layout::Rect;

/// The screen split into regions, plus the rectangle of every tile on the current tab.
pub(super) struct Areas {
    pub(super) screen: Rect,
    pub(super) tabbar: Rect,
    pub(super) body: Rect,
    /// One line: messages and stats, or the prompt while typing.
    pub(super) status: Rect,
    pub(super) tiles: Vec<Rect>,
}

impl Areas {
    /// Splits the screen into its fixed regions. `tiles` is filled in by `App::areas`,
    /// because only the app knows which tab is showing.
    pub(super) fn new(screen: Rect) -> Self {
        let [tabbar, body, status] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .areas(screen);
        Self {
            screen,
            tabbar,
            body,
            status,
            tiles: Vec::new(),
        }
    }

    /// Number of text rows inside tile `index`.
    pub(super) fn tile_height(&self, index: usize) -> usize {
        self.tiles.get(index).map_or(0, |rect| {
            usize::from(rect.height.saturating_sub(BORDER_ROWS))
        })
    }

    /// Number of text columns inside tile `index`.
    pub(super) fn tile_width(&self, index: usize) -> usize {
        self.tiles.get(index).map_or(0, |rect| {
            usize::from(rect.width.saturating_sub(BORDER_ROWS))
        })
    }
}
