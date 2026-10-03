use std::time::{Duration, Instant};

use crate::buffer::RingBuffer;
use crate::filter::{EntryMatch, Filter};
use crate::group::GroupRule;
use crate::lines::Lines;

/// How many matching rows a filter on a stream keeps. A stream can't be read again, so what
/// matched has to be remembered, and memory has to be bounded.
const MATCH_CAPACITY: usize = 50_000;

/// A single entry longer than this many lines is cut there, so one runaway entry can't use
/// unbounded memory.
const MAX_ENTRY_LINES: usize = 5_000;

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
    state: EntryMatch,
    last_line: Instant,
    entries: usize,
}

impl StreamFilterView {
    pub fn new(filter: Filter, rule: GroupRule, prefix_cols: usize) -> Self {
        Self {
            state: filter.start_entry(),
            filter,
            rule,
            prefix_cols,
            rows: RingBuffer::with_origin(MATCH_CAPACITY, 0),
            entry: Vec::new(),
            last_line: Instant::now(),
            entries: 0,
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
        if !self.entry.is_empty() && (starts || self.entry.len() >= MAX_ENTRY_LINES) {
            self.finish();
        }
        if self.entry.is_empty() {
            self.state.clear();
        }
        self.state.feed(&self.filter, text.as_bytes());
        self.entry.push(line.to_string());
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
            for line in self.entry.drain(..) {
                self.rows.push(0, line);
            }
            self.entries += 1;
        } else {
            self.entry.clear();
        }
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
        StreamFilterView::new(Filter::new(None, pattern, false, false).unwrap(), rule, 0)
    }

    #[test]
    fn a_label_prefix_is_shown_but_neither_matched_nor_grouped() {
        let mut view = StreamFilterView::new(
            Filter::new(None, "web", false, false).unwrap(), // "web" is only in the label
            GroupRule::Auto,
            7, // the width of "web  │ "
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
