//! The main tile of a file can show a stretch of the file that is not its live end: older
//! lines loaded by scrolling up, or the place a filter's `n` / `N` jumps to.

use std::fs::File;

use crate::buffer::RingBuffer;
use crate::history;
use crate::lines::Lines;

use super::search::Link;
use super::{Content, Tile};

/// How many older lines are read from disk each time the user scrolls above the oldest one.
pub(super) const HISTORY_CHUNK: usize = 2_000;

/// Most older lines kept on top of the live ones. They are only kept while the view is paused;
/// following the file again releases them, so memory use falls back to the live size.
pub(super) const HISTORY_MAX: usize = 50_000;

/// Lines loaded before a place that is jumped to, and after it (including the place itself).
const CONTEXT_BEFORE: usize = 200;
const CONTEXT_AFTER: usize = 400;

/// Lines read more when scrolling reaches the edge of such a window, and the most it may hold.
const WINDOW_CHUNK: usize = 400;
const WINDOW_CAPACITY: usize = 20_000;

/// A stretch of a file that is not its live end: what the main tile shows after jumping to a
/// match far back in the file. The live end keeps filling in behind it; `G` drops the window.
pub(super) struct HistoryWindow {
    pub(super) lines: RingBuffer,
    /// Where reading continues when the window is scrolled past its last line.
    pub(super) next_offset: u64,
}

/// Reads the stretch of `path` around byte `offset` (the start of a line): some lines before it
/// and many after. `None` if the file can't be read.
fn load_window(path: &str, offset: u64, floor: u64) -> Option<HistoryWindow> {
    let mut file = File::open(path).ok()?;
    let start = history::start_of_last_lines(&mut file, offset, CONTEXT_BEFORE)
        .ok()?
        .max(floor);
    let (found, next_offset) =
        history::read_from(path, start, CONTEXT_BEFORE + CONTEXT_AFTER).ok()?;
    let mut lines = RingBuffer::new(WINDOW_CAPACITY);
    for (offset, text) in found {
        lines.push(offset, text);
    }
    Some(HistoryWindow { lines, next_offset })
}

impl Tile {
    /// Reads a chunk of older lines from disk and puts it in front of the loaded ones.
    pub(super) fn load_older(&mut self) {
        let Content::Source {
            path: Some(path),
            live_capacity,
            lines,
            window,
            ..
        } = &mut self.content
        else {
            return; // a filter, or a command: there is no older part of a file to read
        };
        if let Some(window) = window {
            let want = WINDOW_CHUNK.min(WINDOW_CAPACITY.saturating_sub(window.lines.len()));
            let Some(front) = window.lines.front_offset() else {
                return;
            };
            if want > 0
                && front > lines.floor()
                && let Ok(older) = history::read_before(path, front, lines.floor(), want)
            {
                window.lines.prepend(older);
            }
            return;
        }
        let max = *live_capacity + HISTORY_MAX;
        lines.set_capacity(max); // room first: the view is about to be paused
        let want = HISTORY_CHUNK.min(max.saturating_sub(lines.len()));
        let Some(front) = lines.front_offset() else {
            return;
        };
        if want == 0 || !lines.has_older() {
            return;
        }
        // A read error (the file vanished?) just means no older lines.
        if let Ok(older) = history::read_before(path, front, lines.floor(), want) {
            lines.prepend(older);
        }
    }

    /// In a history window: reads on from disk when the view is about to pass its last line.
    pub(super) fn extend_window(&mut self, height: usize, n: u64) {
        let Content::Source {
            path: Some(path),
            window: Some(window),
            ..
        } = &mut self.content
        else {
            return;
        };
        let needed = self.view.top(&window.lines, height) + height as u64 + n;
        if needed < window.lines.end_seq() {
            return;
        }
        if let Ok((more, next)) = history::read_from(path, window.next_offset, WINDOW_CHUNK) {
            for (offset, text) in more {
                window.lines.push(offset, text);
            }
            window.next_offset = next;
        }
    }

    /// Drops the history window (and what was marked in it): the tile shows the live end again.
    pub(super) fn leave_window(&mut self) {
        if let Content::Source { window, .. } = &mut self.content
            && window.take().is_some()
        {
            self.marked = None;
        }
    }

    /// Is the tile showing a stretch of the file loaded from disk instead of its live end?
    pub fn is_history_view(&self) -> bool {
        matches!(
            self.content,
            Content::Source {
                window: Some(_),
                ..
            }
        )
    }

    /// Main tile: shows the entry of a filter that is at `at` in this source (`rows` lines).
    /// `false` if it can't be shown.
    pub fn show_link(&mut self, at: Link, rows: usize, height: usize) -> bool {
        match at {
            Link::Offset(offset) => self.show_offset(offset, rows, height),
            Link::Seq(seq) => self.show_seq(seq, rows, height),
        }
    }

    /// Main tile of a command or a timeline: shows the lines that start at sequence number `seq`,
    /// marked. `false` if they are no longer held (the buffer is bounded).
    fn show_seq(&mut self, seq: u64, rows: usize, height: usize) -> bool {
        let lines = self.content.lines();
        if !(lines.first_seq()..lines.end_seq()).contains(&seq) {
            return false;
        }
        self.show_row(seq, height);
        self.marked = Some(seq..seq + rows as u64);
        true
    }

    /// Main tile of a file: shows the entry starting at byte `offset` (`rows` lines), with
    /// context above it, and marks it. Uses the loaded lines when it is among them, otherwise
    /// reads that stretch of the file from disk into a window. `false` if there is no file.
    pub fn show_offset(&mut self, offset: u64, rows: usize, height: usize) -> bool {
        let Content::Source {
            path: Some(path),
            lines,
            window,
            ..
        } = &mut self.content
        else {
            return false;
        };
        let mut seq = window.as_ref().and_then(|w| w.lines.seq_of_offset(offset));
        if seq.is_none() && window.is_none() {
            seq = lines.seq_of_offset(offset);
        }
        if seq.is_none() {
            let Some(loaded) = load_window(path, offset, lines.floor()) else {
                return false;
            };
            seq = loaded.lines.seq_of_offset(offset);
            *window = Some(loaded);
        }
        let Some(seq) = seq else { return false };
        self.reveal(seq);
        self.marked = Some(seq..seq + rows as u64);
        self.view.jump_to(seq.saturating_sub(height as u64 / 3));
        true
    }
}
