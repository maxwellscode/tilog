//! Where a tile's lines come from.

use std::sync::mpsc::Receiver;

use crate::buffer::RingBuffer;
use crate::filter_view::FilterView;
use crate::lines::Lines;
use crate::merge::Merger;
use crate::stream::StreamGuard;
use crate::stream_filter::StreamFilterView;
use crate::tail::{Status, TailMsg};

use super::window::HistoryWindow;

/// Where a tile's lines come from. The set of kinds is closed (we know them all), so an
/// `enum` fits; the shared *reading* interface is the `Lines` trait.
pub(super) enum Content {
    /// The main tile: the end of the file (or the latest output of a command), kept in a
    /// bounded ring buffer. For a file, older lines are read from `path` when asked for.
    Source {
        /// `None` for a command: its past can't be read again.
        path: Option<String>,
        live_capacity: usize,
        lines: RingBuffer,
        rx: Receiver<TailMsg>,
        /// The command's state, if this is a command.
        status: Option<Status>,
        /// Dropping it stops the command.
        _guard: Option<StreamGuard>,
        /// When set, this is what is shown instead of `lines` (see `HistoryWindow`).
        window: Option<HistoryWindow>,
    },
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

impl Content {
    // Both variants are coerced to `&dyn Lines` here, in one place.
    pub(super) fn lines(&self) -> &dyn Lines {
        match self {
            Self::Source {
                window: Some(window),
                ..
            } => &window.lines,
            Self::Source { lines, .. } => lines,
            Self::Filter(view) => view,
            Self::StreamFilter(view) => view,
            Self::Merged { lines, .. } => lines,
        }
    }
}
