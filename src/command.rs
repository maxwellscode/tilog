//! Commands: turning text like `goto 120` (typed after `:`) into a [`Command`] value.
//!
//! This module only *parses*. It knows nothing about the UI or the app state. Applying a
//! command is the job of `App::execute`. To add a command:
//!   1. add a variant to [`Command`],
//!   2. add an entry to [`SPECS`] (the compiler then forces you to handle it in `execute`).

use std::fmt;

use crate::merge;
use crate::when::When;

/// Everything the user can ask for. An `enum` is "one of these variants", and a variant may
/// carry data (`Goto` carries a line number). Java enums can't vary their data per constant;
/// Rust enums can.
#[derive(Debug, PartialEq)]
pub enum Command {
    Help,
    Quit,
    /// Follow the newest line: the focused tile, or with `all` every tile of every source.
    Follow {
        all: bool,
    },
    /// Stop following, where the view is: the focused tile, or with `all` every tile.
    Pause {
        all: bool,
    },
    Top,
    Clear,
    Goto(u64),
    /// Jump to a moment in the log.
    GotoTime(When),
    /// A variant can also have named fields, like a struct.
    Filter {
        pattern: String,
        regex: bool,
        ignore_case: bool,
        /// Open it on every source, not just the current one.
        all: bool,
    },
    Close,
    /// `None` shows the current rule; otherwise `auto`, `off`, or a regex (see `GroupRule`).
    Group(Option<String>),
    /// `pattern: None` clears the highlight.
    Highlight {
        pattern: Option<String>,
        regex: bool,
        ignore_case: bool,
    },
    Reload,
    /// Interleave sources by timestamp. Each word is a tab number or a source name; none means
    /// all of them.
    Merge(Vec<String>),
    /// Milliseconds added to the timestamps of the current source.
    Offset(i64),
    Add(String),
    Overview,
    /// `None` saves under the name of the session that is currently loaded.
    Save(Option<String>),
    Load(String),
    Sessions,
    /// Save the focused tile's lines to a file. `force` replaces an existing file.
    Write {
        path: String,
        force: bool,
    },
}

#[derive(Debug, PartialEq)]
pub enum ParseError {
    Unknown(String),
    /// Right command, wrong arguments. Carries the usage string to show.
    Usage(&'static str),
}

// Implementing the `Display` trait is how a type says "this is how I print for humans".
// It is the Rust equivalent of overriding `toString()`.
impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unknown(name) => write!(f, "unknown command: {name} (try :help)"),
            Self::Usage(usage) => write!(f, "usage: {usage}"),
        }
    }
}

impl std::error::Error for ParseError {}

/// Describes one command. The single source of truth: parsing, `/help` and (later)
/// autocompletion all read this table.
pub struct Spec {
    /// First entry is the canonical name, the rest are aliases.
    names: &'static [&'static str],
    pub usage: &'static str,
    pub help: &'static str,
    /// Builds the command from the arguments, or returns `None` if they are invalid.
    /// A `fn` pointer: a plain function or a closure that captures nothing.
    build: fn(&[&str]) -> Option<Command>,
}

impl Spec {
    /// The canonical name, without the slash.
    pub fn name(&self) -> &'static str {
        self.names[0]
    }

    /// Whether the command accepts arguments: completing it should leave a space after the name.
    /// `<x>` is required, `[x]` optional.
    pub fn takes_args(&self) -> bool {
        self.usage.contains(['<', '['])
    }

    /// Whether the command can't run without arguments: picking it from the menu should wait
    /// for them instead of running it right away.
    pub fn requires_args(&self) -> bool {
        self.usage.contains('<')
    }
}

