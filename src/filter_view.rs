use std::fs::File;
use std::io::{BufReader, Seek, SeekFrom};
use std::mem;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::filter::{EntryMatch, Filter};
use crate::group::GroupRule;
use crate::line::CappedLine;
use crate::lines::Lines;
use crate::tail::{self, CHANNEL_BOUND, LineSink};

/// How many matching entries the scanner collects before sending them to the UI in one message.
const BATCH: usize = 4096;

/// Per UI tick, handle at most this many messages, so a big backlog can't freeze the UI.
const MAX_MESSAGES_PER_PUMP: usize = 64;

/// How long the end of the file must stay quiet before the last entry counts as complete.
/// Measured in time, not in wake-ups: watching a busy directory wakes the scanner often.
const ENTRY_QUIET: Duration = Duration::from_millis(140);

/// The most matching entries a file filter keeps (16 bytes each: 64 MB). A filter that matches
/// nearly every line of a huge file would otherwise grow with the file. The ones found first
/// are kept; the title says when there were more.
const MAX_MATCHES: usize = 4_000_000;

/// What the scanner thread tells the UI.
pub enum ScanMsg {
    /// Matching entries, in file order: where each starts (byte offset) and how many lines it has.
    Matches(Vec<(u64, u32)>),
    /// The file was truncated: forget all entries, a new scan starts.
    Reset,
    /// The scanner has read to the end of the file (and keeps following it).
    CaughtUp,
    /// More entries match than are kept: the rest is dropped.
    Limit,
}

/// The entry being read right now. Its lines are not kept, only where it began and how long it is.
struct Pending {
    offset: u64,
    lines: u32,
}

/// Runs on the scanner thread: cuts the file into entries, tests each entry against the filter,
/// and reports the ones that match.
struct FilterSink {
    filter: Filter,
    rule: GroupRule,
    pending: Option<Pending>,
    /// Which conditions the pending entry has satisfied so far.
    state: EntryMatch,
    /// When an end of file was first reached while an entry was pending.
    held_since: Option<Instant>,
    /// Entries reported so far, and the most that are (see `MAX_MATCHES`).
    reported: usize,
    limit: usize,
    /// The UI was told that the limit was reached.
    limit_told: bool,
    batch: Vec<(u64, u32)>,
    tx: SyncSender<ScanMsg>,
}

impl FilterSink {
    /// The pending entry is complete: report it if it matched.
    fn finish_entry(&mut self) -> bool {
        let Some(entry) = self.pending.take() else {
            return true;
        };
        if self.state.is_match() {
            if self.reported >= self.limit {
                // Over the limit: say so once, keep nothing more.
                let first_time = !mem::replace(&mut self.limit_told, true);
                return !first_time || self.tx.send(ScanMsg::Limit).is_ok();
            }
            self.reported += 1;
            self.batch.push((entry.offset, entry.lines));
            if self.batch.len() >= BATCH {
                return self.flush();
            }
        }
        true
    }

    fn flush(&mut self) -> bool {
        if self.batch.is_empty() {
            return true;
        }
        // `mem::take` moves the Vec out and leaves an empty one behind: the batch is
        // handed over to the channel without copying, and we can keep filling a fresh one.
        self.tx
            .send(ScanMsg::Matches(mem::take(&mut self.batch)))
            .is_ok()
    }
}

impl LineSink for FilterSink {
    fn line(&mut self, offset: u64, line: &[u8]) -> bool {
        self.held_since = None;

        // A line either starts a new entry (completing the previous one) or continues it.
        // A continuation line with no entry before it (right after a flush, or at the start
        // of the file) starts one of its own.
        if self.pending.is_none() || self.rule.starts_entry(line) {
            if !self.finish_entry() {
                return false;
            }
            self.pending = Some(Pending { offset, lines: 0 });
            self.state.clear();
        }
        if let Some(entry) = &mut self.pending {
            entry.lines = entry.lines.saturating_add(1);
        }
        self.state.feed(&self.filter, line);
        true
    }

    fn reset(&mut self) -> bool {
        self.pending = None;
        self.held_since = None;
        self.batch.clear();
        self.reported = 0;
        self.limit_told = false;
        self.tx.send(ScanMsg::Reset).is_ok()
    }

