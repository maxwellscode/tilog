//! The help screen.

use super::text::wrap_words;
use crate::NAME;
use crate::VERSION;
use crate::app::{App, BORDER_ROWS};
use crate::cli;
use crate::command;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Style, Stylize};
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Block;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;

impl App {
    pub(super) fn render_help(&self, frame: &mut Frame) {
        let area = frame.area();
        let text = help_lines(help_inner_width(area.width));
        let width = area.width.min(MAX_WIDTH);
        let height = (text.len() as u16 + BORDER_ROWS).min(area.height);
        let popup = Rect::new(
            area.x + (area.width - width) / 2,
            area.y + (area.height - height) / 2,
            width,
            height,
        );
        let visible = usize::from(height.saturating_sub(BORDER_ROWS));
        let scroll = self.help_scroll.min(text.len().saturating_sub(visible));

        // `Clear` wipes whatever was drawn underneath, so the popup is not see-through.
        frame.render_widget(Clear, popup);
        let block = Block::bordered()
            .title(" Help · j/k scroll · q close ")
            .border_style(Style::new().cyan());
        let help = Paragraph::new(text)
            .block(block)
            .scroll((u16::try_from(scroll).unwrap_or(0), 0));
        frame.render_widget(help, popup);
    }
}

/// The widest the help window gets.
const MAX_WIDTH: u16 = 120;

/// The key rows: what to press, and what it does.
const KEYS: &[(&str, &str)] = &[
    (
        "j k ↑ ↓",
        "scroll one line (Enter also scrolls down in a tab)",
    ),
    ("Space b", "page down / up (also f, PageDown, PageUp)"),
    ("d u", "half a page down / up"),
    (
        "g G",
        "first line / last line and follow (also Home, End, F)",
    ),
    ("h l ← →", "scroll sideways"),
    (
        "/text",
        "search: mark text everywhere, jump to the first match (smart case)",
    ),
    (
        "n N",
        "next / previous search match; in a filter pane its entries (or the search hits, if a search is on), shown in the main pane. Scroll the pane first to start near the one you want",
    ),
    (
        "] [",
        "next / previous error line (ERROR, level=error, [crit], HTTP 5xx, ...)",
    ),
    (
        "&text",
        "filter: open a tile with the matching entries (-r regex, -i ignore case)",
    ),
    (
        ":",
        "run a command (a menu appears: Tab or Enter picks one, then Enter runs it)",
    ),
    ("Tab Shift+Tab", "select the next / previous window"),
    (
        "Enter",
        "on the overview: open the selected source in its own tab",
    ),
    (
        "0-9  Alt+← →",
        "switch tabs (0 is the overview; up to 9 sources, plus one timeline)",
    ),
    ("m", "jump to the merged timeline (it can be the tenth tab)"),
    ("Esc", "clear the search; then go back to the overview"),
    (
        "q",
        "back to the overview; on the overview quit (Ctrl+C always quits)",
    ),
    (
        "mouse",
        "click selects a window or tab; drag selects text and copies it; wheel scrolls",
    ),
];

/// Width of the key column.
const KEY_COLUMN: usize = 16;

/// The text columns of the help window on a screen `screen_width` wide: the window is at most
/// `MAX_WIDTH` wide and loses two columns to its border.
pub(in crate::app) fn help_inner_width(screen_width: u16) -> usize {
    usize::from(screen_width.min(MAX_WIDTH)).saturating_sub(2)
}

/// A row of the help: a label in a column of `label_width`, then the text, which is wrapped to
/// `inner_width` with the following lines indented to line up under it.
fn row(label: &str, label_width: usize, text: &str, inner_width: usize) -> Vec<Line<'static>> {
    let room = inner_width.saturating_sub(label_width).max(MIN_TEXT);
    let mut pieces = wrap_words(text, room).into_iter();
    let first = pieces.next().unwrap_or_default();
    let mut lines = vec![Line::from(vec![
        Span::from(format!("{label:<label_width$}")).cyan(),
        Span::raw(first),
    ])];
    lines.extend(pieces.map(|piece| Line::from(format!("{}{piece}", " ".repeat(label_width)))));
    lines
}