pub static SPECS: &[Spec] = &[
    Spec {
        names: &["help", "h", "?"],
        usage: ":help",
        help: "Show this help",
        build: |args| args.is_empty().then_some(Command::Help),
    },
    Spec {
        names: &["add", "a"],
        usage: ":add <path>",
        help: "Add a source: file, ssh:host:/path, docker:name, kube:pod, cmd:..., or a named one",
        // Rejoined with single spaces, so a path with spaces still works.
        build: |args| (!args.is_empty()).then(|| Command::Add(args.join(" "))),
    },
    Spec {
        names: &["overview", "o"],
        usage: ":overview",
        help: "Back to the overview of all sources (Esc in a tab)",
        build: |args| args.is_empty().then_some(Command::Overview),
    },
    Spec {
        names: &["filter"],
        usage: ":filter [-a] [-r] [-i] <text>",
        help: "New tile with the lines matching text (-a on every source, -r regex, -i ignore case)",
        build: |args| {
            let (flags, rest) = split_flags(args);
            // Words were split on whitespace, so runs of spaces in the pattern collapse to one.
            (!rest.is_empty()).then(|| Command::Filter {
                pattern: rest.join(" "),
                regex: flags.regex,
                ignore_case: flags.ignore_case,
                all: flags.all,
            })
        },
    },
    Spec {
        names: &["highlight", "hl"],
        usage: ":highlight [-r] [-i] [text]",
        help: "Mark text in every window, like / search with flags; no text clears",
        build: |args| {
            let (flags, rest) = split_flags(args);
            let pattern = (!rest.is_empty()).then(|| rest.join(" "));
            // Every window is highlighted anyway, so `-a` has nothing to add.
            (!flags.all).then_some(Command::Highlight {
                pattern,
                regex: flags.regex,
                ignore_case: flags.ignore_case,
            })
        },
    },
    Spec {
        names: &["group"],
        usage: ":group [auto|off|regex]",
        help: "How lines form one entry: indented ones continue (auto), none (off), or a regex that starts one",
        build: |args| Some(Command::Group((!args.is_empty()).then(|| args.join(" ")))),
    },
    Spec {
        names: &["merge"],
        usage: ":merge [source...]",
        help: "One tab with sources interleaved by timestamp (tab numbers like 1 2, or names; default all)",
        build: |args| {
            Some(Command::Merge(
                args.iter().map(|word| (*word).to_string()).collect(),
            ))
        },
    },
    Spec {
        names: &["offset"],
        usage: ":offset <+2h|-30m|+90s>",
        help: "Add a time shift to this source's timestamps, then :merge again (clock skew, zones)",
        build: |args| match args {
            [amount] => merge::parse_offset(amount).ok().map(Command::Offset),
            _ => None,
        },
    },
    Spec {
        names: &["write", "w"],
        usage: ":write [-f] <file>",
        help: "Save the focused tile's lines to a file (a filter writes all its matches; -f replaces a file)",
        // Rejoined with single spaces, so a path with spaces still works.
        build: |args| {
            let (force, rest) = match args {
                ["-f", rest @ ..] => (true, rest),
                rest => (false, rest),
            };
            (!rest.is_empty()).then(|| Command::Write {
                path: rest.join(" "),
                force,
            })
        },
    },
    Spec {
        names: &["close"],
        usage: ":close",
        help: "Close the focused tile (a main tile or the overview selection closes its source)",
        build: |args| args.is_empty().then_some(Command::Close),
    },
    Spec {
        names: &["save"],
        usage: ":save [name]",
        help: "Save sources, filters and tab as a session (name optional once loaded)",
        build: |args| match args {
            [] => Some(Command::Save(None)),
            [name] => Some(Command::Save(Some((*name).to_string()))),
            _ => None,
        },
    },
    Spec {
        names: &["load"],
        usage: ":load <name>",
        help: "Replace everything with a saved session",
        build: |args| match args {
            [name] => Some(Command::Load((*name).to_string())),
            _ => None,
        },
    },
    Spec {
        names: &["sessions"],
        usage: ":sessions",
        help: "List saved sessions",
        build: |args| args.is_empty().then_some(Command::Sessions),
    },
    Spec {
        names: &["reload"],
        usage: ":reload",
        help: "Reload config.toml (colors) without restarting",
        build: |args| args.is_empty().then_some(Command::Reload),
    },
    Spec {
        names: &["follow", "f"],
        usage: ":follow [all]",
        help: "Jump to the newest line and follow: the focused tile, or every tile with all",
        build: |args| match args {
            [] => Some(Command::Follow { all: false }),
            ["all"] => Some(Command::Follow { all: true }),
            _ => None,
        },
    },
    Spec {
        names: &["pause", "p"],
        usage: ":pause [all]",
        help: "Stop following where you are: the focused tile, or every tile with all",
        build: |args| match args {
            [] => Some(Command::Pause { all: false }),
            ["all"] => Some(Command::Pause { all: true }),
            _ => None,
        },
    },
    Spec {
        names: &["top"],
        usage: ":top",
        help: "Jump to the first line (focused tile)",
        build: |args| args.is_empty().then_some(Command::Top),
    },
    Spec {
        names: &["goto", "g"],
        usage: ":goto <line | time>",
        help: "Jump to a line number, or to a time like 08:16 or 2026-10-03 08:16:50 (focused tile)",
        // Slice pattern: matches a slice with exactly one element and names it `n`.
        build: |args| match args {
            [n] if n.parse::<u64>().is_ok() => n.parse().ok().map(Command::Goto),
            [] => None,
            _ => When::parse(&args.join(" ")).map(Command::GotoTime),
        },
    },
    Spec {
        names: &["clear"],
        usage: ":clear",
        help: "In a source tab: close all filter tiles. On the overview: empty the selected view",
        build: |args| args.is_empty().then_some(Command::Clear),
    },
    Spec {
        names: &["quit", "q", "exit"],
        usage: ":quit",
        help: "Exit tilog",
        build: |args| args.is_empty().then_some(Command::Quit),
    },
];

