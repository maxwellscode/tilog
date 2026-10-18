//! Randomized tests of everything that reads input tilog does not control: log lines (they can
//! hold anything), commands typed at the prompt, and session and config files (they can come
//! from other people). Thousands of random and mangled inputs each; none may panic.
//!
//! The inputs come from a seeded generator, so a failure can be repeated. For a longer run:
//! `TILOG_FUZZ_ROUNDS=1000000 TILOG_FUZZ_SEED=7 cargo test robustness`.

use std::io::Cursor;
use std::time::{Duration, Instant};

use crate::command;
use crate::config::Config;
use crate::filter::{Filter, FilterSpec};
use crate::group::GroupRule;
use crate::highlight::Highlight;
use crate::line::{CappedLine, strip_escapes};
use crate::merge::parse_offset;
use crate::session::Session;
use crate::spec::SourceSpec;
use crate::theme::{self, Theme};
use crate::timestamp;
use crate::when::When;

/// Pieces that make the parsers take their less common paths.
const PIECES: &[&str] = &[
    "\x1b[",
    "\x1b]",
    "\x07",
    "\x1b\\",
    "\x1b",
    "31m",
    "0m",
    "2026-10-18",
    "T",
    "Z",
    "08:16",
    ":",
    ".123",
    "+02:00",
    "-05",
    "ssh:",
    "docker:",
    "kube:",
    "k8s:",
    "cmd:",
    "-",
    "--",
    "/",
    "~/",
    "..",
    "\"",
    "'",
    "\\",
    "[",
    "]",
    "{",
    "}",
    "=",
    "\n",
    "\r\n",
    "\t",
    " ",
    "é",
    "🙂",
    "\u{200b}",
    "\u{202e}",
    "0",
    "9",
    "99999999999999999999",
    "ERROR",
    "WARN",
    "at ",
    "^",
    "$",
    "(",
    ")",
    "*",
    "+",
    "?",
    "|",
    "&",
    "!",
    "-a",
    "-r",
    "-i",
    "-v",
    "-f",
    "level=",
    "{\"level\":",
    "[[source]]",
    "sources",
    "tab",
    "path",
    "spec",
    "filters",
    "pattern",
    "true",
    "%",
    "ms",
    "s",
    "h",
];

/// A small, seeded random generator (xorshift): the same seed gives the same inputs.
struct Random(u64);

impl Random {
    fn from_env() -> (Self, usize) {
        let number = |name: &str, default: u64| {
            std::env::var(name)
                .ok()
                .and_then(|text| text.parse().ok())
                .unwrap_or(default)
        };
        let seed = number("TILOG_FUZZ_SEED", 0x5EED_u64).max(1);
        let rounds = usize::try_from(number("TILOG_FUZZ_ROUNDS", 3_000)).unwrap_or(3_000);
        (Self(seed), rounds)
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n.max(1) as u64).unwrap_or(0)
    }

    /// Random bytes and pieces, sometimes valid UTF-8 text, sometimes not.
    fn bytes(&mut self, max_parts: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for _ in 0..self.below(max_parts + 1) {
            match self.below(4) {
                0 => out.push(self.next() as u8),
                1 => out.extend((0..self.below(8)).map(|_| b'a' + self.below(26) as u8)),
                _ => out.extend_from_slice(PIECES[self.below(PIECES.len())].as_bytes()),
            }
        }
        out
    }

    fn text(&mut self, max_parts: usize) -> String {
        String::from_utf8_lossy(&self.bytes(max_parts)).into_owned()
    }
}

/// Runs `check` on `rounds` inputs and makes sure the whole run stays quick: a parser that is
/// slow on some input would be slow on a log full of such lines.
fn each(rounds: usize, check: impl FnMut()) {
    each_within(rounds, Duration::from_millis(5), check);
}

/// Like `each`, with a limit of `most` per input (in a debug build).
fn each_within(rounds: usize, most: Duration, mut check: impl FnMut()) {
    let start = Instant::now();
    for _ in 0..rounds {
        check();
    }
    let per_input = start.elapsed() / u32::try_from(rounds.max(1)).unwrap_or(u32::MAX);
    assert!(per_input < most, "{per_input:?} per input is too slow");
}

