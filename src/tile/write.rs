//! `:write`: saving what a tile holds to a file.

use std::fs::OpenOptions;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use super::{Content, Tile};

/// Rows fetched per step, so a filter over a huge file is read in pieces.
const WRITE_CHUNK: usize = 4096;

impl Tile {
    /// Writes every line of the tile to `path`, one per line, as they are in the log (for a
    /// filter, every match, read from disk; for the main tile, the lines that are loaded).
    /// `replace` allows overwriting an existing file; otherwise it must not exist. Returns the
    /// number of lines written.
    pub fn write_to(&self, path: &Path, replace: bool) -> io::Result<u64> {
        let mut options = OpenOptions::new();
        options.write(true);
        if replace {
            options.create(true).truncate(true);
        } else {
            options.create_new(true);
        }
        let mut out = BufWriter::new(options.open(path)?);

        let lines = self.content.lines();
        let (mut seq, end) = (lines.first_seq(), lines.end_seq());
        let mut written = 0;
        while seq < end {
            let rows = lines.range(seq, WRITE_CHUNK);
            if rows.is_empty() {
                break;
            }
            for row in &rows {
                out.write_all(row.as_bytes())?;
                out.write_all(b"\n")?;
            }
            seq += rows.len() as u64;
            written += rows.len() as u64;
        }
        out.flush()?;
        Ok(written)
    }

    /// Does the tile hold the whole log, so that `write_to` saves all of it? A file's main tile
    /// holds only the end of it unless the whole file is small; a command's output is only
    /// what is in memory too, but there is nothing more to get.
    pub fn holds_whole_log(&self) -> bool {
        match &self.content {
            Content::Source { lines, window, .. } => window.is_none() && !lines.has_older(),
            _ => true,
        }
    }
}
