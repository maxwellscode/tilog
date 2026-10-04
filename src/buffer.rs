use std::collections::VecDeque;

use crate::lines::Lines;

/// Where sequence numbers start. Far above zero on purpose: lines loaded from *before* the
/// first one get smaller numbers, and a `u64` can't go below zero.
const ORIGIN: u64 = 1 << 40;

/// The most text one buffer holds, in bytes, however many lines that is. Lines are cut at
/// `line::MAX_LINE_BYTES`, but thousands of long ones would still add up. The oldest lines go
/// first, as when the line limit is reached.
const BYTE_BUDGET: usize = 32 * 1024 * 1024;

/// A line and where it starts in the file, so older lines can be located on disk.
struct Line {
    offset: u64,
    text: String,
}

/// A bounded buffer of lines: when full, pushing a new line evicts the oldest one.
///
/// Every line gets a permanent *sequence number*. Positions in the buffer are expressed in
/// sequence numbers rather than indices, because indices shift every time an old line is
/// evicted (or older ones are loaded in front), while a sequence number always refers to the
/// same line (until that line is evicted).
///
/// The buffer usually holds the *end* of a file: tilog opens big files at their tail. Older
/// lines can be loaded in front on demand (`prepend`), and the capacity can change at runtime.
pub struct RingBuffer {
    lines: VecDeque<Line>,
    capacity: usize,
    /// Sequence number of `lines[0]`.
    first_seq: u64,
    /// Lines received from the file since the buffer was created.
    received: u64,
    /// No line starting before this byte offset may be loaded (set by `clear`).
    floor: u64,
    /// After `clear`: the next line to arrive becomes the floor.
    floor_pending: bool,
    /// When the file was replaced (log rotation): the sequence number the first line of the new
    /// file got. Lines before it come from a file that is no longer at the path.
    rotated_at: Option<u64>,
    /// Bytes of text held (see `BYTE_BUDGET`).
    bytes: usize,
}

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self::with_origin(capacity, ORIGIN)
    }

    /// Like `new`, with an explicit first sequence number (tests use 0 for readable numbers).
    pub fn with_origin(capacity: usize, origin: u64) -> Self {
        assert!(capacity > 0, "capacity must be at least 1");
        Self {
            lines: VecDeque::new(),
            capacity,
            first_seq: origin,
            received: 0,
            floor: 0,
            floor_pending: false,
            rotated_at: None,
            bytes: 0,
        }
    }

    /// Adds the newest line, evicting the oldest ones if the buffer is full.
    pub fn push(&mut self, offset: u64, text: String) {
        if self.floor_pending {
            self.floor = offset;
            self.floor_pending = false;
        }
        while self.lines.len() >= self.capacity {
            self.evict_front();
        }
        self.bytes += text.len();
        self.lines.push_back(Line { offset, text });
        self.received += 1;
        // Keep at least the newest line, however long.
        while self.bytes > BYTE_BUDGET && self.lines.len() > 1 {
            self.evict_front();
        }
    }

    /// Changes how many lines fit. Shrinking evicts the oldest lines at once.
    pub fn set_capacity(&mut self, capacity: usize) {
        self.capacity = capacity.max(1);
        while self.lines.len() > self.capacity {
            self.evict_front();
        }
    }

    fn evict_front(&mut self) {
        if let Some(line) = self.lines.pop_front() {
            self.bytes -= line.text.len(); // and the String is dropped (freed) right here
        }
        self.first_seq += 1;
    }

    /// Puts older lines (in file order, oldest first) in front of the current ones.
    /// Sequence numbers of the existing lines don't change.
    pub fn prepend(&mut self, older: Vec<(u64, String)>) {
        for (offset, text) in older.into_iter().rev() {
            // History is not allowed to push the buffer over its budget.
            if self.bytes + text.len() > BYTE_BUDGET {
                break;
            }
            self.bytes += text.len();
            self.lines.push_front(Line { offset, text });
            self.first_seq -= 1;
        }
    }

    /// Drops all lines but keeps counting: the next line continues the sequence numbers.
    /// What came before can't be loaded back.
    pub fn clear(&mut self) {
        self.first_seq = self.end_seq();
        self.lines.clear();
        self.bytes = 0;
        self.floor_pending = true;
    }

    /// The file was truncated or replaced by a new one (log rotation). The lines in the buffer
    /// stay, so scrolling back still shows them, but older lines can't be read from disk any
    /// more: what is at the path now is a different file. The next lines start it over.
    pub fn rotated(&mut self) {
        self.rotated_at = Some(self.end_seq());
        self.floor = 0;
        self.floor_pending = false;
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// How many lines have been received since the buffer was created.
    pub fn received(&self) -> u64 {
        self.received
    }

    /// Byte offset of the oldest line held.
    pub fn front_offset(&self) -> Option<u64> {
        self.lines.front().map(|line| line.offset)
    }

    /// The lowest byte offset that older lines may still be loaded from.
    pub fn floor(&self) -> u64 {
        self.floor
    }

    /// Are there older lines on disk that are not in the buffer?
    pub fn has_older(&self) -> bool {
        // Lines from before a rotation were in another file: their offsets mean nothing now.
        if self.rotated_at.is_some_and(|at| self.first_seq < at) {
            return false;
        }
        self.front_offset()
            .is_some_and(|offset| offset > self.floor)
    }

    /// The sequence number of the line that starts at byte `offset` of the file, if it is in the
    /// buffer. After a rotation only the lines of the current file are looked at: the offsets of
    /// the ones before it belong to a file that is gone.
    pub fn seq_of_offset(&self, offset: u64) -> Option<u64> {
        let skip = self.rotated_at.map_or(0, |at| {
            usize::try_from(at.saturating_sub(self.first_seq)).unwrap_or(usize::MAX)
        });
        self.lines
            .iter()
            .enumerate()
            .skip(skip)
            .find(|(_, line)| line.offset == offset)
            .map(|(index, _)| self.first_seq + index as u64)
    }

    /// The byte offset stored with the line that has sequence number `seq`. (A stream filter
    /// stores there the sequence number its line has in the source.)
    pub fn offset_at(&self, seq: u64) -> Option<u64> {
        let index = usize::try_from(seq.checked_sub(self.first_seq)?).ok()?;
        self.lines.get(index).map(|line| line.offset)
    }

    /// Sequence number of line `n` (counting from 1) of the part of the file first loaded.
    pub fn seq_of_line(&self, n: u64) -> u64 {
        ORIGIN + n.saturating_sub(1)
    }
}