    fn caught_up(&mut self) -> bool {
        // An entry is complete when the next one starts. At the end of the file we can't know
        // that yet, so the last entry is held back for one short idle round. If no line
        // arrived meanwhile, it is complete (a stack trace is written in one burst).
        if self.pending.is_some() {
            match self.held_since {
                Some(since) if since.elapsed() >= ENTRY_QUIET => {
                    if !self.finish_entry() {
                        return false;
                    }
                    self.held_since = None;
                }
                Some(_) => {} // too soon: the entry may still grow
                None => self.held_since = Some(Instant::now()),
            }
        }
        if !self.flush() {
            return false;
        }
        if self.pending.is_some() {
            return true; // not fully caught up: the last entry might still grow
        }
        // Sent on every idle pass, even when nothing changed. That doubles as a heartbeat:
        // once the tile is closed, this `send` fails and the thread exits.
        self.tx.send(ScanMsg::CaughtUp).is_ok()
    }

    fn wants_quick_recheck(&self) -> bool {
        self.pending.is_some()
    }
}

/// Where a matching entry starts in the file, and which display row it begins at.
struct Entry {
    offset: u64,
    first_row: u64,
}

/// A filtered view of a file: the whole file from the first byte on, then following it.
///
/// It stores 16 bytes per matching entry (where it starts, and which row it begins at) and no
/// text. The text is read back from disk for the few rows on screen. So memory use stays small
/// however large the lines, the entries or the file are.
///
/// Rows are what scrolls: a one-line entry is one row, a stack trace is as many rows as it has
/// lines. `Lines` is expressed in rows, so the viewport needn't know about entries at all.
pub struct FilterView {
    path: String,
    filter: Filter,
    entries: Vec<Entry>,
    total_rows: u64,
    /// The match `n` / `N` stopped at last, as an index into `entries`.
    current: Option<usize>,
    scanning: bool,
    /// More entries matched than `MAX_MATCHES`: only the first ones are here.
    limited: bool,
    rx: Receiver<ScanMsg>,
}

impl FilterView {
    pub fn start(path: &str, filter: Filter, rule: GroupRule) -> Result<Self> {
        Self::start_limited(path, filter, rule, MAX_MATCHES)
    }

    fn start_limited(path: &str, filter: Filter, rule: GroupRule, limit: usize) -> Result<Self> {
        let (tx, rx) = mpsc::sync_channel(CHANNEL_BOUND);
        let sink = FilterSink {
            state: filter.start_entry(),
            filter: filter.clone(),
            rule,
            pending: None,
            held_since: None,
            batch: Vec::new(),
            reported: 0,
            limit,
            limit_told: false,
            tx,
        };
        tail::follow_file(path, sink)?;
        Ok(Self {
            path: path.to_string(),
            filter,
            entries: Vec::new(),
            total_rows: 0,
            current: None,
            scanning: true,
            limited: false,
            rx,
        })
    }

    pub fn filter(&self) -> &Filter {
        &self.filter
    }

    /// Number of matching entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Did more entries match than are kept?
    pub fn is_limited(&self) -> bool {
        self.limited
    }

    /// True until the scanner has read the existing file to its end.
    pub fn is_scanning(&self) -> bool {
        self.scanning
    }

    /// Takes in what the scanner thread has sent. Returns `true` if more may be waiting.
    pub fn pump(&mut self) -> bool {
        let mut taken = 0;
        for _ in 0..MAX_MESSAGES_PER_PUMP {
            match self.rx.try_recv() {
                Ok(ScanMsg::Matches(found)) => {
                    taken += 1;
                    for (offset, lines) in found {
                        self.entries.push(Entry {
                            offset,
                            first_row: self.total_rows,
                        });
                        self.total_rows += u64::from(lines);
                    }
                }
                Ok(ScanMsg::Reset) => {
                    self.entries.clear();
                    self.total_rows = 0;
                    self.current = None;
                    self.scanning = true;
                    self.limited = false;
                }
                Ok(ScanMsg::CaughtUp) => self.scanning = false,
                Ok(ScanMsg::Limit) => self.limited = true,
                Err(_) => return taken > 0, // nothing left (or the thread has ended)
            }
        }
        true
    }