/// Peels leading `-r` (regex) and `-i` (ignore case) flags off the arguments.
/// Returns the flags and what is left: the text.
///
/// `[flag, tail @ ..]` is a slice pattern: it peels the first element off and names the rest.
fn split_flags<'a, 'b>(args: &'a [&'b str]) -> (Flags, &'a [&'b str]) {
    let mut flags = Flags::default();
    let mut rest = args;
    while let [flag, tail @ ..] = rest {
        match *flag {
            "-r" => flags.regex = true,
            "-i" => flags.ignore_case = true,
            "-a" => flags.all = true,
            _ => break,
        }
        rest = tail;
    }
    (flags, rest)
}

/// The `-r -i -a` flags in front of a pattern.
#[derive(Default)]
struct Flags {
    regex: bool,
    ignore_case: bool,
    all: bool,
}

/// Width of the column that lists the usages: the longest one and two spaces, so that every
/// description starts at the same column whatever commands exist.
pub fn usage_column_width() -> usize {
    SPECS
        .iter()
        .map(|spec| spec.usage.chars().count())
        .max()
        .unwrap_or(0)
        + 2
}

/// The commands whose name or alias starts with `prefix` (without the slash), in table order.
///
/// The `'static` lifetime says the returned references point into `SPECS`, which lives for the
/// whole program. They don't borrow from `prefix`, so the caller may drop `prefix` right away.
pub fn matching(prefix: &str) -> Vec<&'static Spec> {
    let prefix = prefix.to_lowercase();
    SPECS
        .iter()
        .filter(|spec| spec.names.iter().any(|name| name.starts_with(&prefix)))
        .collect()
}

