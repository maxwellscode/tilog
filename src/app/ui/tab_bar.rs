//! The bar on top: brand, tabs, clock.

use crate::NAME;
use crate::VERSION;
use crate::app::App;
use crate::merge;
use crate::source::Source;
use crate::timestamp;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use std::ops::Range;

/// The overview's tab, as it is drawn, for measuring.
const OVERVIEW_TAB: &str = " 0: Overview ";

/// Columns kept free at the right end of the tab bar for the clock (`19:23:13 UTC`) and a gap.
pub(super) const CLOCK_RESERVE: usize = 15;

impl App {
    /// The tab bar for a screen `width` columns wide, and the horizontal span of each tab so
    /// mouse clicks can be mapped back. Long names are shortened until all the tabs fit.
    pub(super) fn tab_bar(&self, width: u16) -> (Line<'static>, Vec<Range<u16>>) {
        let active = self.tab.checked_sub(1);
        let names: Vec<&str> = self.sources.iter().map(Source::name).collect();
        let digits = if self.sources.len() >= 10 { 2 } else { 1 }; // "10: merged"
        let overview_width = OVERVIEW_TAB.chars().count() + 1;

        // The labels of the source tabs for a brand of this width: the line, minus the brand, the
        // overview tab and the clock is what they may use. Returns whether
        // the names had to be shortened at all.
        let fit = |brand: &str| -> (Vec<String>, bool) {
            let room = usize::from(width)
                .saturating_sub(brand.chars().count() + 2 + overview_width + CLOCK_RESERVE);
            let mut fitted = fit_labels(&names, active, room, digits);
            let tight = fitted.iter().zip(&names).any(|(fit, name)| fit != name);
            if tight {
                // They don't fit as they are. Drop what the names share and the `.log` first
                // (`example_nginx.log`, `example_redis.log` become `nginx`, `redis`), and cut
                // only what is still too long.
                let compact = self.compact_names();
                let compact: Vec<&str> = compact.iter().map(String::as_str).collect();
                fitted = fit_labels(&compact, active, room, digits);
            }
            (fitted, tight)
        };

        // The version is the first thing to go when space is tight: `tilog` is 7 columns
        // shorter than `tilog v0.1.0`, which the tab names can use.
        let full = format!("{NAME} v{VERSION}");
        let (mut fitted, tight) = fit(&full);
        let brand = if tight {
            fitted = fit(NAME).0;
            NAME.to_string()
        } else {
            full
        };

        let brand_width = brand.chars().count() + 2;
        let mut x = u16::try_from(brand_width).unwrap_or(u16::MAX);
        // Without color, the name is bold: in the terminal's own text color, that is the plainest
        // way to make it stand out without a background.
        let brand_style = if self.no_color {
            Style::new().bold()
        } else {
            Style::new().bold().cyan()
        };
        // The version is only a reference: grayed out, so the name is what the eye finds. (A
        // terminal has one size of text, so gray and dim is all that can make it smaller.)
        let version_style = if self.no_color {
            Style::new().dim()
        } else {
            Style::new().dark_gray()
        };
        let mut spans = vec![Span::styled(NAME, brand_style)];
        if let Some(version) = brand.strip_prefix(NAME).filter(|rest| !rest.is_empty()) {
            spans.push(Span::styled(version.to_string(), version_style));
        }
        spans.push(Span::raw("  "));
        let mut ranges = Vec::new();

        let labels = std::iter::once("Overview".to_string()).chain(fitted);
        for (index, label) in labels.enumerate() {
            // Tabs are numbered like the keys that open them: `0` is the overview, `1`, `2`, ...
            // the sources (the numbers `:merge 1 2` takes).
            let text = format!(" {index}: {label} ");
            let width = u16::try_from(text.chars().count()).unwrap_or(u16::MAX);
            let style = if index == self.tab {
                Style::new().black().on_cyan().bold()
            } else {
                Style::new().gray()
            };
            spans.push(Span::styled(text, style));
            spans.push(Span::raw(" "));
            ranges.push(x..x.saturating_add(width));
            x = x.saturating_add(width + 1);
        }
        (Line::from(spans), ranges)
    }

    /// The source names without their common start and `.log` (see `merge::short_labels`). The
    /// merged timeline is left out of that: its name shares nothing with the files.
    pub(super) fn compact_names(&self) -> Vec<String> {
        let sources: Vec<&Source> = self
            .sources
            .iter()
            .filter(|source| !source.is_merged())
            .collect();
        let names: Vec<&str> = sources.iter().map(|source| source.name()).collect();
        let mut short = merge::short_labels(&names).into_iter();
        self.sources
            .iter()
            .map(|source| {
                if source.is_merged() {
                    source.name().to_string()
                } else {
                    short.next().unwrap_or_default()
                }
            })
            .collect()
    }