    /// Moves to the next (`forward`) or previous match and returns its index, and whether the
    /// search wrapped around the end. Without a current match it starts from `anchor_row`, the
    /// row at the top of the view: forward finds the first match at or below it, backward the
    /// last one above it.
    pub fn step(&mut self, forward: bool, anchor_row: u64) -> Option<(usize, bool)> {
        let stepped = step_index(
            self.entries.len(),
            self.current,
            forward,
            anchor_row,
            |index| self.entries[index].first_row,
        )?;
        self.current = Some(stepped.0);
        Some(stepped)
    }

    /// The match that contains display row `row`.
    pub fn entry_of_row(&self, row: u64) -> Option<usize> {
        self.entries
            .partition_point(|entry| entry.first_row <= row)
            .checked_sub(1)
    }

    /// Where match `index` starts in the file (a byte offset), and how many lines it has.
    pub fn entry(&self, index: usize) -> (u64, usize) {
        (
            self.entries[index].offset,
            usize::try_from(self.rows_of(index)).unwrap_or(usize::MAX),
        )
    }

    /// The first display row of match `index`.
    pub fn first_row(&self, index: usize) -> u64 {
        self.entries[index].first_row
    }

    /// The match `n` / `N` is at, if any, as its position among all matches.
    pub fn current_index(&self) -> Option<usize> {
        self.current
    }

    /// The display rows of the current match.
    pub fn current_rows(&self) -> Option<std::ops::Range<u64>> {
        let index = self.current?;
        let first = self.entries.get(index)?.first_row;
        Some(first..first + self.rows_of(index))
    }

    /// How many rows entry `index` has: the distance to where the next one begins.
    fn rows_of(&self, index: usize) -> u64 {
        let end = self
            .entries
            .get(index + 1)
            .map_or(self.total_rows, |next| next.first_row);
        end - self.entries[index].first_row
    }
}

/// The next (`forward`) or previous entry among `count` entries, and whether that wrapped around
/// the end. From entry `current`, or without one from the entry nearest `anchor_row` (the row
/// at the top of the view): forward finds the first entry at or below it, backward the last one
/// above it. `first_row` gives the first display row of an entry. Shared by the filters of
/// files and of streams.
pub fn step_index(
    count: usize,
    current: Option<usize>,
    forward: bool,
    anchor_row: u64,
    first_row: impl Fn(usize) -> u64,
) -> Option<(usize, bool)> {
    if count == 0 {
        return None;
    }
    Some(match (current, forward) {
        (Some(i), true) if i + 1 < count => (i + 1, false),
        (Some(_), true) => (0, true),
        (Some(i), false) if i > 0 => (i - 1, false),
        (Some(_), false) => (count - 1, true),
        (None, true) => match (0..count).find(|&i| first_row(i) >= anchor_row) {
            Some(i) => (i, false),
            None => (0, true),
        },
        (None, false) => match (0..count).rev().find(|&i| first_row(i) < anchor_row) {
            Some(i) => (i, false),
            None => (count - 1, true),
        },
    })
}

impl Lines for FilterView {
    fn first_seq(&self) -> u64 {
        0
    }

    fn end_seq(&self) -> u64 {
        self.total_rows
    }