// The ring buffer is one way to provide lines to a tile.
impl Lines for RingBuffer {
    /// Sequence number of the oldest line still in the buffer.
    fn first_seq(&self) -> u64 {
        self.first_seq
    }

    /// Sequence number one past the newest line (the number the next push will get).
    fn end_seq(&self) -> u64 {
        self.first_seq + self.lines.len() as u64
    }

    fn range(&self, from_seq: u64, count: usize) -> Vec<String> {
        let skip = usize::try_from(from_seq.saturating_sub(self.first_seq)).unwrap_or(usize::MAX);
        self.lines
            .iter()
            .skip(skip)
            .take(count)
            .map(|line| line.text.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(capacity: usize) -> RingBuffer {
        RingBuffer::with_origin(capacity, 0)
    }

    #[test]
    fn evicts_oldest_and_keeps_sequence_numbers() {
        let mut buf = buffer(3);
        for i in 0..5 {
            buf.push(i * 10, format!("line {i}"));
        }
        assert_eq!(buf.first_seq(), 2);
        assert_eq!(buf.end_seq(), 5);
        assert_eq!(buf.received(), 5);
        assert_eq!(buf.front_offset(), Some(20));
        assert_eq!(buf.range(3, 10), ["line 3", "line 4"]);
    }

    #[test]
    fn clear_keeps_counting_and_forbids_loading_what_came_before() {
        let mut buf = buffer(3);
        buf.push(0, "a".into());
        buf.push(10, "b".into());
        buf.clear();
        assert_eq!((buf.first_seq(), buf.end_seq()), (2, 2));
        assert!(!buf.has_older());

        buf.push(50, "c".into());
        assert_eq!(buf.range(0, 10), ["c"]);
        assert_eq!(buf.floor(), 50);
        assert!(!buf.has_older(), "the cleared lines must stay gone");
    }

    #[test]
    fn prepended_history_keeps_existing_sequence_numbers() {
        let mut buf = RingBuffer::with_origin(10, 100);
        buf.push(40, "d".into());
        buf.push(50, "e".into());
        assert!(buf.has_older());

        buf.prepend(vec![(20, "b".into()), (30, "c".into())]);
        assert_eq!((buf.first_seq(), buf.end_seq()), (98, 102));
        assert_eq!(buf.range(98, 10), ["b", "c", "d", "e"]);
        assert_eq!(buf.range(100, 1), ["d"], "\"d\" is still number 100");
        assert_eq!(buf.front_offset(), Some(20));
        assert_eq!(buf.received(), 2, "history is not a newly received line");
    }

    #[test]
    fn shrinking_the_capacity_evicts_the_oldest() {
        let mut buf = buffer(5);
        for i in 0..5 {
            buf.push(i, i.to_string());
        }
        buf.set_capacity(2);
        assert_eq!(buf.range(0, 10), ["3", "4"]);
        assert_eq!(buf.first_seq(), 3);
        buf.set_capacity(5); // growing never brings lines back by itself
        assert_eq!(buf.len(), 2);
    }

    #[test]
    fn rotation_keeps_the_lines_but_not_the_history_before_it() {
        let mut buf = buffer(4);
        buf.push(5000, "old 1".into());
        buf.push(6000, "old 2".into());
        assert!(
            buf.has_older(),
            "the first lines of the file are not loaded"
        );

        buf.rotated();
        assert_eq!(buf.range(0, 10), ["old 1", "old 2"], "scrollback stays");
        assert!(
            !buf.has_older(),
            "older lines were in the file that was rotated away"
        );

        // The new file starts at offset 0 again.
        buf.push(0, "new 1".into());
        buf.push(40, "new 2".into());
        assert!(!buf.has_older());

        // Once the lines of the old file are gone (capacity 4: "new 3" and "new 4" push out
        // "old 1" and "old 2"), the buffer starts at the beginning of the new file.
        buf.push(80, "new 3".into());
        buf.push(120, "new 4".into());
        assert_eq!(buf.range(0, 10), ["new 1", "new 2", "new 3", "new 4"]);
        assert_eq!(buf.front_offset(), Some(0));
        assert!(
            !buf.has_older(),
            "nothing of the new file comes before its first line"
        );

        // One more pushes out "new 1": now there are older lines of the new file on disk.
        buf.push(160, "new 5".into());
        assert_eq!(buf.front_offset(), Some(40));
        assert!(buf.has_older());
    }

    #[test]
    fn a_line_is_found_by_its_offset() {
        let mut buf = RingBuffer::with_origin(10, 100);
        for (offset, text) in [(0, "a"), (10, "b"), (25, "c"), (40, "d")] {
            buf.push(offset, text.into());
        }
        assert_eq!(buf.seq_of_offset(25), Some(102));
        assert_eq!(buf.seq_of_offset(0), Some(100));
        assert_eq!(
            buf.seq_of_offset(26),
            None,
            "an offset in the middle of a line is no line"
        );
        assert_eq!(buf.seq_of_offset(999), None);

        // After a rotation, the offsets of the old file mean nothing: only the new file counts.
        buf.rotated();
        buf.push(0, "new a".into());
        buf.push(25, "new b".into());
        assert_eq!(
            buf.seq_of_offset(25),
            Some(105),
            "the new file's line, not the old file's"
        );
        assert_eq!(
            buf.seq_of_offset(40),
            None,
            "the old file's last line is not a candidate"
        );
    }

    #[test]
    fn nothing_older_when_the_file_start_is_loaded() {
        let mut buf = buffer(5);
        buf.push(0, "first".into());
        assert!(!buf.has_older());
    }

    #[test]
    fn line_numbers_count_from_one() {
        let buf = RingBuffer::new(5);
        assert_eq!(buf.seq_of_line(1), buf.first_seq());
        assert_eq!(buf.seq_of_line(0), buf.first_seq());
        assert_eq!(buf.seq_of_line(11), buf.first_seq() + 10);
    }

    #[test]
    fn the_buffer_holds_at_most_its_byte_budget_of_text() {
        let mut buf = buffer(1_000_000);
        let line = "z".repeat(1024 * 1024);
        for i in 0..100 {
            buf.push(i, line.clone());
        }
        // 100 lines of 1 MiB would be 100 MiB; the budget keeps only the newest ones.
        assert!(buf.len() <= 32, "{} lines held", buf.len());
        assert!(buf.len() >= 30);
        assert_eq!(buf.end_seq(), 100, "sequence numbers go on counting");

        // One line over the budget is still kept: the newest line is never dropped.
        let mut big = buffer(10);
        big.push(0, "q".repeat(40 * 1024 * 1024));
        assert_eq!(big.len(), 1);
    }
}
