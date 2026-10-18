//! Reading one line at a time from a log, with a limit on how much of a line is kept.
//!
//! A log can contain a line of many megabytes (a minified JSON dump, a binary blob). Held in
//! memory, and then drawn, such a line would cost far more than everything else together. So
//! only the first `MAX_LINE_BYTES` bytes of a line are kept; the rest is read and thrown away,
//! and the line ends with a note saying how much was cut. Byte offsets stay exact: they count
//! what is in the file, not what was kept.
//!
//! Terminal escape sequences (the colors of `docker logs`, `kubectl logs` or a Spring app) are
//! taken out here too, so that searching, filtering, reading timestamps and tilog's own coloring
//! all see the text as a person reads it, not `\x1b[32mINFO\x1b[0m`.

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

    /// The text of the line: without its line ending and its escape sequences, and with a note at
    /// the end if it was cut.
    pub fn text(&self) -> Cow<'_, [u8]> {
        let text = strip_escapes(trim_eol(&self.bytes));
        if self.cut == 0 {
            return text;
        }
        let mut owned = text.into_owned();
        owned.extend_from_slice(format!(" … [{} bytes cut]", self.cut).as_bytes());
        Cow::Owned(owned)
    }

    /// The text of the line as a `String` (invalid UTF-8 becomes `�`).
    pub fn to_text(&self) -> String {
        String::from_utf8_lossy(&self.text()).into_owned()
    }
}

/// The escape character that starts a terminal escape sequence.
const ESC: u8 = 0x1b;

/// `text` without its terminal escape sequences: colors and cursor movements (`ESC [ ... m`),
/// titles and links (`ESC ] ... BEL` or `ESC ] ... ESC \\`), and the short two-byte ones. Text
/// without an escape character is returned as it is, without a copy.
pub fn strip_escapes(text: &[u8]) -> Cow<'_, [u8]> {
    if !text.contains(&ESC) {
        return Cow::Borrowed(text);
    }
    let mut out = Vec::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text[i] != ESC {
            out.push(text[i]);
            i += 1;
            continue;
        }
        i += 1; // the ESC itself
        match text.get(i) {
            // CSI: parameters and intermediates, up to a final byte in 0x40..=0x7e.
            Some(b'[') => {
                i += 1;
                while i < text.len() && !(0x40..=0x7e).contains(&text[i]) {
                    i += 1;
                }
                i += 1;
            }
            // OSC: up to BEL, or up to the string terminator ESC \.
            Some(b']') => {
                i += 1;
                while i < text.len() {
                    if text[i] == 0x07 {
                        i += 1;
                        break;
                    }
                    if text[i] == ESC && text.get(i + 1) == Some(&b'\\') {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            }
            // Any other escape is two bytes long (`ESC c`, `ESC 7`, ...).
            Some(_) => i += 1,
            None => {}
        }
    }
    Cow::Owned(out)
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
    fn colors_and_other_escape_sequences_are_taken_out() {
        let strip = |text: &[u8]| String::from_utf8(strip_escapes(text).into_owned()).unwrap();
        // Colors, as docker, kubectl and Spring Boot write them.
        assert_eq!(
            strip(b"\x1b[32mINFO\x1b[0m started \x1b[1;31mERROR\x1b[m"),
            "INFO started ERROR"
        );
        assert_eq!(
            strip(b"\x1b[38;5;208morange\x1b[39m \x1b[48;2;1;2;3mrgb\x1b[0m"),
            "orange rgb"
        );
        // A link and a window title (OSC), ended by ESC \\ and by BEL.
        assert_eq!(
            strip(b"see \x1b]8;;https://x.org\x1b\\here\x1b]8;;\x1b\\ now"),
            "see here now"
        );
        assert_eq!(strip(b"\x1b]0;title\x07text"), "text");
        // Two-byte escapes, and an escape cut off at the end of the line.
        assert_eq!(strip(b"\x1bcreset\x1b7saved"), "resetsaved");
        assert_eq!(strip(b"cut \x1b[3"), "cut ");
        assert_eq!(strip(b"lone \x1b"), "lone ");
        // Text without escapes is not copied.
        assert!(matches!(strip_escapes(b"plain text"), Cow::Borrowed(_)));
    }

    #[test]
    fn a_line_read_from_a_log_comes_out_without_its_colors() {
        let lines = read_all(b"\x1b[33mWARN\x1b[0m slow\n", 100);
        assert_eq!(
            lines,
            [(19, "WARN slow".to_string())],
            "the offset still counts every byte"
        );
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
