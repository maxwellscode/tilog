//! Coloring log lines: the configured rules, and the search highlight on top of them.
//!
//! Each line is painted in layers. Every character starts with the default style. Each rule, in
//! order, paints its matches over that, and finally the highlight paints over everything. A later
//! layer only changes the attributes it sets, so a rule that just says `bold` keeps the color an
//! earlier rule gave.

use std::mem;
use std::ops::Range;

use anyhow::{Result, anyhow};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use regex::Regex;

use crate::config::{Config, RuleConfig, Scope};
use crate::highlight::Highlight;

/// Blue stands out from both the dark default background and the colors of the rules.
const DEFAULT_MARKED_BG: &str = "#2f5f9f";

pub struct Theme {
    rules: Vec<Rule>,
    highlight: Style,
    marked: Style,
}

struct Rule {
    regex: Regex,
    style: Style,
    whole_line: bool,
}

impl Theme {
    /// Compiles the configuration's rules. Fails with a message naming the offending rule.
    pub fn from_config(config: &Config) -> Result<Self> {
        let highlight = Style::new()
            .fg(parse_color(
                config.colors.highlight_fg.as_deref().unwrap_or("black"),
            )?)
            .bg(parse_color(
                config.colors.highlight_bg.as_deref().unwrap_or("yellow"),
            )?);

        let rules = if config.colors.enabled {
            config
                .rules_in_effect()
                .iter()
                .map(Rule::compile)
                .collect::<Result<Vec<_>>>()?
        } else {
            Vec::new()
        };
        let marked = Style::new()
            .bg(parse_color(
                config
                    .colors
                    .marked_bg
                    .as_deref()
                    .unwrap_or(DEFAULT_MARKED_BG),
            )?)
            .add_modifier(Modifier::BOLD);
        Ok(Self {
            rules,
            highlight,
            marked,
        })
    }

    /// The theme you get with no configuration file.
    pub fn builtin() -> Self {
        Self::from_config(&Config::default()).expect("the built-in rules are valid")
    }

    /// The row the filter's `n` / `N` is at: a clearly visible background, and bold.
    pub fn marked_style(&self) -> Style {
        self.marked
    }

    /// `style` as it must look on the marked row: the dim and the blue colors of the rules
    /// (timestamps, brackets, stack frames) would vanish on its blue background, so they turn
    /// light. Every other color stays.
    pub fn on_marked(style: Style) -> Style {
        match style.fg {
            Some(Color::DarkGray | Color::Gray) => style.fg(Color::White),
            Some(Color::Blue | Color::Indexed(4 | 8)) => style.fg(Color::LightCyan),
            _ => style,
        }
    }

    pub fn highlight_style(&self) -> Style {
        self.highlight
    }

    /// Paints every rule over `styles` (one entry per character of `text`).
    fn paint(&self, text: &str, starts: &[usize], styles: &mut [Style]) {
        for rule in &self.rules {
            paint_matches(
                &rule.regex,
                text,
                starts,
                styles,
                rule.style,
                rule.whole_line,
            );
        }
    }
}

impl Rule {
    fn compile(config: &RuleConfig) -> Result<Self> {
        let name = config.name.as_deref().unwrap_or(&config.pattern);
        let regex = Regex::new(&config.pattern).map_err(|e| {
            let reason = e.to_string();
            anyhow!(
                "rule \"{name}\": invalid regex: {}",
                reason.lines().last().unwrap_or("").trim()
            )
        })?;

        let mut style = Style::new();
        if let Some(fg) = &config.fg {
            style = style.fg(parse_color(fg).map_err(|e| anyhow!("rule \"{name}\": {e}"))?);
        }
        if let Some(bg) = &config.bg {
            style = style.bg(parse_color(bg).map_err(|e| anyhow!("rule \"{name}\": {e}"))?);
        }
        for (enabled, modifier) in [
            (config.bold, Modifier::BOLD),
            (config.italic, Modifier::ITALIC),
            (config.dim, Modifier::DIM),
            (config.underline, Modifier::UNDERLINED),
        ] {
            if enabled {
                style = style.add_modifier(modifier);
            }
        }
        Ok(Self {
            regex,
            style,
            whole_line: config.scope == Scope::Line,
        })
    }
}

fn parse_color(text: &str) -> Result<Color> {
    text.trim()
        .parse::<Color>()
        .map_err(|_| anyhow!("unknown color \"{text}\""))
}

/// Byte offset of every character of `text`. Regex matches are byte ranges, but styles are
/// kept per character, and this table converts between the two.
pub fn char_starts(text: &str) -> Vec<usize> {
    text.char_indices().map(|(byte, _)| byte).collect()
}