/// Parses a command line, as typed after the `:` prompt. A leading `:` or `/` is tolerated.
pub fn parse(input: &str) -> Result<Command, ParseError> {
    let input = input.trim();
    let body = input.strip_prefix([':', '/']).unwrap_or(input);

    let mut words = body.split_whitespace();
    let name = words.next().unwrap_or("").to_lowercase();
    let args: Vec<&str> = words.collect();

    let spec = SPECS
        .iter()
        .find(|spec| spec.names.contains(&name.as_str()))
        .ok_or_else(|| ParseError::Unknown(name.clone()))?;

    (spec.build)(&args).ok_or(ParseError::Usage(spec.usage))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_commands_and_aliases() {
        assert_eq!(parse("/help"), Ok(Command::Help));
        assert_eq!(parse("  /Q  "), Ok(Command::Quit));
        assert_eq!(parse("/goto 42"), Ok(Command::Goto(42)));
        assert_eq!(parse("/g   7"), Ok(Command::Goto(7)));
    }

    #[test]
    fn matching_narrows_down_by_prefix_and_alias() {
        let names = |prefix| -> Vec<&str> { matching(prefix).iter().map(|s| s.name()).collect() };
        assert_eq!(
            names(""),
            [
                "help",
                "add",
                "overview",
                "filter",
                "highlight",
                "group",
                "merge",
                "offset",
                "write",
                "close",
                "save",
                "load",
                "sessions",
                "reload",
                "follow",
                "pause",
                "top",
                "goto",
                "clear",
                "quit"
            ]
        );
        assert_eq!(names("f"), ["filter", "follow"]);
        assert_eq!(names("s"), ["save", "sessions"]);
        assert_eq!(names("g"), ["group", "goto"]);
        assert_eq!(names("h"), ["help", "highlight"]);
        assert_eq!(names("hl"), ["highlight"]);
        assert_eq!(names("o"), ["overview", "offset"]);
        assert_eq!(names("m"), ["merge"]);
        assert_eq!(names("Q"), ["quit"]); // case-insensitive, matches the alias "q"
        assert_eq!(names("?"), ["help"]);
        assert!(names("zz").is_empty());
    }

    #[test]
    fn filter_and_pause_take_the_all_option() {
        assert_eq!(
            parse("/filter -a -i boom"),
            Ok(Command::Filter {
                pattern: "boom".into(),
                regex: false,
                ignore_case: true,
                all: true
            })
        );
        assert_eq!(parse("/pause"), Ok(Command::Pause { all: false }));
        assert_eq!(parse("/pause all"), Ok(Command::Pause { all: true }));
        assert_eq!(parse("/pause x"), Err(ParseError::Usage(":pause [all]")));
        // Every window is highlighted already, so the flag makes no sense there.
        assert!(parse("/highlight -a x").is_err());
    }

    #[test]
    fn goto_takes_a_line_number_or_a_time() {
        assert_eq!(parse("/goto 120"), Ok(Command::Goto(120)));
        assert_eq!(
            parse("/goto 08:16"),
            Ok(Command::GotoTime(When::parse("08:16").unwrap()))
        );
        assert_eq!(
            parse("/goto 2026-10-03 08:16:50"),
            Ok(Command::GotoTime(
                When::parse("2026-10-03 08:16:50").unwrap()
            ))
        );
        assert!(parse("/goto 25:99").is_err());
        assert!(parse("/goto").is_err());
    }

    #[test]
    fn write_takes_a_path_and_an_optional_force() {
        let write = |path: &str, force| {
            Ok(Command::Write {
                path: path.into(),
                force,
            })
        };
        assert_eq!(parse("/write out.log"), write("out.log", false));
        assert_eq!(
            parse("/w -f ~/my dir/out.log"),
            write("~/my dir/out.log", true)
        );
        assert_eq!(
            parse("/write"),
            Err(ParseError::Usage(":write [-f] <file>"))
        );
        assert_eq!(
            parse("/write -f"),
            Err(ParseError::Usage(":write [-f] <file>"))
        );
    }

    #[test]
    fn follow_takes_an_optional_all() {
        assert_eq!(parse("/follow"), Ok(Command::Follow { all: false }));
        assert_eq!(parse("/follow all"), Ok(Command::Follow { all: true }));
        assert_eq!(
            parse("/follow some"),
            Err(ParseError::Usage(":follow [all]"))
        );
    }

    #[test]
    fn parses_session_commands() {
        assert_eq!(parse("/save"), Ok(Command::Save(None)));
        assert_eq!(parse("/save work"), Ok(Command::Save(Some("work".into()))));
        assert_eq!(parse("/save a b"), Err(ParseError::Usage(":save [name]")));
        assert_eq!(parse("/load work"), Ok(Command::Load("work".into())));
        assert_eq!(parse("/load"), Err(ParseError::Usage(":load <name>")));
        assert_eq!(parse("/sessions"), Ok(Command::Sessions));
        // `takes_args` covers both required `<x>` and optional `[x]` arguments.
        let spec = |name: &str| SPECS.iter().find(|s| s.name() == name).unwrap();
        assert!(
            spec("save").takes_args() && spec("load").takes_args() && !spec("quit").takes_args()
        );
        // Only required arguments make the menu wait; `/save` and `/group` run as they are.
        assert!(spec("load").requires_args() && spec("filter").requires_args());
        assert!(!spec("save").requires_args() && !spec("group").requires_args());
    }

    #[test]
    fn parses_highlight_and_reload() {
        let highlight = |pattern: Option<&str>, regex, ignore_case| {
            Ok(Command::Highlight {
                pattern: pattern.map(String::from),
                regex,
                ignore_case,
            })
        };
        assert_eq!(parse("/highlight"), highlight(None, false, false));
        assert_eq!(
            parse("/hl timeout"),
            highlight(Some("timeout"), false, false)
        );
        assert_eq!(
            parse("/highlight -r -i a|b"),
            highlight(Some("a|b"), true, true)
        );
        assert_eq!(parse("/reload"), Ok(Command::Reload));
        assert_eq!(parse("/reload now"), Err(ParseError::Usage(":reload")));
    }

    #[test]
    fn parses_merge_and_offset() {
        assert_eq!(parse("merge"), Ok(Command::Merge(vec![])));
        assert_eq!(
            parse("merge 1 api"),
            Ok(Command::Merge(vec!["1".into(), "api".into()]))
        );
        assert_eq!(parse("offset +2h"), Ok(Command::Offset(7_200_000)));
        assert_eq!(parse("offset -90s"), Ok(Command::Offset(-90_000)));
        assert!(matches!(parse("offset"), Err(ParseError::Usage(_))));
        assert!(matches!(parse("offset soon"), Err(ParseError::Usage(_))));
        // `:o` is still the alias of :overview.
        assert_eq!(parse("o"), Ok(Command::Overview));
    }

    #[test]
    fn parses_group() {
        assert_eq!(parse("/group"), Ok(Command::Group(None)));
        assert_eq!(parse("/group off"), Ok(Command::Group(Some("off".into()))));
        assert_eq!(
            parse(r"/group ^\d{4}-\d{2} "),
            Ok(Command::Group(Some(r"^\d{4}-\d{2}".into())))
        );
        // `/g` stays the alias of /goto.
        assert_eq!(parse("/g 7"), Ok(Command::Goto(7)));
    }

    #[test]
    fn parses_add_and_overview() {
        assert_eq!(
            parse("/add logs/a.log"),
            Ok(Command::Add("logs/a.log".into()))
        );
        assert_eq!(
            parse("/add my logs/a b.log"),
            Ok(Command::Add("my logs/a b.log".into()))
        );
        assert_eq!(parse("/add"), Err(ParseError::Usage(":add <path>")));
        assert_eq!(parse("/o"), Ok(Command::Overview));
    }

    #[test]
    fn parses_filter_flags_and_pattern() {
        let filter = |pattern: &str, regex, ignore_case| {
            Ok(Command::Filter {
                pattern: pattern.into(),
                regex,
                ignore_case,
                all: false,
            })
        };
        assert_eq!(parse("/filter Error:"), filter("Error:", false, false));
        assert_eq!(
            parse("/filter -i -r err(or)?"),
            filter("err(or)?", true, true)
        );
        assert_eq!(
            parse("/filter Payment failed"),
            filter("Payment failed", false, false)
        );
        assert_eq!(
            parse("/filter"),
            Err(ParseError::Usage(":filter [-a] [-r] [-i] <text>"))
        );
        assert_eq!(
            parse("/filter -r"),
            Err(ParseError::Usage(":filter [-a] [-r] [-i] <text>"))
        );
    }

    #[test]
    fn reports_errors() {
        assert_eq!(parse("hello"), Err(ParseError::Unknown("hello".into())));
        assert_eq!(parse("/nope"), Err(ParseError::Unknown("nope".into())));
        assert_eq!(parse("/"), Err(ParseError::Unknown(String::new())));
        assert_eq!(
            parse("/goto"),
            Err(ParseError::Usage(":goto <line | time>"))
        );
        assert_eq!(
            parse("/goto abc"),
            Err(ParseError::Usage(":goto <line | time>"))
        );
        assert_eq!(parse("/quit now"), Err(ParseError::Usage(":quit")));
    }
}
