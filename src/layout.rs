//! Pure geometry: how many rectangles go where. No state, no drawing, so it is easy to test.

use ratatui::layout::{Constraint, Layout, Rect};

/// Tiles `n` windows into `area` as a grid that is as square as possible.
/// A last, shorter row is stretched to the full width.
pub fn grid(area: Rect, n: usize) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let cols = (n as f64).sqrt().ceil() as usize;
    let rows = n.div_ceil(cols);

    let mut tiles = Vec::with_capacity(n);
    for (row, row_area) in Layout::vertical(vec![Constraint::Fill(1); rows])
        .split(area)
        .iter()
        .enumerate()
    {
        let in_this_row = (n - row * cols).min(cols);
        let cells = Layout::horizontal(vec![Constraint::Fill(1); in_this_row]).split(*row_area);
        tiles.extend(cells.iter().copied());
    }
    tiles
}

/// A source tab: the main tile (index 0) on the left, filter tiles stacked top-down on the
/// right. Until the first filter exists, the main tile gets the full width.
pub fn main_and_filters(area: Rect, tile_count: usize) -> Vec<Rect> {
    if tile_count <= 1 {
        return vec![area];
    }
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(area);
    let rows = Layout::vertical(vec![Constraint::Fill(1); tile_count - 1]).split(right);
    std::iter::once(left).chain(rows.iter().copied()).collect()
}

/// Moves `index` by `delta` within `0..len`, wrapping around at both ends.
pub fn rotate(index: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    (index as isize + delta).rem_euclid(len as isize) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    const AREA: Rect = Rect::new(0, 0, 100, 40);

    #[test]
    fn grid_shapes() {
        assert!(grid(AREA, 0).is_empty());
        assert_eq!(grid(AREA, 1), [AREA]);

        let two = grid(AREA, 2);
        assert_eq!((two[0].width, two[1].width, two[0].height), (50, 50, 40));

        // 3 windows: two on top, the last one spans the full width below.
        let three = grid(AREA, 3);
        assert_eq!(
            (three[0].width, three[1].width, three[2].width),
            (50, 50, 100)
        );
        assert_eq!(three[2].y, 20);

        assert_eq!(grid(AREA, 4).len(), 4);
        assert_eq!(grid(AREA, 5).len(), 5); // 3 + 2
    }

    #[test]
    fn source_tab_layout() {
        assert_eq!(main_and_filters(AREA, 1), [AREA]);

        let tiles = main_and_filters(AREA, 3);
        assert_eq!(tiles.len(), 3);
        assert_eq!(tiles[0].width, 45);
        assert_eq!(tiles[1].x, 45);
        assert_eq!((tiles[1].height, tiles[2].height), (20, 20));
        assert_eq!(tiles[2].y, 20);
    }

    #[test]
    fn rotate_wraps() {
        assert_eq!(rotate(0, 3, -1), 2);
        assert_eq!(rotate(2, 3, 1), 0);
        assert_eq!(rotate(1, 3, 1), 2);
        assert_eq!(rotate(0, 0, 1), 0);
    }
}