/// The name of the capture group that narrows what a rule paints: with `(?P<v>...)` in the
/// pattern, only that part of each match is colored, the rest just gives it context
/// (`"level":"(?P<v>error)"` colors `error` but not the quotes).
const VALUE_GROUP: &str = "v";

/// Paints `style` onto the characters matched by `regex` (or onto all of them, if `whole_line`
/// is set and there is any match).
pub fn paint_matches(
    regex: &Regex,
    text: &str,
    starts: &[usize],
    styles: &mut [Style],
    style: Style,
    whole_line: bool,
) {
    if whole_line {
        if regex.is_match(text) {
            for slot in styles.iter_mut() {
                *slot = slot.patch(style);
            }
        }
        return;
    }
    let narrowed = regex
        .capture_names()
        .flatten()
        .any(|name| name == VALUE_GROUP);
    if narrowed {
        for captures in regex.captures_iter(text) {
            if let Some(found) = captures.name(VALUE_GROUP) {
                paint_range(found.range(), starts, styles, style);
            }
        }
        return;
    }
    for found in regex.find_iter(text) {
        paint_range(found.range(), starts, styles, style);
    }
}

/// Paints `style` onto the characters of the byte range `bytes`.
fn paint_range(bytes: Range<usize>, starts: &[usize], styles: &mut [Style], style: Style) {
    if bytes.is_empty() {
        return;
    }
    // The first character at or after each end of the range.
    let from = starts.partition_point(|&byte| byte < bytes.start);
    let to = starts.partition_point(|&byte| byte < bytes.end);
    for slot in &mut styles[from..to] {
        *slot = slot.patch(style);
    }
}

/// Colors for the labels of a merged timeline, one per source (red is left out: it means error).
const LABEL_COLORS: [Color; 7] = [
    Color::Cyan,
    Color::Magenta,
    Color::Yellow,
    Color::Green,
    Color::LightBlue,
    Color::LightMagenta,
    Color::LightYellow,
];

/// The color of a source label. The same label always gets the same color, whichever tab or
/// session it is shown in.
fn label_style(label: &str) -> Style {
    // FNV-1a: a tiny hash, plenty to spread a few names over a few colors.
    let hash = label.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    Style::new().fg(LABEL_COLORS[(hash % LABEL_COLORS.len() as u64) as usize])
}

