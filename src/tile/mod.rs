//! One window on screen: some content plus its own scroll state.
//!
//! The pieces live in submodules, each adding methods to `Tile`: `scroll` (moving the view),
//! `search` (finding text, stepping through filter matches), `window` (showing a stretch of the
//! file that is not its live end), `title`, and `content` (where the lines come from).

mod content;
mod scroll;
mod search;
#[cfg(test)]
mod tests;
mod time;
mod title;
mod window;
mod write;

use std::cell::Cell;
use std::ops::Range;
use std::time::Instant;

use anyhow::{Context, Result};

use crate::buffer::RingBuffer;
use crate::filter::Filter;
use crate::filter_view::FilterView;
use crate::group::GroupRule;
use crate::history;
use crate::lines::Lines;
use crate::merge::{self, MemberInit, Merger};
use crate::spec::CommandSpec;
use crate::stream;
use crate::stream_filter::StreamFilterView;
use crate::tail::{self, Status, TailMsg};
use crate::viewport::Viewport;

pub use search::Find;

use content::Content;
use window::HISTORY_MAX;

/// Per UI tick, take at most this many lines from the main source, so a huge file being
/// loaded can't freeze the UI.
const MAX_LINES_PER_PUMP: usize = 4096;

/// The height assumed for a tile that has not been drawn yet.
const DEFAULT_HEIGHT: usize = 20;

/// Rows kept by a merged timeline: more than one source's worth.
const MERGED_CAPACITY: usize = 20_000;

/// The most rows one selection copies, so dragging over a huge log can't stall the program.
const MAX_COPY_ROWS: usize = 200_000;

/// One window on screen: some content plus its own scroll state.
pub struct Tile {
    content: Content,
    view: Viewport,
    /// How many columns the text is scrolled to the left.
    hscroll: usize,
    /// The row the last search jumped to, so `n` knows where to continue from.
    match_row: Option<u64>,
    /// The same for `]` / `[`, so they and `n` / `N` each continue from their own last stop.
    error_row: Option<u64>,
    /// Rows to highlight in the main tile: the place a filter's `n` / `N` brought you to.
    marked: Option<Range<u64>>,
    /// How many rows the tile was last drawn with, so `pause` can freeze the view without
    /// being told the size of the screen.
    shown_height: Cell<usize>,
}

impl Tile {
    fn new(content: Content) -> Self {
        Self {
            content,
            view: Viewport::new(),
            hscroll: 0,
            match_row: None,
            error_row: None,
            marked: None,
            shown_height: Cell::new(DEFAULT_HEIGHT),
        }
    }

    /// The main tile: follows `path`, keeping the newest `capacity` lines.
    ///
    /// Opens the file *at its end*: it finds where the last `capacity` lines begin by reading
    /// backwards, and starts there. A multi-gigabyte file opens as fast as a small one.
    pub fn source(path: &str, capacity: usize) -> Result<Self> {
        let start =
            history::tail_start(path, capacity).with_context(|| format!("cannot open {path}"))?;
        let rx = tail::spawn(path, start)?;
        let content = Content::Source {
            path: Some(path.to_string()),
            live_capacity: capacity,
            lines: RingBuffer::new(capacity),
            rx,
            status: None,
            _guard: None,
            window: None,
        };
        Ok(Self::new(content))
    }

    /// The main tile of a command (ssh, docker, kubectl, ...): starts it, keeps its newest
    /// `capacity` lines, and restarts it when it drops.
    pub fn stream(spec: CommandSpec, capacity: usize) -> Self {
        let (rx, guard) = stream::spawn(spec, capacity);
        let content = Content::Source {
            path: None,
            live_capacity: capacity,
            lines: RingBuffer::new(capacity),
            rx,
            status: Some(Status::Connecting),
            _guard: Some(guard),
            window: None,
        };
        Self::new(content)
    }

