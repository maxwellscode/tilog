use anyhow::{Result, anyhow};
use regex::bytes::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

/// What the user typed for one condition: the part of a filter that can be saved and rebuilt.
/// A compiled `Regex` can't be written to a file, but the text it was built from can.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FilterSpec {
    pub pattern: String,
    #[serde(default)]
    pub regex: bool,
    #[serde(default)]
    pub ignore_case: bool,
    /// Keep the entries that do *not* match (`&!text`, like `grep -v`).
    #[serde(default)]
    pub exclude: bool,
}

/// A condition a line must meet. A filter forked from another filter keeps the parent's
/// conditions and adds its own, so a line must satisfy *all* of them. A condition can also be
/// an exclusion: then an entry is kept only if *none* of its lines matches.
///
/// Matching works on raw bytes (`regex::bytes`): no UTF-8 validation or `String` allocation
/// per line, which matters when scanning gigabytes.
#[derive(Clone)]
pub struct Filter {
    parts: Vec<Part>,
}

// `Regex` is cheap to clone (it is reference-counted inside), so cloning a Filter to hand it
// to a scanner thread costs almost nothing.
#[derive(Clone)]
struct Part {
    regex: Regex,
    label: String,
    spec: FilterSpec,
}

impl Filter {
    /// A filter that keeps what matches `pattern` (tests build filters this way).
    #[cfg(test)]
    pub fn new(
        parent: Option<&Filter>,
        pattern: &str,
        is_regex: bool,
        ignore_case: bool,
    ) -> Result<Self> {
        let spec = FilterSpec {
            pattern: pattern.to_string(),
            regex: is_regex,
            ignore_case,
            exclude: false,
        };
        Self::with_spec(parent, &spec)
    }

    /// The filter of `parent` (if any) with the condition `spec` added. A plain-text pattern is
    /// escaped and compiled as a regex too: one code path, and the regex engine has a fast path
    /// for literal text.
    pub fn with_spec(parent: Option<&Filter>, spec: &FilterSpec) -> Result<Self> {
        let pattern = &spec.pattern;
        let source = if spec.regex {
            pattern.clone()
        } else {
            regex::escape(pattern)
        };
        let regex = RegexBuilder::new(&source)
            .case_insensitive(spec.ignore_case)
            .build()
            // Regex errors span several lines; the status bar has room for one.
            .map_err(|e| {
                anyhow!(
                    "invalid regex: {}",
                    e.to_string().lines().last().unwrap_or("")
                )
            })?;

        let label = format!(
            "{}{}{pattern}{}",
            if spec.exclude { "!" } else { "" },
            if spec.regex { "re:" } else { "" },
            if spec.ignore_case { " (i)" } else { "" },
        );

        let mut parts = parent.map(|p| p.parts.clone()).unwrap_or_default();
        parts.push(Part {
            regex,
            label,
            spec: spec.clone(),
        });
        Ok(Self { parts })
    }

    /// The conditions of this filter, parent's first. Feeding them to `from_specs` rebuilds it.
    pub fn specs(&self) -> Vec<FilterSpec> {
        self.parts.iter().map(|part| part.spec.clone()).collect()
    }

    pub fn from_specs(specs: &[FilterSpec]) -> Result<Self> {
        let mut filter: Option<Filter> = None;
        for spec in specs {
            filter = Some(Filter::with_spec(filter.as_ref(), spec)?);
        }
        filter.ok_or_else(|| anyhow!("a filter needs at least one condition"))
    }

    /// Starts matching a new entry (one or more lines) against this filter.
    pub fn start_entry(&self) -> EntryMatch {
        // An exclusion is met until a line matches it; an inclusion once one does.
        let initial: Vec<bool> = self.parts.iter().map(|part| part.spec.exclude).collect();
        EntryMatch {
            satisfied: initial.clone(),
            initial,
        }
    }

    pub fn label(&self) -> String {
        let labels: Vec<&str> = self.parts.iter().map(|p| p.label.as_str()).collect();
        labels.join(" + ")
    }
}

/// Progress of matching one entry. An entry matches once *every* condition of the filter has
/// been satisfied by *some* line of it, not necessarily the same line: for a stack trace,
/// `SEVERE` can be on the header line and `SocketTimeout` three lines further down.
///
/// Only a few booleans are kept, never the lines, so entries of any length cost nothing.
pub struct EntryMatch {
    satisfied: Vec<bool>,
    /// What `satisfied` starts as: `true` for the exclusions.
    initial: Vec<bool>,
}

impl EntryMatch {
    /// Shows `line`, part of the current entry, to the filter.
    pub fn feed(&mut self, filter: &Filter, line: &[u8]) {
        for (done, part) in self.satisfied.iter_mut().zip(&filter.parts) {
            if part.spec.exclude {
                // One matching line is enough to leave the whole entry out.
                if *done && part.regex.is_match(line) {
                    *done = false;
                }
            } else if !*done {
                *done = part.regex.is_match(line);
            }
        }
    }

    pub fn is_match(&self) -> bool {
        self.satisfied.iter().all(|&done| done)
    }

