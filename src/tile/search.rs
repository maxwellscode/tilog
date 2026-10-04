//! Looking for text in a tile, and stepping through a filter's matches.

use std::mem;
use std::ops::Range;

use crate::highlight::Highlight;
use crate::lines::Lines;
use crate::severity;

use super::frozen::Bounded;
use super::{Content, Tile};

/// Where a search starts, relative to the view.
#[derive(Clone, Copy, PartialEq)]
pub enum Find {
    /// The first match at or below the top of the view.
    First,
    /// The match after the current one (or the first below the top).
    Next,
    /// The match before the current one (or before the top).
    Previous,
}

/// Where an entry of a filter lies in the source it was made from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Link {
    /// A byte offset in a file.
    Offset(u64),
    /// The sequence number of a line of a command's output, or of a merged timeline.
    Seq(u64),
}

/// Where `step_match` landed: the entry's place in the source.
pub struct MatchStep {
    pub at: Link,
    pub rows: usize,
    /// Position among the matches, and how many there are.
    pub index: usize,
    pub total: usize,
    pub wrapped: bool,
}

/// Rows read per step while searching.
const SEARCH_CHUNK: usize = 256;

/// A search gives up after looking at this many rows, so one key press can't hang the screen.
const SEARCH_LIMIT: u64 = 500_000;

impl Tile {
    /// Moves the view to a row containing a match of `highlight`.
    ///
    /// Returns `None` (and moves nothing) if there is no match in what is loaded, otherwise
    /// whether the search had to wrap around the start or end to find one.
    ///
    /// While the view follows the end of a log, the first search finds the *newest* match,
    /// because that is what you are usually after; otherwise it looks down from the top row.
    pub fn find_match(
        &mut self,
        highlight: &Highlight,
        height: usize,
        direction: Find,
    ) -> Option<bool> {
        let bounded = Bounded::of(&self.content, self.frozen_end);
        let lines: &dyn Lines = &bounded;
        let (first, end) = (lines.first_seq(), lines.end_seq());
        let top = self.view.top(lines, height);
        let following = self.view.is_following();
        // The match the last jump landed on, if it is still on screen.
        let current = self
            .match_row
            .filter(|row| (top..top + height as u64).contains(row));

        let forward = |from: u64| match scan_forward(lines, highlight, from) {
            Some(row) => Some((row, false)),
            None => scan_forward(lines, highlight, first).map(|row| (row, true)), // wrap
        };
        let (row, wrapped) = match direction {
            Find::First if self.view.is_following() => {
                scan_backward(lines, highlight, end).map(|row| (row, false))?
            }
            Find::First => forward(top)?,
            Find::Next => forward(current.map_or(top, |row| row + 1))?,
            Find::Previous => match scan_backward(
                lines,
                highlight,
                current.unwrap_or(if following { end } else { top }),
            ) {
                Some(row) => (row, false),
                None => (scan_backward(lines, highlight, end)?, true), // wrap
            },
        };

        self.match_row = Some(row);
        self.marked = None; // the search's own marker takes over
        // A third of the way down, so there is context above the match.
        self.view.jump_to(row.saturating_sub(height as u64 / 3));
        Some(wrapped)
    }

    /// The row the last search jumped to (it may be off screen, or from an older search).
    pub fn match_row(&self) -> Option<u64> {
        self.match_row
    }

    /// Moves to the next (`forward`) or previous line that looks like an error and marks it.
    /// Same result as `find_match`.
    pub fn find_error(&mut self, forward: bool, height: usize) -> Option<bool> {
        let direction = if forward { Find::Next } else { Find::Previous };
        // `find_match` continues from `match_row`: lend it the error position, then give the
        // search its own back.
        let search_row = mem::replace(&mut self.match_row, self.error_row);
        let found = self.find_match(severity::errors(), height, direction);
        self.error_row = mem::replace(&mut self.match_row, search_row);
        let wrapped = found?;
        self.marked = self.error_row.map(|row| row..row + 1);
        Some(wrapped)
    }

