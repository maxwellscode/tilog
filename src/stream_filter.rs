use std::collections::VecDeque;
use std::ops::Range;
use std::time::{Duration, Instant};

use crate::buffer::RingBuffer;
use crate::filter::{EntryMatch, Filter};
use crate::filter_view::step_index;
use crate::group::GroupRule;
use crate::lines::Lines;

/// How many matching rows a filter on a stream keeps. A stream can't be read again, so what
/// matched has to be remembered, and memory has to be bounded.
const MATCH_CAPACITY: usize = 50_000;

/// A single entry longer than this many lines is cut there, so one runaway entry can't use
/// unbounded memory.
const MAX_ENTRY_LINES: usize = 5_000;

/// The same for the text of one entry: lines of up to 64 KiB each could otherwise add up to
/// hundreds of megabytes before the line limit is reached.
const MAX_ENTRY_BYTES: usize = 4 * 1024 * 1024;

/// How long without a new line before the last entry counts as complete.
const IDLE_RELEASE: Duration = Duration::from_millis(150);

/// A filtered view of a *stream* (ssh, docker, kubectl, ...).
///
/// For a file, a filter tile reads the file itself. A stream has no file to read, and opening a
/// second connection per filter would be wasteful, so these filters are fed, on the UI thread,
/// by the one connection the source already has: first with the lines in memory, then with
/// every new line. The price: only what tilog has seen is searched, and only the last
/// `MATCH_CAPACITY` matching rows are kept.
pub struct StreamFilterView {
    filter: Filter,
    rule: GroupRule,
    /// Characters at the start of every line that are not part of the log text (the source
    /// label of a merged timeline). They are shown, but never matched or used to group.
    prefix_cols: usize,
    /// Rows of the matching entries, oldest first. Sequence numbers start at 0.
    rows: RingBuffer,
    /// The entry being collected: its lines, and whether it matches so far.
    entry: Vec<String>,
    /// Bytes of text in `entry`.
    entry_bytes: usize,
    state: EntryMatch,
    last_line: Instant,
    entries: usize,
    /// The sequence number the source gave to the next line that is fed. Kept with every row
    /// (as its offset), so a match can be found again in the source.
    next_source_seq: u64,
    /// The row at which each kept entry begins, oldest first.
    starts: VecDeque<u64>,
    /// The row the entry `n` / `N` is at begins with.
    current: Option<u64>,
}

impl StreamFilterView {
    /// `first_source_seq` is the sequence number the source gave to the first line that will be
    /// fed; the following lines have the numbers after it.
    pub fn new(filter: Filter, rule: GroupRule, prefix_cols: usize, first_source_seq: u64) -> Self {
        Self {
            state: filter.start_entry(),
            filter,
            rule,
            prefix_cols,
            rows: RingBuffer::with_origin(MATCH_CAPACITY, 0),
            entry: Vec::new(),
            entry_bytes: 0,
            last_line: Instant::now(),
            entries: 0,
            next_source_seq: first_source_seq,
            starts: VecDeque::new(),
            current: None,
        }
    }

    pub fn filter(&self) -> &Filter {
        &self.filter
    }

    /// Number of matching entries seen.
    pub fn len(&self) -> usize {
        self.entries
    }

    /// Takes in the next line of the stream.
    pub fn feed(&mut self, line: &str) {
        let text = after_chars(line, self.prefix_cols);
        // A line either starts a new entry (completing the previous one) or continues it.
        let starts = self.rule.starts_entry(text.as_bytes());
        let too_big =
            self.entry.len() >= MAX_ENTRY_LINES || self.entry_bytes + line.len() > MAX_ENTRY_BYTES;
        if !self.entry.is_empty() && (starts || too_big) {
            self.finish();
        }
        if self.entry.is_empty() {
            self.state.clear();
        }
        self.state.feed(&self.filter, text.as_bytes());
        self.entry_bytes += line.len();
        self.entry.push(line.to_string());
        self.next_source_seq += 1;
        self.last_line = Instant::now();
    }

    /// Called regularly: an entry that nothing has followed for a moment is complete.
    pub fn tick(&mut self) {
        if !self.entry.is_empty() && self.last_line.elapsed() >= IDLE_RELEASE {
            self.finish();
        }
    }

    fn finish(&mut self) {
        if self.state.is_match() {
            // The lines of the entry are the last ones fed.
            let first_source = self.next_source_seq - self.entry.len() as u64;
            self.starts.push_back(self.rows.end_seq());
            for (i, line) in self.entry.drain(..).enumerate() {
                self.rows.push(first_source + i as u64, line);
            }
            self.entries += 1;
            self.entry_bytes = 0;
            // Entries whose first row has been pushed out are gone.
            while self
                .starts
                .front()
                .is_some_and(|&row| row < self.rows.first_seq())
            {
                self.starts.pop_front();
            }
        } else {
            self.entry.clear();
            self.entry_bytes = 0;
        }
    }
}

impl StreamFilterView {
    /// Moves to the next (`forward`) or previous entry: see `FilterView::step`.
    pub fn step(&mut self, forward: bool, anchor_row: u64) -> Option<(usize, bool)> {
        let stepped = step_index(
            self.starts.len(),
            self.current_index(),
            forward,
            anchor_row,
            |i| self.starts[i],
        )?;
        self.current = Some(self.starts[stepped.0]);
        Some(stepped)
    }

    /// Forgets the entry `n` / `N` stopped at: the next step starts from the view again.
    pub fn forget_current(&mut self) {
        self.current = None;
    }