    /// A filter tile for a command's output. It is fed with `backfill` (the lines already in
    /// memory) now, and with every new line later through `feed`.
    /// `prefix_cols` characters at the start of each line are not log text (see `label_cols`).
    pub fn stream_filtered(
        filter: Filter,
        rule: GroupRule,
        backfill: &[String],
        prefix_cols: usize,
    ) -> Self {
        let mut view = StreamFilterView::new(filter, rule, prefix_cols);
        for line in backfill {
            view.feed(line);
        }
        Self::new(Content::StreamFilter(view))
    }

    /// A tile that interleaves several sources by timestamp. Starts with the lines they already
    /// have in memory, merged; new lines arrive through `merge_feed`.
    pub fn merged(members: Vec<MemberInit>) -> Self {
        let (merger, rows) = Merger::start(members);
        let width = merger.label_width();
        let mut lines = RingBuffer::new(MERGED_CAPACITY);
        for (member, text) in rows {
            lines.push(0, merge::format_row(merger.label(member), width, &text));
        }
        let content = Content::Merged {
            lines,
            label_cols: merge::row_prefix_len(width),
            merger,
        };
        Self::new(content)
    }

    /// New lines of the source `id`, for a merged tile. Other tiles ignore them.
    pub fn merge_feed(&mut self, id: u64, lines: &[String]) {
        if let Content::Merged { merger, .. } = &mut self.content {
            let now = Instant::now();
            for line in lines {
                merger.feed(id, line, now);
            }
        }
    }

    /// Characters at the start of every row that are a source label, not log text. 0 unless
    /// this is a merged tile.
    pub fn label_cols(&self) -> usize {
        match &self.content {
            Content::Merged { label_cols, .. } => *label_cols,
            _ => 0,
        }
    }

    /// Every line currently in memory, oldest first (what a new stream filter starts from).
    pub fn snapshot(&self) -> Vec<String> {
        let lines = self.content.lines();
        let count = usize::try_from(lines.end_seq() - lines.first_seq()).unwrap_or(usize::MAX);
        lines.range(lines.first_seq(), count)
    }

    /// Does this tile need to be shown each new line of the source (a stream filter)?
    pub fn wants_lines(&self) -> bool {
        matches!(self.content, Content::StreamFilter(_))
    }

    /// Shows new lines of the source to a stream filter. Other tiles ignore them.
    pub fn feed(&mut self, lines: &[String]) {
        if let Content::StreamFilter(view) = &mut self.content {
            for line in lines {
                view.feed(line);
            }
        }
    }

    /// A tile showing every entry of `path` that matches `filter`, from the start of the file.
    /// `rule` says which lines belong together as one entry.
    pub fn filtered(path: &str, filter: Filter, rule: GroupRule) -> Result<Self> {
        let content = Content::Filter(FilterView::start(path, filter, rule)?);
        Ok(Self::new(content))
    }

    /// Takes in what the background thread has delivered. Returns `true` if more may be
    /// waiting, so the UI should come back right away instead of idling.
    pub fn pump(&mut self) -> bool {
        self.pump_collecting(None)
    }

    /// Like `pump`, and also copies every new line into `fresh`, so the source can pass them on
    /// to its stream filters.
    pub fn pump_collecting(&mut self, mut fresh: Option<&mut Vec<String>>) -> bool {
        let following = self.view.is_following();
        match &mut self.content {
            Content::Source {
                live_capacity,
                lines,
                rx,
                status: current,
                ..
            } => {
                // Older lines loaded by scrolling up are kept only while the view is paused.
                // Following the file again shrinks the buffer back to its live size.
                lines.set_capacity(if following {
                    *live_capacity
                } else {
                    *live_capacity + HISTORY_MAX
                });

                for _ in 0..MAX_LINES_PER_PUMP {
                    match rx.try_recv() {
                        Ok(TailMsg::Line { offset, text }) => {
                            if let Some(fresh) = fresh.as_deref_mut() {
                                fresh.push(text.clone());
                            }
                            lines.push(offset, text);
                        }
                        Ok(TailMsg::Rotated) => lines.rotated(),
                        Ok(TailMsg::Status(status)) => *current = Some(status),
                        Err(_) => return false,
                    }
                }
                true
            }
            Content::Filter(view) => view.pump(),
            Content::StreamFilter(view) => {
                view.tick();
                false
            }
            Content::Merged { lines, merger, .. } => {
                // Lines whose turn has come, in time order.
                let width = merger.label_width();
                for (member, text) in merger.release(Instant::now()) {
                    let row = merge::format_row(merger.label(member), width, &text);
                    if let Some(fresh) = fresh.as_deref_mut() {
                        fresh.push(row.clone());
                    }
                    lines.push(0, row);
                }
                false
            }
        }
    }