    fn range(&self, from_seq: u64, count: usize) -> Vec<String> {
        if from_seq >= self.total_rows || count == 0 {
            return Vec::new();
        }
        // The entry that contains row `from_seq`: the last one starting at or before it.
        let first_entry = self
            .entries
            .partition_point(|entry| entry.first_row <= from_seq)
            - 1;
        let mut skip = from_seq - self.entries[first_entry].first_row;

        // Opened per call: this only runs for the rows on screen, so it is cheap, and it
        // means the view holds no file handle while idle.
        let Ok(file) = File::open(&self.path) else {
            return vec!["<cannot open file>".to_string(); count.min(1)];
        };
        let mut reader = BufReader::new(file);
        let mut line = CappedLine::new();
        let mut rows = Vec::with_capacity(count);

        // Entries are not next to each other in the file (the ones in between didn't match),
        // so each entry is read from its own offset.
        'entries: for index in first_entry..self.entries.len() {
            if let Err(e) = reader.seek(SeekFrom::Start(self.entries[index].offset)) {
                rows.push(format!("<read error: {e}>"));
                break;
            }
            for row in 0..self.rows_of(index) {
                line.clear();
                match line.read_from(&mut reader) {
                    Ok(0) => break, // the file got shorter underneath us
                    Ok(_) if row < skip => {}
                    Ok(_) => rows.push(line.to_text()),
                    Err(e) => {
                        rows.push(format!("<read error: {e}>"));
                        break 'entries;
                    }
                }
                if rows.len() == count {
                    break 'entries;
                }
            }
            skip = 0; // only the first entry is entered part-way
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Write;
    use std::time::{Duration, Instant};

    /// Pump until `done` holds, or fail after a timeout.
    fn pump_until(view: &mut FilterView, done: impl Fn(&FilterView) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !done(view) {
            assert!(Instant::now() < deadline, "timed out");
            view.pump();
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn temp_log(tag: &str, content: &str) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("tilog-view-{tag}-{}.log", std::process::id()));
        std::fs::write(&path, content).unwrap();
        path
    }

    fn start(path: &std::path::Path, pattern: &str, rule: GroupRule) -> FilterView {
        let filter = Filter::new(None, pattern, false, false).unwrap();
        FilterView::start(path.to_str().unwrap(), filter, rule).unwrap()
    }

    #[test]
    fn scans_from_the_start_then_follows_and_reads_lines_back() {
        let path = temp_log("lines", "INFO a\nSEVERE b\nINFO c\nSEVERE d\n");
        let mut view = start(&path, "SEVERE", GroupRule::Auto);

        // History: matches that were in the file before the view existed.
        pump_until(&mut view, |v| v.len() == 2 && !v.is_scanning());
        assert_eq!(view.range(0, 10), ["SEVERE b", "SEVERE d"]);

        // Follow: a match appended later shows up; a non-match does not.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"INFO e\nSEVERE f\n").unwrap();
        pump_until(&mut view, |v| v.len() == 3);
        assert_eq!(view.range(1, 10), ["SEVERE d", "SEVERE f"]);
        assert_eq!(view.range(99, 10), Vec::<String>::new());

        std::fs::remove_file(&path).unwrap();
    }

    const TRACE_LOG: &str = "INFO a\n\
        SEVERE b\n\tat x.Foo(Foo.java:1)\n\tat y.Bar(Bar.java:2)\n\
        INFO c\n\
        WARN d\n\tat z.Baz(Baz.java:3)\n\
        SEVERE e\nCaused by: boom\n\t... 3 more\n\
        INFO f\n";

    #[test]
    fn a_match_shows_the_whole_multi_line_entry() {
        let path = temp_log("trace", TRACE_LOG);

        // "SEVERE" is on the first line of two entries (3 + 3 rows).
        let mut view = start(&path, "SEVERE", GroupRule::Auto);
        pump_until(&mut view, |v| v.len() == 2 && !v.is_scanning());
        assert_eq!(view.end_seq(), 6);
        assert_eq!(
            view.range(0, 10),
            [
                "SEVERE b",
                "\tat x.Foo(Foo.java:1)",
                "\tat y.Bar(Bar.java:2)",
                "SEVERE e",
                "Caused by: boom",
                "\t... 3 more"
            ]
        );

        // Reading from the middle of an entry, across the gap to the next one.
        assert_eq!(view.range(2, 2), ["\tat y.Bar(Bar.java:2)", "SEVERE e"]);
        assert_eq!(view.range(5, 5), ["\t... 3 more"]);

        // A line deep inside a trace matches its entry, header included.
        let mut view = start(&path, "Bar.java", GroupRule::Auto);
        pump_until(&mut view, |v| v.len() == 1 && !v.is_scanning());
        assert_eq!(view.range(0, 10)[0], "SEVERE b");

        // With grouping off, only the line itself matches.
        let mut view = start(&path, "Bar.java", GroupRule::Off);
        pump_until(&mut view, |v| v.len() == 1 && !v.is_scanning());
        assert_eq!(view.range(0, 10), ["\tat y.Bar(Bar.java:2)"]);

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn steps_through_the_matches_forward_and_backward_with_wrap_around() {
        // Matches at lines 2, 5 and 6 (0-based): "SEVERE b" has a 2-line trace under it.
        let text = "INFO a\nINFO b\nSEVERE c\n\tat x\nINFO d\nSEVERE e\nSEVERE f\n";
        let path = temp_log("step", text);
        let mut view = start(&path, "SEVERE", GroupRule::Auto);
        pump_until(&mut view, |v| v.len() == 3 && !v.is_scanning());

        // From the top, `n` finds the first, then the next ones, then wraps.
        assert_eq!(view.step(true, 0), Some((0, false)));
        assert_eq!(view.step(true, 0), Some((1, false)));
        assert_eq!(view.step(true, 0), Some((2, false)));
        assert_eq!(view.step(true, 0), Some((0, true)), "wrapped around");
        // `N` goes back, and wraps the other way.
        assert_eq!(view.step(false, 0), Some((2, true)));
        assert_eq!(view.step(false, 0), Some((1, false)));

        // Where a match is in the file, and how many lines it has.
        assert_eq!(view.entry(0), (text.find("SEVERE c").unwrap() as u64, 2));
        assert_eq!(view.entry(1), (text.find("SEVERE e").unwrap() as u64, 1));
        assert_eq!(
            view.current_rows(),
            Some(view.first_row(1)..view.first_row(1) + 1)
        );
        assert_eq!(view.current_index(), Some(1));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn without_a_current_match_stepping_starts_from_the_top_of_the_view() {
        let path = temp_log("anchor", "SEVERE a\nINFO\nSEVERE b\nINFO\nSEVERE c\n");
        let mut view = start(&path, "SEVERE", GroupRule::Off);
        pump_until(&mut view, |v| v.len() == 3 && !v.is_scanning());

        // The pane's rows are numbered over the matches only: the three matches are rows 0, 1
        // and 2. With the view starting at row 2, `n` finds the third match...
        assert_eq!(view.step(true, 2), Some((2, false)));
        // ...and `N`, from the same place without a current match, finds the one above.
        view.current = None;
        assert_eq!(view.step(false, 2), Some((1, false)));
        // Below every match, forward wraps to the first; above every match, backward to the last.
        view.current = None;
        assert_eq!(view.step(true, 99), Some((0, true)));
        view.current = None;
        assert_eq!(view.step(false, 0), Some((2, true)));

        // No matches at all.
        let mut none = start(&path, "ABSENT", GroupRule::Off);
        pump_until(&mut none, |v| !v.is_scanning());
        assert_eq!(none.step(true, 0), None);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn the_last_entry_is_released_after_a_short_idle_and_a_growing_entry_stays_whole() {
        let path = temp_log("live", "SEVERE a\n\tat one\n");
        let mut view = start(&path, "SEVERE", GroupRule::Auto);

        // Nothing follows the entry, so it is delivered once the file has been idle a moment.
        pump_until(&mut view, |v| v.len() == 1 && !v.is_scanning());
        assert_eq!(view.range(0, 10), ["SEVERE a", "\tat one"]);

        // A new entry arrives in two writes close together: it must not be cut in two.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"SEVERE b\n").unwrap();
        std::thread::sleep(Duration::from_millis(30));
        f.write_all(b"\tat two\n").unwrap();
        pump_until(&mut view, |v| v.len() == 2 && !v.is_scanning());
        assert_eq!(view.range(2, 10), ["SEVERE b", "\tat two"]);

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_filter_keeps_at_most_its_limit_of_matches_and_says_so() {
        let content: String = (0..50).map(|i| format!("hit {i}\nother {i}\n")).collect();
        let path = temp_log("limit", &content);
        let filter = Filter::new(None, "hit", false, false).unwrap();
        let mut view =
            FilterView::start_limited(path.to_str().unwrap(), filter, GroupRule::Off, 10).unwrap();

        pump_until(&mut view, |v| v.is_limited() && !v.is_scanning());
        assert_eq!(
            view.len(),
            10,
            "the first ten are kept, the other forty are not"
        );
        assert_eq!(view.range(0, 1), ["hit 0"]);
        assert_eq!(view.range(9, 1), ["hit 9"]);
        std::fs::remove_file(&path).unwrap();
    }
}