/// The least width the text of a row is wrapped to, however narrow the window.
const MIN_TEXT: usize = 20;

/// The help screen for a window `inner_width` columns wide: what this is, the keys, and every
/// command. Long descriptions are wrapped, never cut off.
pub(in crate::app) fn help_lines(inner_width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(format!("{NAME} v{VERSION}")).bold().cyan()];
    lines.extend(
        wrap_words(cli::DESCRIPTION, inner_width.max(MIN_TEXT))
            .into_iter()
            .map(Line::from),
    );
    lines.push(Line::raw(""));
    lines.push(Line::from("Keys").bold());
    for (keys, what) in KEYS {
        lines.extend(row(keys, KEY_COLUMN, what, inner_width));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from("Commands (type : first)").bold());
    let column = command::usage_column_width();
    for spec in command::SPECS {
        lines.extend(row(spec.usage, column, spec.help, inner_width));
    }
    lines.push(Line::raw(""));
    for note in [
        "Sources: a file, ssh:host:/path, docker:name, kube:pod, ssh:host:docker:name, ssh:host:kube:pod, cmd:command.",
        "Colors and named sources: ~/.config/tilog/config.toml (tilog --print-config).",
    ] {
        lines.extend(
            wrap_words(note, inner_width.max(MIN_TEXT))
                .into_iter()
                .map(|l| Line::from(l).dark_gray()),
        );
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(line: &Line) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn every_description_starts_in_the_same_column() {
        // The column used to be a fixed 28 wide, which a longer usage pushed out of line.
        let column = command::usage_column_width();
        for spec in command::SPECS {
            assert!(
                spec.usage.chars().count() < column,
                "{} does not fit",
                spec.usage
            );
        }
        let rows: Vec<_> = help_lines(118)
            .into_iter()
            .filter(|line| {
                let Some(first) = line.spans.first().map(|span| span.content.trim_end()) else {
                    return false;
                };
                command::SPECS.iter().any(|spec| spec.usage == first)
            })
            .collect();
        assert_eq!(rows.len(), command::SPECS.len());
        assert!(
            rows.iter()
                .all(|line| line.spans[0].content.chars().count() == column)
        );
    }

    #[test]
    fn long_descriptions_wrap_under_their_column_instead_of_being_cut() {
        for width in [118, 80, 60] {
            let lines = help_lines(width);
            // Nothing is wider than the window, and every command's whole text is still there.
            assert!(
                lines
                    .iter()
                    .all(|line| plain(line).chars().count() <= width),
                "width {width}"
            );
            let text: String = lines.iter().map(plain).collect::<Vec<_>>().join(" ");
            let squashed: String = text.split_whitespace().collect();
            for spec in command::SPECS {
                let help: String = spec.help.split_whitespace().collect();
                assert!(squashed.contains(&help), "width {width}: {}", spec.help);
            }
        }
        // At 80 columns the longest description needs more than one line, and the second one
        // starts in the description column.
        let column = command::usage_column_width();
        let lines = help_lines(80);
        let at = lines
            .iter()
            .position(|line| plain(line).starts_with(":goto"))
            .expect("the goto row");
        assert!(plain(&lines[at + 1]).starts_with(&" ".repeat(column)));
    }

    #[test]
    fn the_help_lists_every_command_and_the_main_keys() {
        let text: String = help_lines(118)
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        for spec in command::SPECS {
            assert!(text.contains(spec.usage), "help is missing {}", spec.usage);
        }
        for expected in ["tilog v", "/text", "&text", "Space b", "n N"] {
            assert!(text.contains(expected), "help is missing {expected:?}");
        }
    }
}
