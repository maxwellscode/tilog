//! Reading a file *backwards*, so a huge file can be opened at its end without reading the
//! rest, and older lines can be fetched on demand when the user scrolls up.
//!
//! All offsets are byte offsets. A "line" is whatever ends in `\n`.

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};

use crate::line::CappedLine;

/// Bytes read per step while walking backwards.
const CHUNK: u64 = 64 * 1024;

/// The byte offset where the last `n` complete lines before `end` begin, or 0 if there are
/// fewer than `n`. `end` is the end of the file, or the start of a line.
///
/// A trailing partial line (no `\n` yet) is not counted: it isn't a line until it is finished.
pub fn start_of_last_lines(file: &mut File, end: u64, n: usize) -> io::Result<u64> {
    if n == 0 {
        return Ok(end);
    }
    let mut buf = vec![0u8; CHUNK as usize];
    let mut remaining = end;
    let mut newlines = 0;

    while remaining > 0 {
        let len = remaining.min(CHUNK);
        let chunk_start = remaining - len;
        file.seek(SeekFrom::Start(chunk_start))?;
        file.read_exact(&mut buf[..len as usize])?;

        // The n lines are ended by the 1st..n-th newline counted from the back; the line before
        // them is ended by the (n+1)-th, and our start is the byte right after that one.
        for (i, byte) in buf[..len as usize].iter().enumerate().rev() {
            if *byte == b'\n' {
                newlines += 1;
                if newlines == n + 1 {
                    return Ok(chunk_start + i as u64 + 1);
                }
            }
        }
        remaining = chunk_start;
    }
    Ok(0)
}

/// The complete lines in `start..end` (both at line boundaries), each with its byte offset.
pub fn read_lines(file: &mut File, start: u64, end: u64) -> io::Result<Vec<(u64, String)>> {
    file.seek(SeekFrom::Start(start))?;
    // Read through a limit, so the range is never held in memory as a whole: a line of many
    // megabytes is cut as it is read (see `CappedLine`).
    let mut reader = BufReader::new(file.take(end.saturating_sub(start)));
    let (mut lines, mut offset) = (Vec::new(), start);
    let mut line = CappedLine::new();
    loop {
        line.clear();
        let read = line.read_from(&mut reader)?;
        if read == 0 || !line.is_complete() {
            return Ok(lines); // a last line without its `\n` is not a line yet
        }
        lines.push((offset, line.to_text()));
        offset += read as u64;
    }
}

/// Up to `n` complete lines starting at byte offset `start` (the start of a line), each with its
/// offset, and the offset right after the last one (where the next read continues).
///
/// A last line that doesn't end in `\n` yet is left out: it is still being written.
pub fn read_from(path: &str, start: u64, n: usize) -> io::Result<(Vec<(u64, String)>, u64)> {
    let mut reader = BufReader::new(File::open(path)?);
    reader.seek(SeekFrom::Start(start))?;

    let mut lines = Vec::new();
    let mut offset = start;
    let mut line = CappedLine::new();
    while lines.len() < n {
        line.clear();
        let read = line.read_from(&mut reader)?;
        if read == 0 || !line.is_complete() {
            break;
        }
        lines.push((offset, line.to_text()));
        offset += read as u64;
    }
    Ok((lines, offset))
}

/// How far apart `find_time` lets the search bounds get before it reads on line by line.
const TIME_SPAN: u64 = 256 * 1024;

/// Lines looked at after a probe point to find one that has a timestamp.
const PROBE_LINES: usize = 200;

/// Where a time lies in a file.
#[derive(Debug, PartialEq)]
pub enum TimeFound {
    /// The first line at or after the time: its offset and its own time.
    At { offset: u64, time: i64 },
    /// Every line is before the time; this is the last one that has a timestamp.
    AfterEnd { offset: u64, time: i64 },
    /// No line has a timestamp.
    None,
}

