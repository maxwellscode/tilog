//! Selecting text with the mouse. A selection lives inside one tile and is stored as positions
//! in the *log* (line sequence number and character column), not on the screen: while you drag,
//! a followed log keeps scrolling, and the selection must stay on the lines you pointed at.

use std::ops::Range;

/// A place in a tile's text. Ordered like reading order: by line, then by column.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Point {
    pub seq: u64,
    pub col: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct Selection {
    /// Which tile of the current tab.
    pub tile: usize,
    pub anchor: Point,
    pub head: Point,
    /// Has the pointer moved since the button went down? A plain click selects nothing.
    pub dragged: bool,
}

impl Selection {
    /// The two ends, first in reading order first (you can drag backwards).
    pub fn ordered(&self) -> (Point, Point) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }

    /// The characters of line `seq` that are selected, if any.
    pub fn columns_in(&self, seq: u64) -> Option<Range<usize>> {
        if !self.dragged {
            return None;
        }
        let (first, last) = self.ordered();
        if seq < first.seq || seq > last.seq {
            return None;
        }
        // The first line from the start point on, the last up to and including the end point,
        // the lines between whole.
        let start = if seq == first.seq { first.col } else { 0 };
        let end = if seq == last.seq {
            last.col + 1
        } else {
            usize::MAX
        };
        Some(start..end)
    }

    /// The selected text. `rows` are the lines from the first to the last selected one.
    /// Trailing spaces are dropped from every line, as terminals do when copying.
    pub fn extract(&self, rows: &[String]) -> String {
        let (first, _) = self.ordered();
        rows.iter()
            .enumerate()
            .filter_map(|(i, row)| {
                let columns = self.columns_in(first.seq + i as u64)?;
                let text: String = row
                    .chars()
                    .skip(columns.start)
                    .take(columns.end.saturating_sub(columns.start))
                    .collect();
                Some(text.trim_end().to_string())
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection(anchor: (u64, usize), head: (u64, usize)) -> Selection {
        Selection {
            tile: 0,
            anchor: Point {
                seq: anchor.0,
                col: anchor.1,
            },
            head: Point {
                seq: head.0,
                col: head.1,
            },
            dragged: true,
        }
    }

    fn rows(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    #[test]
    fn selects_part_of_one_line() {
        let s = selection((10, 6), (10, 10));
        assert_eq!(s.extract(&rows(&["hello world again"])), "world");
        assert_eq!(s.columns_in(10), Some(6..11));
        assert_eq!(s.columns_in(9), None);
        assert_eq!(s.columns_in(11), None);
    }

    #[test]
    fn selects_across_lines_like_a_terminal_does() {
        // From column 4 of the first line to column 2 of the third.
        let s = selection((10, 4), (12, 2));
        let text = s.extract(&rows(&["one two three", "whole middle line", "abcdef"]));
        assert_eq!(text, "two three\nwhole middle line\nabc");
        assert_eq!(s.columns_in(11), Some(0..usize::MAX));
    }

    #[test]
    fn dragging_backwards_selects_the_same() {
        let forwards = selection((10, 6), (12, 2));
        let backwards = selection((12, 2), (10, 6));
        let lines = rows(&["one two three", "middle", "abcdef"]);
        assert_eq!(backwards.extract(&lines), forwards.extract(&lines));
        assert_eq!(backwards.ordered().0, Point { seq: 10, col: 6 });
    }

    #[test]
    fn trailing_spaces_and_short_lines_are_handled() {
        let s = selection((0, 4), (1, 40));
        assert_eq!(s.extract(&rows(&["abc def   ", "short"])), "def\nshort");
        // A column past the end of the line selects what there is.
        assert_eq!(selection((0, 50), (0, 60)).extract(&rows(&["tiny"])), "");
    }

    #[test]
    fn a_click_without_a_drag_selects_nothing() {
        let mut s = selection((5, 2), (5, 2));
        s.dragged = false;
        assert_eq!(s.columns_in(5), None);
        assert_eq!(s.extract(&rows(&["abcdef"])), "");
    }

    #[test]
    fn works_with_multibyte_characters() {
        let s = selection((0, 1), (0, 2));
        assert_eq!(s.extract(&rows(&["äöüß"])), "öü");
    }
}