#[test]
fn log_lines_of_any_bytes_are_taken_in_cleanly() {
    let (mut random, rounds) = Random::from_env();
    each(rounds, || {
        let bytes = random.bytes(40);
        let clean = strip_escapes(&bytes);
        assert!(clean.len() <= bytes.len());

        let mut line = CappedLine::new();
        let mut input = Cursor::new(&bytes);
        while line.read_from(&mut input).unwrap() > 0 {
            let text = line.to_text();
            assert!(!text.contains('\n'), "one line is one line: {text:?}");
            line.clear();
        }
    });
}

#[test]
fn timestamps_and_times_of_any_text_parse_or_not_but_never_panic() {
    let (mut random, rounds) = Random::from_env();
    let now = timestamp::now_ms();
    each(rounds, || {
        let text = random.text(12);
        let _ = timestamp::parse(&text, now);
        if let Some(when) = When::parse(&text) {
            let _ = when.resolve(now);
        }
        let _ = parse_offset(&text);
    });
}

#[test]
fn any_command_line_parses_or_says_why() {
    let (mut random, rounds) = Random::from_env();
    let names = [
        "filter ",
        "highlight ",
        "goto ",
        "write ",
        "group ",
        "merge ",
        "offset ",
    ];
    each(rounds, || {
        let mut text = random.text(10);
        if random.below(2) == 0 {
            text.insert_str(0, names[random.below(names.len())]);
        }
        let _ = command::parse(&text);
        let _ = SourceSpec::parse(&text);
        let _ = GroupRule::parse(&text);
    });
}

#[test]
fn any_pattern_compiles_or_is_refused_and_matching_stays_quick() {
    let (mut random, rounds) = Random::from_env();
    each(rounds / 3, || {
        let pattern = random.text(6);
        let (regex, ignore_case) = (random.below(2) == 0, random.below(2) == 0);
        let line = random.text(30);
        if let Ok(highlight) = Highlight::new(&pattern, regex, ignore_case) {
            let _ = highlight.is_match(&line);
        }
        let spec = FilterSpec {
            pattern,
            regex,
            ignore_case,
            exclude: random.below(2) == 0,
        };
        let _ = Filter::with_spec(None, &spec);
    });
}

#[test]
fn any_row_is_drawn_at_any_width_and_scroll() {
    let (mut random, rounds) = Random::from_env();
    let theme = Theme::builtin();
    let highlight = Highlight::new("e", false, true).unwrap();
    each(rounds, || {
        let text = random.text(30);
        let chars = text.chars().count();
        let selected = (random.below(3) == 0).then(|| {
            let start = random.below(chars + 3);
            start..start + random.below(chars + 3)
        });
        let _ = theme::render_row(
            &text,
            &theme,
            (random.below(2) == 0).then_some(&highlight),
            random.below(12),
            random.below(chars + 20),
            random.below(300),
            selected,
        );
    });
}

#[test]
fn a_mangled_session_or_config_file_is_refused_not_a_crash() {
    let (mut random, rounds) = Random::from_env();
    let session = "tab = 1\nselected = 0\n\n[[sources]]\npath = \"app.log\"\n\n\
                   [[sources.filters]]\npattern = \"ERROR\"\nregex = false\nignore_case = true\n";
    let config = crate::config::DEFAULT_CONFIG;
    // Read once at the start, not for every line: a whole config with its color rules may take
    // longer than a line.
    each_within(rounds / 30, Duration::from_millis(50), || {
        for base in [session, config] {
            // The real file with random damage: a piece cut out, random text put in.
            let mut text = base.to_string();
            let at = random.below(text.len());
            let at = (0..=at)
                .rev()
                .find(|&i| text.is_char_boundary(i))
                .unwrap_or(0);
            let end = (at + random.below(40)).min(text.len());
            let end = (end..=text.len())
                .find(|&i| text.is_char_boundary(i))
                .unwrap_or(text.len());
            text.replace_range(at..end, &random.text(6));
            if let Ok(session) = toml::from_str::<Session>(&text) {
                for source in &session.sources {
                    let _ = SourceSpec::parse(&source.path);
                }
            }
            if let Ok(config) = Config::parse(&text) {
                let _ = Theme::from_config(&config);
            }
        }
    });
}