    /// Which tab, if any, is at screen column `x` in a tab bar `width` columns wide.
    pub(in crate::app) fn tab_at(&self, x: u16, width: u16) -> Option<usize> {
        self.tab_bar(width)
            .1
            .iter()
            .position(|range| range.contains(&x))
    }

    pub(super) fn render_tabbar(&self, frame: &mut Frame, area: Rect) {
        let (tabs, ranges) = self.tab_bar(area.width);
        frame.render_widget(tabs, area);

        // The current time in UTC at the right edge: the time of every timestamp tilog
        // merges, and a way to see how old the newest line is. (A span is styled, not the
        // line: a line's style would repaint the whole area, including the tabs.)
        //
        // Where a source comes from is not here: it belongs to one source, so it is in the
        // title of that source's window.
        let used = ranges.last().map_or(0, |range| range.end);
        let clock = format!("{} UTC", timestamp::utc_clock(timestamp::now_ms()));
        let clock_width = u16::try_from(clock.chars().count() + 1).unwrap_or(0);
        if area.width > used + clock_width + 2 {
            let clock = Span::styled(clock, Style::new().gray());
            frame.render_widget(Line::from(clock).right_aligned(), area);
        }
    }
}

/// The labels of the source tabs, shortened so that all of them fit in `room` columns.
///
/// Names that are already short stay as they are. The longest ones lose a character at a time
/// (the cut shows as `…`), and the tab that is open is the last to be touched, because it is the
/// one you are reading. A label is never cut below a few characters: the tab number in front
/// of it still tells the tabs apart.
pub(super) fn fit_labels(
    names: &[&str],
    active: Option<usize>,
    room: usize,
    number_digits: usize,
) -> Vec<String> {
    const SMALLEST: usize = 2;
    // Around a label: " 3: " in front (one more column for a two-digit number), " " behind,
    // and a space between tabs.
    let around = 5 + number_digits;

    let original: Vec<Vec<char>> = names.iter().map(|name| name.chars().collect()).collect();
    let mut lengths: Vec<usize> = original.iter().map(Vec::len).collect();
    let used = |lengths: &[usize]| lengths.iter().map(|len| len + around).sum::<usize>();

    while used(&lengths) > room {
        // The longest label that may still shrink, the open tab last.
        let candidate = (0..lengths.len())
            .filter(|&i| lengths[i] > SMALLEST)
            .max_by_key(|&i| (Some(i) != active, lengths[i]));
        match candidate {
            Some(i) => lengths[i] -= 1,
            None => break,
        }
    }

    original
        .iter()
        .zip(&lengths)
        .map(|(chars, &len)| {
            if len < chars.len() {
                // The last character of the shortened label is the ellipsis.
                chars[..len - 1].iter().collect::<String>() + "…"
            } else {
                chars.iter().collect()
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_is_grayed_out_next_to_the_name() {
        let app = App::new();
        let spans = app.tab_bar(120).0.spans;
        assert_eq!(spans[0].content, "tilog");
        assert_eq!(spans[1].content, format!(" v{VERSION}"));
        assert_eq!(spans[1].style.fg, Some(ratatui::style::Color::DarkGray));
        assert_ne!(spans[0].style.fg, spans[1].style.fg);
    }

    #[test]
    fn without_color_the_name_is_bold_text() {
        let mut app = App::new();
        let brand = |app: &App| app.tab_bar(120).0.spans[0].style;
        assert_eq!(brand(&app).fg, Some(ratatui::style::Color::Cyan));
        app.no_color = true;
        let style = brand(&app);
        assert!(style.add_modifier.contains(ratatui::style::Modifier::BOLD));
        assert!(
            !style
                .add_modifier
                .contains(ratatui::style::Modifier::REVERSED),
            "no white background"
        );
        assert_eq!(style.fg, None);
    }

    #[test]
    fn clicks_find_the_right_tab_and_the_gaps_between_tabs_are_no_tab() {
        let dir = std::env::temp_dir().join(format!("tilog-tabs-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = App::new();
        for name in ["one.log", "two.log"] {
            let path = dir.join(name);
            std::fs::write(&path, "x\n").unwrap();
            app.add_path(path.to_str().unwrap()).unwrap();
        }

        let (line, ranges) = app.tab_bar(120);
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert!(
            !text.contains('│'),
            "no divider after the overview: {text:?}"
        );

        // Every tab is found by clicking anywhere on its label, the space between two tabs is
        // no tab, and the tabs are not shifted off their labels.
        assert_eq!(ranges.len(), 3);
        for (index, range) in ranges.iter().enumerate() {
            assert_eq!(app.tab_at(range.start, 120), Some(index));
            assert_eq!(app.tab_at(range.end - 1, 120), Some(index));
        }
        assert_eq!(app.tab_at(ranges[0].end, 120), None);
        let label_at = |range: &std::ops::Range<u16>| -> String {
            text.chars()
                .skip(usize::from(range.start))
                .take(usize::from(range.end - range.start))
                .collect()
        };
        assert_eq!(label_at(&ranges[0]).trim(), "0: Overview");
        assert_eq!(label_at(&ranges[1]).trim(), "1: one.log");
        assert_eq!(label_at(&ranges[2]).trim(), "2: two.log");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn fit(names: &[&str], active: Option<usize>, room: usize) -> Vec<String> {
        fit_labels(names, active, room, 1)
    }

    #[test]
    fn labels_that_fit_are_left_alone() {
        assert_eq!(
            fit(&["web.log", "db.log"], None, 100),
            ["web.log", "db.log"]
        );
        assert!(fit(&[], None, 0).is_empty());
    }

    #[test]
    fn the_longest_labels_shrink_first() {
        // 10 + 2 + 10 characters plus 3 x 6 around them = 40; only 34 columns are free.
        assert_eq!(
            fit(&["aaaaaaaaaa", "bb", "cccccccccc"], None, 34),
            ["aaaaaa…", "bb", "cccccc…"]
        );
    }

    #[test]
    fn the_open_tab_is_shortened_last() {
        let names = ["long_active_name", "other_long_name"];
        // Short by 8 columns: the other tab pays all of it.
        assert_eq!(fit(&names, Some(0), 35), ["long_active_name", "other_…"]);
        // Short by much more: the other one bottoms out, then the active one gives way too.
        let tight = fit(&names, Some(0), 20);
        assert_eq!(tight[1], "o…");
        assert!(tight[0].chars().count() < "long_active_name".chars().count());
    }

    #[test]
    fn a_label_is_never_cut_below_a_couple_of_characters() {
        assert_eq!(fit(&["abcdef", "ghijkl"], None, 0), ["a…", "g…"]);
        assert_eq!(
            fit(&["ä", "ö"], None, 0),
            ["ä", "ö"],
            "short names stay whole"
        );
        assert_eq!(
            fit(&["äöüßäöüß"], None, 0),
            ["ä…"],
            "cut on characters, not bytes"
        );
    }

    #[test]
    fn nine_sources_with_long_names_still_fit_the_tab_bar() {
        let dir = std::env::temp_dir().join(format!("tilog-nine-tabs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = App::new();
        for i in 0..9 {
            let path = dir.join(format!("a_rather_long_log_file_name_{i}.log"));
            std::fs::write(&path, "x\n").unwrap();
            app.add_path(path.to_str().unwrap()).unwrap();
        }
        app.tab = 4; // the open tab keeps the most of its name

        let width: u16 = 130;
        let (line, ranges) = app.tab_bar(width);
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(ranges.len(), 10);
        assert!(
            usize::from(ranges.last().unwrap().end) + CLOCK_RESERVE <= usize::from(width),
            "the tabs run into the clock: {text:?}"
        );

        // Every tab can be clicked, and the open one is the most readable.
        let label = |range: &std::ops::Range<u16>| -> String {
            text.chars()
                .skip(usize::from(range.start))
                .take(usize::from(range.end - range.start))
                .collect()
        };
        for (index, range) in ranges.iter().enumerate() {
            assert_eq!(app.tab_at(range.start, width), Some(index));
            if index > 0 {
                assert!(
                    label(range).trim().starts_with(&format!("{index}: ")),
                    "{:?}",
                    label(range)
                );
            }
        }
        let open = label(&ranges[4]).chars().count();
        assert!(
            ranges
                .iter()
                .enumerate()
                .all(|(i, r)| i == 4 || label(r).chars().count() <= open)
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_version_goes_first_when_space_is_tight() {
        let dir = std::env::temp_dir().join(format!("tilog-brand-tabs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = App::new();
        for name in [
            "checkout_service.log",
            "payments_service.log",
            "inventory_service.log",
        ] {
            let path = dir.join(name);
            std::fs::write(&path, "x\n").unwrap();
            app.add_path(path.to_str().unwrap()).unwrap();
        }
        let text = |width| -> String {
            app.tab_bar(width)
                .0
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        };

        // Plenty of room: version and full names.
        let roomy = text(200);
        assert!(roomy.contains("tilog v0.1.0") && roomy.contains("1: checkout_service.log"));

        // These three names need exactly 79 columns, the version and the rest of the bar take
        // 43: 122 columns fit it all.
        let just_fits = text(122);
        assert!(
            just_fits.contains("tilog v0.1.0") && !just_fits.contains('…'),
            "{just_fits:?}"
        );

        // One column less: the version goes (7 columns), and the names stay whole.
        let drop_version = text(121);
        assert!(!drop_version.contains("v0.1.0"), "{drop_version:?}");
        assert!(drop_version.starts_with("tilog  "), "{drop_version:?}");
        for name in [
            "checkout_service.log",
            "payments_service.log",
            "inventory_service.log",
        ] {
            assert!(
                drop_version.contains(name),
                "{name} was cut: {drop_version:?}"
            );
        }

        // Far too narrow even then: now the names are cut as well.
        let narrow = text(80);
        assert!(
            !narrow.contains("v0.1.0") && narrow.contains('…'),
            "{narrow:?}"
        );

        // Clicking still lands on the right tabs with the shorter brand.
        for width in [120, 80] {
            let (_, ranges) = app.tab_bar(width);
            for (index, range) in ranges.iter().enumerate() {
                assert_eq!(app.tab_at(range.start, width), Some(index));
            }
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_tenth_tab_for_the_timeline_fits_and_can_be_clicked() {
        let dir = std::env::temp_dir().join(format!("tilog-ten-tabs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = App::new();
        for i in 0..9 {
            let path = dir.join(format!("source_number_{i}.log"));
            std::fs::write(&path, format!("2026-10-03 08:00:0{i} line\n")).unwrap();
            app.add_path(path.to_str().unwrap()).unwrap();
        }
        app.run_command_line("merge");
        assert_eq!(app.sources.len(), 10);

        let width: u16 = 140;
        let (line, ranges) = app.tab_bar(width);
        let text: String = line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(ranges.len(), 11);
        assert!(
            usize::from(ranges.last().unwrap().end) + CLOCK_RESERVE <= usize::from(width),
            "{text:?}"
        );

        let last: String = text
            .chars()
            .skip(usize::from(ranges[10].start))
            .take(usize::from(ranges[10].end - ranges[10].start))
            .collect();
        assert!(
            last.trim().starts_with("10: "),
            "the two-digit number is shown: {last:?}"
        );
        assert_eq!(app.tab_at(ranges[10].start, width), Some(10));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn two_digit_tab_numbers_take_one_more_column() {
        // Two names of 10 characters: with 1-digit numbers 2 x (10 + 6) = 32 columns...
        assert_eq!(
            fit_labels(&["abcdefghij", "klmnopqrst"], None, 32, 1),
            ["abcdefghij", "klmnopqrst"]
        );
        // ...with 2-digit numbers 34, so the same room is one column short for each.
        let tighter = fit_labels(&["abcdefghij", "klmnopqrst"], None, 32, 2);
        assert_eq!(
            tighter.iter().map(|l| l.chars().count()).sum::<usize>(),
            20 - 2
        );
    }

    #[test]
    fn tabs_that_share_a_prefix_are_shortened_by_dropping_it() {
        let dir = std::env::temp_dir().join(format!("tilog-prefix-tabs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut app = App::new();
        for name in [
            "example_nginx_access.log",
            "example_postgres.log",
            "example_redis.log",
            "example_syslog.log",
        ] {
            let path = dir.join(name);
            std::fs::write(&path, "x\n").unwrap();
            app.add_path(path.to_str().unwrap()).unwrap();
        }
        let text = |width| -> String {
            app.tab_bar(width)
                .0
                .spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect()
        };

        // Room enough: the names are shown as they are.
        assert!(text(200).contains("1: example_nginx_access.log"));
        // Not enough: the shared start and `.log` go, and the tabs stay tell-apart-able.
        let tight = text(100);
        for expected in ["1: nginx_access", "2: postgres", "3: redis", "4: syslog"] {
            assert!(
                tight.contains(expected),
                "{expected:?} missing in {tight:?}"
            );
        }
        assert!(!tight.contains("example_") && !tight.contains(".log"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
