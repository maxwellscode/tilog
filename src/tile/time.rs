//! `:goto <time>`: jumping to a moment in the log.

use std::fs;

use crate::history::{self, TimeFound};
use crate::timestamp;
use crate::when::When;

use super::{Content, Tile};

/// Rows read per step while scanning, and the most rows a scan looks at.
const SCAN_CHUNK: usize = 512;
const SCAN_LIMIT: u64 = 1_000_000;

/// How many of the newest rows are looked at to find out what day the log is on.
const NEWEST_ROWS: usize = 500;

/// Where `jump_to_time` landed.
#[derive(Debug, PartialEq)]
pub struct Landed {
    /// The time of the line that is shown.
    pub time: i64,
    /// The moment asked for is after every line: this is the last one with a time.
    pub past_end: bool,
}

impl Tile {
    /// Shows the first line at or after `when`, marked, with context above it. The tile does not
    /// need a line at exactly that time. A file is searched on disk (so lines that are not
    /// loaded are found too); anything else is searched in the rows it holds.
    pub fn jump_to_time(&mut self, when: &When) -> Result<Landed, String> {
        let now = timestamp::now_ms();
        let height = self.shown_height.get();
        if let Content::Source {
            path: Some(path),
            lines,
            ..
        } = &self.content
        {
            let (path, floor) = (path.clone(), lines.floor());
            return self.jump_to_time_in_file(&path, floor, when, now, height);
        }
        self.jump_to_time_in_rows(when, now, height)
    }

    fn jump_to_time_in_file(
        &mut self,
        path: &str,
        floor: u64,
        when: &When,
        now: i64,
        height: usize,
    ) -> Result<Landed, String> {
        let io_error = |e: std::io::Error| format!("cannot read {path}: {e}");
        let len = fs::metadata(path).map_err(io_error)?.len();
        let tail = history::read_before(path, len, floor, NEWEST_ROWS).map_err(io_error)?;
        let newest = tail
            .iter()
            .rev()
            .find_map(|(_, text)| timestamp::parse(text, now))
            .ok_or("no timestamps in this log")?;
        let target = when.resolve(newest);

        let found = history::find_time(path, floor, target, |line| timestamp::parse(line, now))
            .map_err(io_error)?;
        let (offset, time, past_end) = match found {
            TimeFound::At { offset, time } => (offset, time, false),
            TimeFound::AfterEnd { offset, time } => (offset, time, true),
            TimeFound::None => return Err("no timestamps in this log".to_string()),
        };
        if !self.show_offset(offset, 1, height) {
            return Err("cannot show that line".to_string());
        }
        Ok(Landed { time, past_end })
    }

    fn jump_to_time_in_rows(
        &mut self,
        when: &When,
        now: i64,
        height: usize,
    ) -> Result<Landed, String> {
        let skip = self.label_cols();
        let lines = self.content.lines();
        let (first, end) = (lines.first_seq(), lines.end_seq());
        let time_of =
            |row: &str| timestamp::parse(&row.chars().skip(skip).collect::<String>(), now);

        // The newest time tells which day a bare `08:16` means.
        let newest_from = end.saturating_sub(NEWEST_ROWS as u64).max(first);
        let newest = lines
            .range(newest_from, NEWEST_ROWS)
            .iter()
            .rev()
            .find_map(|row| time_of(row))
            .ok_or("no timestamps in this tile")?;
        let target = when.resolve(newest);

        let (mut seq, mut last) = (first, None);
        while seq < end && seq - first < SCAN_LIMIT {
            let rows = lines.range(seq, SCAN_CHUNK);
            for (i, row) in rows.iter().enumerate() {
                let Some(time) = time_of(row) else { continue };
                if time >= target {
                    return Ok(self.land_on(seq + i as u64, time, false, height));
                }
                last = Some((seq + i as u64, time));
            }
            seq += rows.len().max(1) as u64;
        }
        let (row, time) = last.ok_or("no timestamps in this tile")?;
        Ok(self.land_on(row, time, true, height))
    }

    fn land_on(&mut self, row: u64, time: i64, past_end: bool, height: usize) -> Landed {
        self.show_row(row, height);
        Landed { time, past_end }
    }
}
