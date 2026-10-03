//! The text in a tile's border.

use crate::tail::Status;

use super::window::HISTORY_MAX;
use super::{Content, Tile};

/// How much of a command's error message goes into the title. A long one (ssh explains at
/// length) would push the name of the source out of it.
pub(super) const MAX_WHY: usize = 44;

/// `text` cut to at most `max` characters, with `…` where it was cut.
pub(super) fn shorten(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{head}…")
}

impl Tile {
    /// `name` labels a main tile (the source's name or path); filter tiles describe themselves.
    pub fn title(&self, name: &str) -> String {
        match &self.content {
            Content::Source {
                live_capacity,
                lines,
                status,
                window,
                ..
            } => {
                // Older lines exist on disk. Scrolling up loads them, up to a limit.
                let more = match (
                    lines.has_older(),
                    lines.len() >= *live_capacity + HISTORY_MAX,
                ) {
                    _ if window.is_some() => " · G = back to the live end",
                    (false, _) => "",
                    (true, false) => " · more above (scroll up)",
                    (true, true) => " · history limit (use & to filter)",
                };
                // A command's state comes first: when it is not running, that matters most.
                let state = match status {
                    None | Some(Status::Connected) => String::new(),
                    Some(Status::Connecting) => " · connecting…".to_string(),
                    Some(Status::Retrying { in_secs, why }) => {
                        format!(" · ⟳ retry in {in_secs}s ({})", shorten(why, MAX_WHY))
                    }
                    Some(Status::Ended { why }) => format!(" · ended: {}", shorten(why, MAX_WHY)),
                };
                format!(" {name}{state}{more} ")
            }
            Content::Merged { merger, .. } => {
                format!(" {name} · {} sources ", merger.labels().len())
            }
            Content::StreamFilter(view) => {
                format!(
                    " filter: {} · {} matches · live only ",
                    view.filter().label(),
                    view.len()
                )
            }
            Content::Filter(view) => {
                let scanning = if view.is_scanning() {
                    " · scanning…"
                } else {
                    ""
                };
                let at = view
                    .current_index()
                    .map_or(String::new(), |i| format!("{}/", i + 1));
                format!(
                    " filter: {} · {at}{} matches{scanning} ",
                    view.filter().label(),
                    view.len()
                )
            }
        }
    }
}