    /// Is this a filter, whose entries `n` / `N` can walk through and the main tile can follow?
    pub fn is_filter(&self) -> bool {
        matches!(self.content, Content::Filter(_) | Content::StreamFilter(_))
    }

    /// Rows to draw highlighted: the entry `n` / `N` is at (in a filter, among its rows; in the
    /// main tile, the place the filter brought it to).
    pub fn marked_rows(&self) -> Option<Range<u64>> {
        match &self.content {
            Content::Filter(view) => view.current_rows().or_else(|| self.marked.clone()),
            Content::StreamFilter(view) => view.current_rows().or_else(|| self.marked.clone()),
            _ => self.marked.clone(),
        }
    }

    /// Filters: moves to the next (`forward`) or previous entry, shows it in the filter's own
    /// pane, and returns where it is in the source, so the main tile can show it too.
    pub fn step_match(&mut self, forward: bool, height: usize) -> Option<MatchStep> {
        let anchor = self.top_seq(height);
        let (index, wrapped, at, rows, first, total) = match &mut self.content {
            Content::Filter(view) => {
                let (index, wrapped) = view.step(forward, anchor)?;
                let (offset, rows) = view.entry(index);
                (
                    index,
                    wrapped,
                    Link::Offset(offset),
                    rows,
                    view.first_row(index),
                    view.len(),
                )
            }
            Content::StreamFilter(view) => {
                let (index, wrapped) = view.step(forward, anchor)?;
                let (seq, rows) = view.entry(index);
                (
                    index,
                    wrapped,
                    Link::Seq(seq),
                    rows,
                    view.first_row(index),
                    view.kept(),
                )
            }
            _ => return None,
        };
        let last = first + rows as u64;
        let top = self
            .view
            .top(&Bounded::of(&self.content, self.frozen_end), height);
        if first < top || last > top + height as u64 {
            self.view.jump_to(first.saturating_sub(height as u64 / 3));
        }
        Some(MatchStep {
            at,
            rows,
            index,
            total,
            wrapped,
        })
    }

    /// Filters: where the entry that contains the row the last search jumped to lies in the
    /// source, and how many lines it has.
    pub fn link_of_match_row(&self) -> Option<(Link, usize)> {
        let row = self.match_row?;
        match &self.content {
            Content::Filter(view) => {
                let (offset, rows) = view.entry(view.entry_of_row(row)?);
                Some((Link::Offset(offset), rows))
            }
            Content::StreamFilter(view) => {
                let (seq, rows) = view.entry(view.entry_of_row(row)?);
                Some((Link::Seq(seq), rows))
            }
            _ => None,
        }
    }
}

/// The first row at or after `start` that contains a match.
fn scan_forward(lines: &dyn Lines, highlight: &Highlight, start: u64) -> Option<u64> {
    let end = lines.end_seq();
    let start = start.max(lines.first_seq());
    let mut seq = start;
    while seq < end && seq - start < SEARCH_LIMIT {
        let count = SEARCH_CHUNK.min(usize::try_from(end - seq).unwrap_or(SEARCH_CHUNK));
        let rows = lines.range(seq, count);
        if let Some(i) = rows.iter().position(|row| highlight.is_match(row)) {
            return Some(seq + i as u64);
        }
        seq += count as u64;
    }
    None
}

/// The last row *before* `before` that contains a match.
fn scan_backward(lines: &dyn Lines, highlight: &Highlight, before: u64) -> Option<u64> {
    let first = lines.first_seq();
    let mut to = before.min(lines.end_seq());
    let mut scanned = 0;
    while to > first && scanned < SEARCH_LIMIT {
        let from = to.saturating_sub(SEARCH_CHUNK as u64).max(first);
        let rows = lines.range(from, usize::try_from(to - from).unwrap_or(SEARCH_CHUNK));
        if let Some(i) = rows.iter().rposition(|row| highlight.is_match(row)) {
            return Some(from + i as u64);
        }
        scanned += to - from;
        to = from;
    }
    None
}
