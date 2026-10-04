//! Reading one line at a time from a log, with a limit on how much of a line is kept.
//!
//! A log can contain a line of many megabytes (a minified JSON dump, a binary blob). Held in
//! memory, and then drawn, such a line would cost far more than everything else together. So
//! only the first `MAX_LINE_BYTES` bytes of a line are kept; the rest is read and thrown away,
//! and the line ends with a note saying how much was cut. Byte offsets stay exact: they count
//! what is in the file, not what was kept.

use std::borrow::Cow;
use std::io::{self, BufRead};

use crate::tail::trim_eol;

/// The most bytes of one line that are kept.
pub const MAX_LINE_BYTES: usize = 64 * 1024;

/// A line being assembled. A writer may be in the middle of a line, so `read_from` can be called
/// again to add what has been written since, until `is_complete`.
#[derive(Default)]
pub struct CappedLine {
    bytes: Vec<u8>,
    /// Bytes of the line that did not fit and were dropped.
    cut: u64,
    complete: bool,
}

impl CappedLine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets the line, to start the next one.
    pub fn clear(&mut self) {
        self.bytes.clear();
        self.cut = 0;
        self.complete = false;
    }

    /// Reads up to the next `\n` (or the end of what is available) and adds it to the line.
    /// Returns how many bytes of the input were used: 0 at the end of the input.
    pub fn read_from(&mut self, reader: &mut impl BufRead) -> io::Result<usize> {
        self.read_capped(reader, MAX_LINE_BYTES)
    }

    fn read_capped(&mut self, reader: &mut impl BufRead, cap: usize) -> io::Result<usize> {
        let mut used_total = 0;
        loop {
            let chunk = reader.fill_buf()?;
            if chunk.is_empty() {
                return Ok(used_total);
            }
            let newline = chunk.iter().position(|&byte| byte == b'\n');
            let used = newline.map_or(chunk.len(), |i| i + 1);
            // Keep what fits; the line ending is not part of the text.
            let text_len = newline.unwrap_or(used);
            let keep = text_len.min(cap.saturating_sub(self.bytes.len()));
            self.bytes.extend_from_slice(&chunk[..keep]);
            self.cut += (text_len - keep) as u64;
            reader.consume(used);
            used_total += used;
            if newline.is_some() {
                self.complete = true;
                return Ok(used_total);
            }
        }
    }

    /// Does the line end with its `\n`? (Otherwise the writer is still in the middle of it.)
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// The text of the line: without its line ending, and with a note at the end if it was cut.
    pub fn text(&self) -> Cow<'_, [u8]> {
        let text = trim_eol(&self.bytes);
        if self.cut == 0 {
            return Cow::Borrowed(text);
        }
        let mut owned = text.to_vec();
        owned.extend_from_slice(format!(" … [{} bytes cut]", self.cut).as_bytes());
        Cow::Owned(owned)
    }

    /// The text of the line as a `String` (invalid UTF-8 becomes `�`).
    pub fn to_text(&self) -> String {
        String::from_utf8_lossy(&self.text()).into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn read_all(input: &[u8], cap: usize) -> Vec<(usize, String)> {
        let mut reader = Cursor::new(input.to_vec());
        let mut line = CappedLine::new();
        let mut out = Vec::new();
        loop {
            line.clear();
            let used = line.read_capped(&mut reader, cap).unwrap();
            if used == 0 {
                return out;
            }
            out.push((used, line.to_text()));
        }
    }

    #[test]
    fn short_lines_are_read_whole_and_offsets_count_the_line_endings() {
        let lines = read_all(b"one\r\ntwo\nlast", 100);
        assert_eq!(
            lines,
            [
                (5, "one".to_string()),
                (4, "two".to_string()),
                (4, "last".to_string())
            ]
        );
    }

    #[test]
    fn a_long_line_is_cut_but_its_whole_length_is_consumed() {
        let mut input = b"head ".to_vec();
        input.extend(std::iter::repeat_n(b'x', 1000));
        input.extend_from_slice(b"\nnext\n");
        let lines = read_all(&input, 20);
        assert_eq!(
            lines[0].0, 1006,
            "the whole line was used up, so the next one starts right"
        );
        assert_eq!(
            lines[0].1,
            format!("head {} … [{} bytes cut]", "x".repeat(15), 985)
        );
        assert_eq!(lines[1], (5, "next".to_string()));
    }

    #[test]
    fn a_line_written_in_pieces_is_put_together_and_cut_once() {
        let mut line = CappedLine::new();
        line.read_capped(&mut Cursor::new(b"abcdefgh".to_vec()), 5)
            .unwrap();
        assert!(!line.is_complete());
        line.read_capped(&mut Cursor::new(b"ijkl\n".to_vec()), 5)
            .unwrap();
        assert!(line.is_complete());
        assert_eq!(line.to_text(), "abcde … [7 bytes cut]");
    }

    #[test]
    fn a_50_megabyte_line_costs_only_the_cap() {
        let input = vec![b'y'; 50_000_000];
        let mut line = CappedLine::new();
        let used = line
            .read_from(&mut std::io::BufReader::new(Cursor::new(input)))
            .unwrap();
        assert_eq!(used, 50_000_000);
        assert!(line.to_text().len() < MAX_LINE_BYTES + 64);
    }
}