/// Finds the first line of `path` (not before byte `floor`) whose time, as read by `time_of`, is
/// at or after `target`. The log is taken as sorted by time, as logs are: it is searched by
/// bisecting the file, so a multi-gigabyte log answers in a few reads. Lines without a timestamp
/// (the rest of a stack trace) are skipped over.
pub fn find_time(
    path: &str,
    floor: u64,
    target: i64,
    time_of: impl Fn(&str) -> Option<i64>,
) -> io::Result<TimeFound> {
    let mut file = File::open(path)?;
    let (mut low, mut high) = (floor, file.metadata()?.len());

    // Invariant: `low` is the start of a line whose time is before `target` (or the floor), and
    // the answer is not after `high`.
    while high - low > TIME_SPAN {
        let mid = low + (high - low) / 2;
        match probe(&mut file, mid, high, &time_of)? {
            Some((offset, time)) if time < target => low = offset,
            Some((offset, _)) => high = offset,
            None => high = mid,
        }
    }

    let (mut offset, mut last) = (low, None);
    loop {
        let (lines, next) = read_from(path, offset, 2000)?;
        if lines.is_empty() {
            break;
        }
        for (line_offset, text) in lines {
            if let Some(time) = time_of(&text) {
                if time >= target {
                    return Ok(TimeFound::At {
                        offset: line_offset,
                        time,
                    });
                }
                last = Some((line_offset, time));
            }
        }
        offset = next;
    }
    Ok(match last {
        Some((offset, time)) => TimeFound::AfterEnd { offset, time },
        None => TimeFound::None,
    })
}

/// The first line that starts at or after byte `from` (and before `limit`) and has a time:
/// its offset and its time. `from` may be anywhere, also in the middle of a line.
fn probe(
    file: &mut File,
    from: u64,
    limit: u64,
    time_of: &impl Fn(&str) -> Option<i64>,
) -> io::Result<Option<(u64, i64)>> {
    file.seek(SeekFrom::Start(from.saturating_sub(1)))?;
    let mut reader = BufReader::new(&mut *file);
    // The byte before `from` tells whether `from` is the start of a line; if not, skip the rest
    // of this one.
    let mut offset = from.saturating_sub(1);
    let mut line = CappedLine::new();
    let read = line.read_from(&mut reader)?;
    offset += read as u64;
    if from == 0 {
        offset = 0;
        reader.seek(SeekFrom::Start(0))?;
    }
    for _ in 0..PROBE_LINES {
        if offset >= limit {
            break;
        }
        line.clear();
        let read = line.read_from(&mut reader)?;
        if read == 0 || !line.is_complete() {
            break;
        }
        if let Some(time) = time_of(&line.to_text()) {
            return Ok(Some((offset, time)));
        }
        offset += read as u64;
    }
    Ok(None)
}

/// Where to start reading `path` so that about the last `lines` lines are included.
pub fn tail_start(path: &str, lines: usize) -> io::Result<u64> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    start_of_last_lines(&mut file, len, lines)
}