    /// Forget everything, ready for the next entry (reuses the allocation).
    pub fn clear(&mut self) {
        self.satisfied.copy_from_slice(&self.initial);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter(pattern: &str, is_regex: bool, ignore_case: bool) -> Filter {
        Filter::new(None, pattern, is_regex, ignore_case).unwrap()
    }

    /// Does this filter match an entry made of these lines?
    fn matches(filter: &Filter, lines: &[&[u8]]) -> bool {
        let mut entry = filter.start_entry();
        for line in lines {
            entry.feed(filter, line);
        }
        entry.is_match()
    }

    /// The common case: an entry of one line.
    fn matches_line(filter: &Filter, line: &[u8]) -> bool {
        matches(filter, &[line])
    }

    #[test]
    fn plain_text_is_literal() {
        let f = filter("a.b", false, false);
        assert!(matches_line(&f, b"xx a.b xx"));
        assert!(!matches_line(&f, b"xx axb xx"));
    }

    #[test]
    fn regex_and_ignore_case() {
        assert!(matches_line(
            &filter(r"order \d+", true, false),
            b"order 10453 failed"
        ));
        assert!(!matches_line(
            &filter("error", false, false),
            b"SEVERE ERROR"
        ));
        assert!(matches_line(&filter("error", false, true), b"SEVERE ERROR"));
    }

    #[test]
    fn forked_filter_requires_parent_and_child() {
        let parent = filter("SEVERE", false, false);
        let child = Filter::new(Some(&parent), "Payment", false, false).unwrap();
        assert!(matches_line(&child, b"SEVERE Payment failed"));
        assert!(!matches_line(&child, b"SEVERE Servlet failed"));
        assert!(!matches_line(&child, b"INFO Payment ok"));
        assert_eq!(child.label(), "SEVERE + Payment");
    }

    #[test]
    fn conditions_may_be_met_by_different_lines_of_an_entry() {
        let parent = filter("SEVERE", false, false);
        let child = Filter::new(Some(&parent), "SocketTimeout", false, false).unwrap();
        let trace: [&[u8]; 3] = [
            b"08:16 SEVERE Payment failed",
            b"\tjava.net.SocketTimeoutException: Read timed out",
            b"\t\tat com.example.GatewayClient.post(GatewayClient.java:88)",
        ];
        assert!(matches(&child, &trace));
        assert!(
            !matches(&child, &trace[..1]),
            "header alone lacks SocketTimeout"
        );
        assert!(
            !matches(&child, &trace[1..]),
            "the trace alone lacks SEVERE"
        );
    }

    #[test]
    fn entry_match_can_be_reused() {
        let f = filter("x", false, false);
        let mut entry = f.start_entry();
        entry.feed(&f, b"x");
        assert!(entry.is_match());
        entry.clear();
        assert!(!entry.is_match());
    }

    #[test]
    fn specs_rebuild_an_equivalent_filter() {
        let parent = filter("SEVERE", false, false);
        let child = Filter::new(Some(&parent), r"order \d+", true, true).unwrap();

        let rebuilt = Filter::from_specs(&child.specs()).unwrap();
        assert_eq!(rebuilt.label(), child.label());
        assert!(matches_line(&rebuilt, b"SEVERE order 12")); // child ignores case, parent does not
        assert!(!matches_line(&rebuilt, b"INFO ORDER 12"));
        assert!(Filter::from_specs(&[]).is_err());
    }

    #[test]
    fn invalid_regex_gives_a_one_line_error() {
        let err = Filter::new(None, "(", true, false)
            .err()
            .unwrap()
            .to_string();
        assert!(err.starts_with("invalid regex:"));
        assert!(!err.contains('\n'));
    }

    #[test]
    fn matches_invalid_utf8() {
        assert!(matches_line(&filter("ok", false, false), b"\xff\xfe ok"));
    }

    fn exclude(parent: Option<&Filter>, pattern: &str) -> Filter {
        let spec = FilterSpec {
            pattern: pattern.into(),
            regex: false,
            ignore_case: false,
            exclude: true,
        };
        Filter::with_spec(parent, &spec).unwrap()
    }

    #[test]
    fn an_exclusion_keeps_the_entries_without_the_text() {
        let f = exclude(None, "healthcheck");
        assert!(matches_line(&f, b"GET /orders 200"));
        assert!(!matches_line(&f, b"GET /healthcheck 200"));
        assert_eq!(f.label(), "!healthcheck");
        // One line of an entry is enough to leave all of it out.
        assert!(!matches(
            &f,
            &[b"ERROR boom", b"\tat healthcheck.Probe.run"]
        ));
        // An entry match can be reused after it was ruled out.
        let mut entry = f.start_entry();
        entry.feed(&f, b"healthcheck");
        assert!(!entry.is_match());
        entry.clear();
        assert!(
            entry.is_match(),
            "a fresh entry has nothing to leave out yet"
        );
    }

    #[test]
    fn an_exclusion_can_narrow_down_a_filter() {
        // ERROR, but not the known noise.
        let errors = filter("ERROR", false, false);
        let f = exclude(Some(&errors), "SocketTimeout");
        assert!(matches_line(&f, b"ERROR payment declined"));
        assert!(!matches_line(&f, b"ERROR SocketTimeoutException"));
        assert!(
            !matches_line(&f, b"INFO ok"),
            "the parent still has to match"
        );
        assert_eq!(f.label(), "ERROR + !SocketTimeout");
        // It survives being saved and loaded.
        assert!(Filter::from_specs(&f.specs()).unwrap().specs()[1].exclude);
    }
}