/// One log row, colored, as the part from character `skip` on that fits in `take` columns.
///
/// The first `label_cols` characters are a source label (merged timelines); they get their own
/// color, and the rules see the text *after* them, so a rule anchored at the start of a line
/// (`^\d{4}-`) still works. Colors and highlight are worked out on the whole row first and the
/// window cut out after, so scrolling sideways never changes what a match looks like.
/// `selected` are the characters (counted in the whole row) to show as selected.
pub fn render_row(
    text: &str,
    theme: &Theme,
    highlight: Option<&Highlight>,
    label_cols: usize,
    skip: usize,
    take: usize,
    selected: Option<Range<usize>>,
) -> Line<'static> {
    let starts = char_starts(text);
    let mut styles = vec![Style::default(); starts.len()];

    let label_cols = label_cols.min(starts.len());
    let body_start = starts.get(label_cols).copied().unwrap_or(text.len());
    let (label, body) = text.split_at(body_start);
    if label_cols > 0 {
        let style = label_style(label.split('│').next().unwrap_or("").trim());
        styles[..label_cols].fill(style);
    }
    let body_starts = char_starts(body);
    theme.paint(body, &body_starts, &mut styles[label_cols..]);
    if let Some(highlight) = highlight {
        highlight.paint(text, &starts, &mut styles, theme.highlight_style());
    }
    // The text selected with the mouse, in reverse video: visible on any color.
    if let Some(range) = selected {
        let end = range.end.min(styles.len());
        for style in &mut styles[range.start.min(end)..end] {
            *style = style.add_modifier(Modifier::REVERSED);
        }
    }

    let end = starts.len().min(skip.saturating_add(take));
    if skip >= end {
        return Line::default();
    }

    // Neighbouring characters with the same style become one span.
    let mut spans = Vec::new();
    let mut run = String::new();
    let mut run_style = styles[skip];
    for (ch, style) in text.chars().skip(skip).zip(&styles[skip..end]) {
        if *style != run_style {
            spans.push(Span::styled(mem::take(&mut run), run_style));
            run_style = *style;
        }
        run.push(ch);
    }
    spans.push(Span::styled(run, run_style));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The row as (text, style) runs, to compare against.
    fn runs(line: &Line) -> Vec<(String, Style)> {
        line.spans
            .iter()
            .map(|span| (span.content.to_string(), span.style))
            .collect()
    }

    fn theme(toml: &str) -> Theme {
        Theme::from_config(&Config::parse(toml).unwrap()).unwrap()
    }

    const ONLY_MINE: &str = "[colors]\ndefaults = false\n";

    #[test]
    fn colors_just_the_matched_text() {
        let theme = theme(&format!(
            "{ONLY_MINE}[[rule]]\npattern = 'ERROR'\nfg = \"red\"\nbold = true\n"
        ));
        let red = Style::new().fg(Color::Red).add_modifier(Modifier::BOLD);
        let line = render_row("a ERROR b", &theme, None, 0, 0, 100, None);
        assert_eq!(
            runs(&line),
            [
                ("a ".to_string(), Style::default()),
                ("ERROR".to_string(), red),
                (" b".to_string(), Style::default())
            ]
        );
    }

    #[test]
    fn later_rules_override_and_scope_line_colors_everything() {
        let theme = theme(&format!(
            "{ONLY_MINE}\
             [[rule]]\npattern = 'x'\nfg = \"red\"\n\
             [[rule]]\npattern = 'x'\nfg = \"blue\"\n\
             [[rule]]\npattern = '^\\s+at '\nscope = \"line\"\ndim = true\n"
        ));
        let line = render_row("axb", &theme, None, 0, 0, 100, None);
        assert_eq!(
            line.spans[1].style,
            Style::new().fg(Color::Blue),
            "the later rule wins"
        );

        // `dim` only adds the attribute: the color from earlier rules stays.
        let line = render_row("  at x", &theme, None, 0, 0, 100, None);
        assert!(
            line.spans
                .iter()
                .all(|s| s.style.add_modifier.contains(Modifier::DIM))
        );
        assert_eq!(line.spans.last().unwrap().style.fg, Some(Color::Blue));
    }

    #[test]
    fn highlight_paints_over_the_colors() {
        let theme = theme(&format!(
            "{ONLY_MINE}[[rule]]\npattern = 'ERROR'\nfg = \"red\"\n"
        ));
        let highlight = Highlight::literal("rr");
        let line = render_row("ERROR", &theme, Some(&highlight), 0, 0, 100, None);
        assert_eq!(line.spans.len(), 3); // "E" red, "RR" highlighted, "OR" red
        assert_eq!(line.spans[1].content, "RR"); // smart case: all-lowercase search ignores case
        assert_eq!(line.spans[1].style.bg, Some(Color::Yellow));
        assert_eq!(line.spans[0].style.fg, Some(Color::Red));
    }

    #[test]
    fn scrolled_window_is_cut_after_painting() {
        let theme = theme(&format!(
            "{ONLY_MINE}[[rule]]\npattern = 'ERROR'\nfg = \"red\"\n"
        ));
        let line = render_row("ab ERROR cd", &theme, None, 0, 5, 4, None);
        assert_eq!(line.spans[0].content, "ROR"); // the cut-off start of a match keeps its color
        assert_eq!(line.spans[0].style.fg, Some(Color::Red));
        assert!(
            render_row("abc", &theme, None, 0, 10, 4, None)
                .spans
                .is_empty()
        );
    }

    #[test]
    fn selected_text_is_shown_in_reverse_video_over_the_colors() {
        let theme = theme(&format!(
            "{ONLY_MINE}[[rule]]\npattern = 'ERROR'\nfg = \"red\"\n"
        ));
        let line = render_row("ab ERROR cd", &theme, None, 0, 0, 100, Some(1..5));
        let reversed = |span: &Span| span.style.add_modifier.contains(Modifier::REVERSED);
        let marked: String = line
            .spans
            .iter()
            .filter(|span| reversed(span))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(marked, "b ER");
        // The color of a selected word stays, so it can be read through the selection.
        let selected_er = line
            .spans
            .iter()
            .find(|span| span.content.contains("ER"))
            .unwrap();
        assert_eq!(selected_er.style.fg, Some(Color::Red));
        // A range past the end of the row is harmless.
        assert!(
            render_row("abc", &theme, None, 0, 0, 100, Some(5..usize::MAX))
                .spans
                .len()
                == 1
        );
    }

    #[test]
    fn multibyte_text_is_colored_per_character() {
        let theme = theme(&format!(
            "{ONLY_MINE}[[rule]]\npattern = 'ö+'\nfg = \"green\"\n"
        ));
        let line = render_row("äööx", &theme, None, 0, 0, 100, None);
        assert_eq!(line.spans[1].content, "öö");
        assert_eq!(line.spans[1].style.fg, Some(Color::Green));
    }

    #[test]
    fn bad_configuration_names_the_rule() {
        let config = Config::parse("[[rule]]\nname = \"oops\"\npattern = '('\n").unwrap();
        assert!(
            Theme::from_config(&config)
                .err()
                .unwrap()
                .to_string()
                .contains("rule \"oops\"")
        );
        let config =
            Config::parse("[[rule]]\nname = \"pink\"\npattern = 'x'\nfg = \"nope\"\n").unwrap();
        let err = Theme::from_config(&config).err().unwrap().to_string();
        assert!(err.contains("rule \"pink\"") && err.contains("unknown color"));
    }

    #[test]
    fn the_built_in_theme_colors_a_tomcat_line() {
        let theme = Theme::builtin();
        let line = render_row(
            "03-Oct-2026 08:16:51.209 SEVERE [http-nio-8080-exec-7] boom",
            &theme,
            None,
            0,
            0,
            200,
            None,
        );
        let styled: Vec<&str> = line
            .spans
            .iter()
            .filter(|s| s.style != Style::default())
            .map(|s| &*s.content)
            .collect();
        assert_eq!(
            styled,
            [
                "03-Oct-2026 08:16:51.209",
                "SEVERE",
                "[http-nio-8080-exec-7]"
            ]
        );
    }

    /// The colored parts of `text` under the built-in theme, as (text, foreground).
    fn colored(text: &str) -> Vec<(String, Color)> {
        let line = render_row(text, &Theme::builtin(), None, 0, 0, 300, None);
        line.spans
            .iter()
            .filter_map(|span| Some((span.content.to_string(), span.style.fg?)))
            .collect()
    }

    fn only(text: &str, color: Color) -> Vec<(String, Color)> {
        vec![(text.to_string(), color)]
    }

    #[test]
    fn a_named_group_narrows_what_is_painted() {
        let theme = theme(&format!(
            "{ONLY_MINE}[[rule]]\npattern = 'level=(?P<v>\\w+)'\nfg = \"red\"\n"
        ));
        let line = render_row("a level=error b", &theme, None, 0, 0, 100, None);
        let red: Vec<&str> = line
            .spans
            .iter()
            .filter(|s| s.style.fg.is_some())
            .map(|s| &*s.content)
            .collect();
        assert_eq!(red, ["error"], "the `level=` in front is context only");
    }

    #[test]
    fn levels_are_found_in_logfmt_json_and_nginx_in_any_case() {
        let severe = Color::Red;
        assert_eq!(colored("level=error msg=x"), only("error", severe));
        assert_eq!(
            colored(r#"{"level":"ERROR","msg":"x"}"#),
            only("ERROR", severe)
        );
        assert_eq!(colored(r#"{"level":50,"msg":"x"}"#), only("50", severe));
        assert_eq!(
            colored(r#"{"level":40,"msg":"x"}"#),
            only("40", Color::Yellow)
        );
        assert_eq!(colored("level=warn msg=x"), only("warn", Color::Yellow));
        assert_eq!(colored("level=info msg=x"), only("info", Color::Green));
        assert!(colored("x [crit] 1#1: boom").contains(&("crit".to_string(), severe)));
        assert_eq!(
            colored("E1003 08:14:00.1 controller.go:1] boom")[0],
            ("E".to_string(), severe)
        );
    }

    #[test]
    fn plain_words_that_only_look_like_levels_stay_uncolored() {
        assert!(colored("retrying after error_count=3 and an error in prose").is_empty());
        assert!(
            colored(r#"{"level":30,"msg":"error handling ready"}"#)
                .iter()
                .all(|(_, color)| *color == Color::Green)
        );
    }

    #[test]
    fn http_problems_are_colored_but_success_is_not() {
        let line = |code: u16| {
            format!(
                r#"10.0.0.1 - - [03/Oct/2026:08:14:00 +0000] "GET /x HTTP/1.1" {code} 512 "-" "curl""#
            )
        };
        assert!(
            colored(&line(200))
                .iter()
                .all(|(text, _)| !text.contains("200"))
        );
        assert!(colored(&line(503)).contains(&("503".to_string(), Color::Red)));
        assert!(colored(&line(404)).contains(&("404".to_string(), Color::Yellow)));
    }

    #[test]
    fn dim_and_blue_text_turns_light_on_the_marked_row() {
        let dim = Style::new().fg(Color::DarkGray);
        assert_eq!(Theme::on_marked(dim).fg, Some(Color::White));
        assert_eq!(
            Theme::on_marked(Style::new().fg(Color::Blue)).fg,
            Some(Color::LightCyan)
        );
        let red = Style::new().fg(Color::Red);
        assert_eq!(Theme::on_marked(red), red, "other colors are kept");
        assert_eq!(Theme::on_marked(Style::default()), Style::default());
    }

    #[test]
    fn colors_can_be_switched_off() {
        let theme = theme("[colors]\nenabled = false\n");
        let line = render_row("ERROR everywhere", &theme, None, 0, 0, 100, None);
        assert_eq!(line.spans.len(), 1);
    }
}