    /// The filter of a filtered tile, `None` for the main tile.
    pub fn filter(&self) -> Option<&Filter> {
        match &self.content {
            Content::Source { .. } | Content::Merged { .. } => None,
            Content::Filter(view) => Some(view.filter()),
            Content::StreamFilter(view) => Some(view.filter()),
        }
    }

    /// The text rows to draw in a tile `height` rows tall, made safe to draw.
    pub fn visible(&self, height: usize) -> Vec<String> {
        self.shown_height.set(height.max(1));
        let lines = self.content.lines();
        let rows = lines.range(self.view.top(lines, height), height);
        rows.iter().map(|row| printable(row)).collect()
    }

    /// Lines received from the file since tilog started (for a filter tile: rows shown).
    pub fn total_lines(&self) -> u64 {
        match &self.content {
            Content::Source { lines, .. } | Content::Merged { lines, .. } => lines.received(),
            Content::Filter(view) => view.end_seq(),
            Content::StreamFilter(view) => view.end_seq(),
        }
    }

    /// Number of matching entries, for filter tiles.
    pub fn matches(&self) -> Option<usize> {
        match &self.content {
            Content::Source { .. } | Content::Merged { .. } => None,
            Content::Filter(view) => Some(view.len()),
            Content::StreamFilter(view) => Some(view.len()),
        }
    }

    /// Does the view stick to the newest line? (Shown next to the title, not in it.)
    pub fn is_following(&self) -> bool {
        self.view.is_following()
    }

    /// Sequence number of the first line on screen, for a tile `height` rows tall.
    pub fn top_seq(&self, height: usize) -> u64 {
        self.view.top(self.content.lines(), height)
    }

    /// Sequence number of the newest line, if there is any.
    pub fn last_seq(&self) -> Option<u64> {
        let lines = self.content.lines();
        (lines.end_seq() > lines.first_seq()).then(|| lines.end_seq() - 1)
    }

    /// The text of lines `from_seq` to `to_seq` (inclusive), as it is shown.
    pub fn text_between(&self, from_seq: u64, to_seq: u64) -> Vec<String> {
        let count = usize::try_from(to_seq.saturating_sub(from_seq) + 1).unwrap_or(usize::MAX);
        let rows = self
            .content
            .lines()
            .range(from_seq, count.min(MAX_COPY_ROWS));
        rows.iter().map(|row| printable(row)).collect()
    }

    /// Empties the tile. Only the main tile can be cleared; returns `false` otherwise.
    pub fn clear(&mut self) -> bool {
        match &mut self.content {
            Content::Source { lines, window, .. } => {
                *window = None;
                lines.clear();
                self.marked = None;
                true
            }
            Content::Merged { lines, .. } => {
                lines.clear();
                true
            }
            Content::Filter(_) | Content::StreamFilter(_) => false,
        }
    }
}

/// Makes a log line safe to draw. Tabs (stack traces) become spaces, and other control
/// characters, such as the ESC of color codes, would garble the terminal, so they are shown
/// as `·`.
fn printable(line: &str) -> String {
    line.chars()
        .map(|c| match c {
            '\t' => "    ".to_string(),
            c if c.is_control() => "·".to_string(),
            c => c.to_string(),
        })
        .collect()
}
