//! Where a tile's lines come from.

use std::sync::mpsc::Receiver;

use crate::buffer::RingBuffer;
use crate::filter_view::FilterView;
use crate::lines::Lines;
use crate::merge::Merger;
use crate::stream::StreamGuard;
use crate::stream_filter::StreamFilterView;
use crate::tail::{Status, TailMsg};

use super::login::Asking;
use super::window::{HISTORY_MAX, HistoryWindow};

/// The main tile of a source: the end of the file (or the latest output of a command), kept in
/// a bounded ring buffer. For a file, older lines are read from `path` when asked for.
pub(super) struct SourceContent {
    /// `None` for a command: its past can't be read again.
    pub(super) path: Option<String>,
    pub(super) live_capacity: usize,
    pub(super) lines: RingBuffer,
    pub(super) rx: Receiver<TailMsg>,
    /// The command's state, if this is a command.
    pub(super) status: Option<Status>,
    /// Dropping it stops the command.
    pub(super) _guard: Option<StreamGuard>,
    /// When set, this is what is shown instead of `lines` (see `HistoryWindow`).
    pub(super) window: Option<HistoryWindow>,
    /// A question `ssh` is waiting to have answered (see `login`).
    pub(super) asking: Option<Asking>,
}

/// Where a tile's lines come from. The set of kinds is closed (we know them all), so an
/// `enum` fits; the shared *reading* interface is the `Lines` trait.
pub(super) enum Content {
    /// The main tile of a source (see `SourceContent`).
    Source(SourceContent),
    /// A filtered tile of a file: the whole file, as an index of byte offsets.
    Filter(FilterView),
    /// A filtered tile of a command's output: what matched, kept in memory.
    StreamFilter(StreamFilterView),
    /// Several sources interleaved by timestamp. Each row starts with the source's label.
    Merged {
        lines: RingBuffer,
        merger: Merger,
        /// Characters in front of the text of each row: the label column and its bar.
        label_cols: usize,
    },
}

/// Per UI tick, take at most this many lines from a source, so a huge file being loaded can't
/// freeze the UI.
const MAX_LINES_PER_PUMP: usize = 4096;

impl SourceContent {
    /// The lines on show: a stretch of the file loaded from disk if there is one, otherwise
    /// the live end.
    pub(super) fn shown(&self) -> &dyn Lines {
        match &self.window {
            Some(window) => &window.lines,
            None => &self.lines,
        }
    }

    /// Takes in what the reader thread has delivered: lines, a rotation, a state, a question.
    /// New lines are also copied into `fresh`, if given. Returns `true` if more may be waiting.
    pub(super) fn pump(&mut self, following: bool, mut fresh: Option<&mut Vec<String>>) -> bool {
        // Older lines loaded by scrolling up are kept only while the view is paused. Following
        // the file again shrinks the buffer back to its live size.
        self.lines.set_capacity(if following {
            self.live_capacity
        } else {
            self.live_capacity + HISTORY_MAX
        });

        // Report "more may be waiting" whenever something was taken in, not only after a full
        // batch: the channel is smaller than a batch, and the UI must come straight back for
        // the next chunk instead of idling.
        let mut taken = 0;
        for _ in 0..MAX_LINES_PER_PUMP {
            match self.rx.try_recv() {
                Ok(TailMsg::Line { offset, text }) => {
                    taken += 1;
                    if let Some(fresh) = fresh.as_deref_mut() {
                        fresh.push(text.clone());
                    }
                    self.lines.push(offset, text);
                }
                Ok(TailMsg::Rotated) => self.lines.rotated(),
                Ok(TailMsg::Status(status)) => self.status = Some(status),
                Ok(TailMsg::Ask(ask)) => {
                    taken += 1;
                    // Only one question is open at a time; a newer one replaces it.
                    if let Some(old) = self.asking.replace(Asking::new(ask)) {
                        old.cancel();
                    }
                }
                Err(_) => return taken > 0,
            }
        }
        true
    }
}

impl Content {
    // All kinds are coerced to `&dyn Lines` here, in one place.
    pub(super) fn lines(&self) -> &dyn Lines {
        match self {
            Self::Source(source) => source.shown(),
            Self::Filter(view) => view,
            Self::StreamFilter(view) => view,
            Self::Merged { lines, .. } => lines,
        }
    }
}