/// Up to `n` lines that come right before byte offset `end`, but none starting before `floor`.
pub fn read_before(path: &str, end: u64, floor: u64, n: usize) -> io::Result<Vec<(u64, String)>> {
    if end <= floor || n == 0 {
        return Ok(Vec::new());
    }
    let mut file = File::open(path)?;
    let start = start_of_last_lines(&mut file, end, n)?.max(floor);
    read_lines(&mut file, start, end)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_log(tag: &str, content: &[u8]) -> std::path::PathBuf {
        let path =
            std::env::temp_dir().join(format!("tilog-history-{tag}-{}.log", std::process::id()));
        fs::write(&path, content).unwrap();
        path
    }

    fn texts(lines: Vec<(u64, String)>) -> Vec<String> {
        lines.into_iter().map(|(_, text)| text).collect()
    }

    #[test]
    fn finds_the_start_of_the_last_lines() {
        let path = temp_log("start", b"a\nb\nc\n");
        let mut file = File::open(&path).unwrap();
        assert_eq!(start_of_last_lines(&mut file, 6, 1).unwrap(), 4); // "c\n"
        assert_eq!(start_of_last_lines(&mut file, 6, 2).unwrap(), 2); // "b\nc\n"
        assert_eq!(start_of_last_lines(&mut file, 6, 3).unwrap(), 0);
        assert_eq!(start_of_last_lines(&mut file, 6, 99).unwrap(), 0);
        assert_eq!(start_of_last_lines(&mut file, 6, 0).unwrap(), 6);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_partial_last_line_is_not_counted() {
        let path = temp_log("partial", b"a\nb\nc");
        // "c" is unfinished: the last complete line is "b", so one line back starts at "b".
        assert_eq!(tail_start(path.to_str().unwrap(), 1).unwrap(), 2);
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn works_across_chunk_boundaries() {
        // Far more than one 64 KiB chunk: 20,000 lines of about 12 bytes.
        let content: String = (0..20_000).map(|i| format!("line {i:06}\n")).collect();
        let path = temp_log("big", content.as_bytes());
        let path_str = path.to_str().unwrap();

        let start = tail_start(path_str, 5000).unwrap();
        let mut file = File::open(&path).unwrap();
        let lines = read_lines(&mut file, start, content.len() as u64).unwrap();
        assert_eq!(lines.len(), 5000);
        assert_eq!(lines[0].1, "line 015000");
        assert_eq!(lines[4999].1, "line 019999");
        // The offsets point at the real start of each line.
        assert_eq!(&content[lines[1].0 as usize..][..11], "line 015001");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn reads_forward_from_an_offset_and_says_where_to_continue() {
        let path = temp_log("from", b"one\ntwo\r\nthree\nfour\npartial");
        let path_str = path.to_str().unwrap();

        // From "two" (offset 4): two lines, then the offset of "four".
        let (lines, next) = read_from(path_str, 4, 2).unwrap();
        assert_eq!(lines, [(4, "two".to_string()), (9, "three".to_string())]);
        assert_eq!(next, 15);

        // Continuing from there: "four", and the unfinished last line is not a line yet.
        let (lines, next) = read_from(path_str, next, 10).unwrap();
        assert_eq!(lines, [(15, "four".to_string())]);
        assert_eq!(next, 20);
        assert!(read_from(path_str, next, 10).unwrap().0.is_empty());

        // Past the end, and a missing file.
        assert!(read_from(path_str, 999, 5).unwrap().0.is_empty());
        assert!(read_from("/definitely/not/here.log", 0, 5).is_err());
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn reads_older_lines_down_to_a_floor() {
        let path = temp_log("before", b"one\ntwo\nthree\nfour\nfive\n");
        let path_str = path.to_str().unwrap();
        // "four" starts at byte 14: the two lines before it.
        let older = read_before(path_str, 14, 0, 2).unwrap();
        assert_eq!(older, [(4, "two".to_string()), (8, "three".to_string())]);
        // The floor cuts it short; at or below the floor there is nothing.
        assert_eq!(texts(read_before(path_str, 14, 8, 10).unwrap()), ["three"]);
        assert!(read_before(path_str, 8, 8, 10).unwrap().is_empty());
        // Invalid UTF-8 and CRLF are handled.
        fs::write(&path, b"ok\r\n\xff\xfe bad\nlast\n").unwrap();
        assert_eq!(
            texts(read_before(path_str, 14, 0, 10).unwrap()),
            ["ok", "\u{fffd}\u{fffd} bad"]
        );
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn finds_a_time_by_bisecting_a_big_file() {
        // 40,000 lines, one second apart: far more than one TIME_SPAN (256 KiB) of text,
        // with a stack trace line in between that has no time of its own.
        let content: String = (0..40_000)
            .map(|i| {
                format!(
                    "{i:05} entry\n{}",
                    if i % 500 == 0 { "    at trace\n" } else { "" }
                )
            })
            .collect();
        let path = temp_log("time", content.as_bytes());
        let path_str = path.to_str().unwrap();
        let time_of = |line: &str| line.split_whitespace().next()?.parse::<i64>().ok();

        let found = find_time(path_str, 0, 31_337, time_of).unwrap();
        let TimeFound::At { offset, time } = found else {
            panic!("{found:?}")
        };
        assert_eq!(time, 31_337);
        assert_eq!(&content[offset as usize..][..11], "31337 entry");

        // Before the first line, and between two lines: the first line at or after.
        assert!(matches!(
            find_time(path_str, 0, -5, time_of).unwrap(),
            TimeFound::At { offset: 0, time: 0 }
        ));
        let found = find_time(path_str, 0, 499, time_of).unwrap();
        assert!(matches!(found, TimeFound::At { time: 499, .. }));
        // After the last line.
        let found = find_time(path_str, 0, 99_999, time_of).unwrap();
        assert!(
            matches!(found, TimeFound::AfterEnd { time: 39_999, .. }),
            "{found:?}"
        );
        // The floor keeps the search away from what was cleared.
        let floor = content.find("20000 entry").unwrap() as u64;
        let found = find_time(path_str, floor, 100, time_of).unwrap();
        assert!(
            matches!(found, TimeFound::At { time: 20_000, .. }),
            "{found:?}"
        );
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_file_without_times_has_nothing_to_find() {
        let path = temp_log("notime", b"a\nb\nc\n");
        let found = find_time(path.to_str().unwrap(), 0, 5, |_| None).unwrap();
        assert_eq!(found, TimeFound::None);
        fs::remove_file(&path).unwrap();
    }
}