    /// The entry `n` / `N` is at, as its position among the kept ones.
    pub fn current_index(&self) -> Option<usize> {
        let row = self.current?;
        self.starts.binary_search(&row).ok()
    }

    /// How many entries are kept (all of them seen, unless more than the capacity matched).
    pub fn kept(&self) -> usize {
        self.starts.len()
    }

    /// The first display row of entry `index`.
    pub fn first_row(&self, index: usize) -> u64 {
        self.starts[index]
    }

    /// Where entry `index` starts in the source (its sequence number there), and how many lines
    /// it has.
    pub fn entry(&self, index: usize) -> (u64, usize) {
        let row = self.starts[index];
        let end = self
            .starts
            .get(index + 1)
            .copied()
            .unwrap_or(self.rows.end_seq());
        let source = self.rows.offset_at(row).unwrap_or(0);
        (source, usize::try_from(end - row).unwrap_or(usize::MAX))
    }

    /// The entry that contains display row `row`.
    pub fn entry_of_row(&self, row: u64) -> Option<usize> {
        self.starts
            .partition_point(|&start| start <= row)
            .checked_sub(1)
    }

    /// The display rows of the entry `n` / `N` is at.
    pub fn current_rows(&self) -> Option<Range<u64>> {
        let index = self.current_index()?;
        let first = self.starts[index];
        Some(first..first + self.entry(index).1 as u64)
    }
}

/// `text` without its first `n` characters.
fn after_chars(text: &str, n: usize) -> &str {
    if n == 0 {
        return text;
    }
    text.char_indices()
        .nth(n)
        .map_or("", |(byte, _)| &text[byte..])
}

impl Lines for StreamFilterView {
    fn first_seq(&self) -> u64 {
        self.rows.first_seq()
    }

    fn end_seq(&self) -> u64 {
        self.rows.end_seq()
    }

    fn range(&self, from_seq: u64, count: usize) -> Vec<String> {
        self.rows.range(from_seq, count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(pattern: &str, rule: GroupRule) -> StreamFilterView {
        StreamFilterView::new(
            Filter::new(None, pattern, false, false).unwrap(),
            rule,
            0,
            0,
        )
    }

    #[test]
    fn a_label_prefix_is_shown_but_neither_matched_nor_grouped() {
        let mut view = StreamFilterView::new(
            Filter::new(None, "web", false, false).unwrap(), // "web" is only in the label
            GroupRule::Auto,
            7, // the width of "web  │ "
            0,
        );
        feed_all(
            &mut view,
            &[
                "web  │ INFO a",
                "web  │ \tat trace",
                "db   │ web mentioned",
                "db   │ other",
            ],
        );
        // Only the line whose *text* contains "web" matches, and the label didn't make
        // the indented line look like a new entry.
        assert_eq!(view.range(0, 10), ["db   │ web mentioned"]);

        let mut grouped = StreamFilterView::new(
            Filter::new(None, "trace", false, false).unwrap(),
            GroupRule::Auto,
            7,
            0,
        );
        feed_all(
            &mut grouped,
            &["web  │ ERROR x", "web  │ \tat trace", "web  │ INFO y"],
        );
        assert_eq!(
            grouped.range(0, 10),
            ["web  │ ERROR x", "web  │ \tat trace"]
        );
    }

    fn feed_all(view: &mut StreamFilterView, lines: &[&str]) {
        for line in lines {
            view.feed(line);
        }
        // Let the last entry settle, as the idle timer does in the running program.
        std::thread::sleep(IDLE_RELEASE + Duration::from_millis(20));
        view.tick();
    }

    #[test]
    fn keeps_whole_matching_entries() {
        let mut view = view("boom", GroupRule::Auto);
        feed_all(
            &mut view,
            &[
                "INFO a",
                "SEVERE x",
                "\tat y.Z(Z.java:1)",
                "INFO b",
                "WARN boom",
                "\tat q",
                "INFO c",
            ],
        );
        assert_eq!(view.len(), 1);
        assert_eq!(view.range(0, 10), ["WARN boom", "\tat q"]);
    }

    #[test]
    fn a_match_deep_in_a_trace_keeps_the_header() {
        let mut view = view("Timeout", GroupRule::Auto);
        feed_all(
            &mut view,
            &[
                "SEVERE failed",
                "\tjava.net.SocketTimeoutException",
                "\tat x",
                "INFO ok",
            ],
        );
        assert_eq!(
            view.range(0, 10),
            [
                "SEVERE failed",
                "\tjava.net.SocketTimeoutException",
                "\tat x"
            ]
        );
    }

    #[test]
    fn the_last_entry_waits_for_the_idle_timer() {
        let mut view = view("hit", GroupRule::Auto);
        view.feed("hit one");
        view.tick();
        assert_eq!(view.len(), 0, "the entry could still grow");
        std::thread::sleep(IDLE_RELEASE + Duration::from_millis(20));
        view.tick();
        assert_eq!(view.len(), 1);
    }

    #[test]
    fn off_means_line_by_line_and_old_rows_are_dropped() {
        let mut view = view("x", GroupRule::Off);
        feed_all(&mut view, &["x1", "\tnot", "x2"]);
        assert_eq!(view.range(0, 10), ["x1", "x2"]);

        let mut many = view_with_small_memory();
        for i in 0..5 {
            many.rows.push(0, format!("row {i}"));
        }
        assert_eq!(
            many.range(many.first_seq(), 10),
            ["row 2", "row 3", "row 4"]
        );
    }

    fn view_with_small_memory() -> StreamFilterView {
        let mut v = view("x", GroupRule::Off);
        v.rows = RingBuffer::with_origin(3, 0);
        v
    }
}
