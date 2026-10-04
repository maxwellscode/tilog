//! What a paused tile shows: the lines it had when it stopped following.
//!
//! Pausing a view that has fewer lines than the window is tall (a docker source that only
//! prints one error again and again, a short file) would not keep it still: every new line
//! lands inside the window. So a view that is not following is cut off at the end the lines had
//! when following stopped. The newer lines are still collected, and appear when following
//! resumes (scrolling down to the bottom, `F`, `G` or `:follow`).

use crate::lines::Lines;

use super::content::Content;

/// The lines of a tile as the view sees them: all of them, or only those up to `end`.
pub(super) struct Bounded<'a> {
    inner: &'a dyn Lines,
    end: Option<u64>,
}

impl<'a> Bounded<'a> {
    /// The lines of `content`, cut off at `frozen_end` if there is one. A stretch of the file
    /// loaded from disk (a history window) has numbers of its own, which `frozen_end` does not
    /// refer to, so it is never cut.
    pub(super) fn of(content: &'a Content, frozen_end: Option<u64>) -> Self {
        let end = match content {
            Content::Source {
                window: Some(_), ..
            } => None,
            _ => frozen_end,
        };
        Self {
            inner: content.lines(),
            end,
        }
    }
}

impl Lines for Bounded<'_> {
    fn first_seq(&self) -> u64 {
        self.inner.first_seq()
    }

    fn end_seq(&self) -> u64 {
        // Never before the first line: the frozen lines may have been evicted since.
        match self.end {
            Some(end) => end.min(self.inner.end_seq()).max(self.inner.first_seq()),
            None => self.inner.end_seq(),
        }
    }

    fn range(&self, from_seq: u64, count: usize) -> Vec<String> {
        let room = usize::try_from(self.end_seq().saturating_sub(from_seq)).unwrap_or(usize::MAX);
        self.inner.range(from_seq, count.min(room))
    }
}
