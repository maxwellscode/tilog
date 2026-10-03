//! Which lines belong together. A stack trace is one log *entry* spread over many lines:
//!
//! ```text
//! 03-Oct-2026 08:16:51.209 SEVERE ... Payment failed        <- starts an entry
//!     java.net.SocketTimeoutException: Read timed out       <- continues it
//!         at com.example.GatewayClient.post(...)            <- continues it
//! 03-Oct-2026 08:16:51.233 INFO ... Retrying                <- starts the next one
//! ```
//!
//! The rule only answers one question per line: does this line start a new entry?

use anyhow::{Result, anyhow};
use regex::bytes::Regex;

#[derive(Clone, Debug, Default)]
pub enum GroupRule {
    /// Every line is its own entry (no grouping).
    Off,
    /// Indented lines and well-known trace markers continue the previous entry.
    #[default]
    Auto,
    /// A line starts a new entry if it matches this regex; all others continue the entry.
    Pattern { regex: Regex, source: String },
}

impl GroupRule {
    /// `off`, `auto`, or any other text, which is taken as the regex that starts an entry.
    pub fn parse(text: &str) -> Result<Self> {
        match text.trim() {
            "off" => Ok(Self::Off),
            "auto" => Ok(Self::Auto),
            pattern => {
                // Regex errors span several lines; the input box has room for one.
                let regex = Regex::new(pattern).map_err(|e| {
                    anyhow!(
                        "invalid regex: {}",
                        e.to_string().lines().last().unwrap_or("").trim()
                    )
                })?;
                Ok(Self::Pattern {
                    regex,
                    source: pattern.to_string(),
                })
            }
        }
    }

    /// The text that `parse` turns back into this rule (this is what sessions store).
    pub fn describe(&self) -> String {
        match self {
            Self::Off => "off".to_string(),
            Self::Auto => "auto".to_string(),
            Self::Pattern { source, .. } => source.clone(),
        }
    }

    pub fn starts_entry(&self, line: &[u8]) -> bool {
        match self {
            Self::Off => true,
            Self::Auto => !is_continuation(line),
            Self::Pattern { regex, .. } => regex.is_match(line),
        }
    }
}

/// Java, Python and friends: a blank line, an indented line, or one of the markers that stack
/// traces put at the start of a line, continues the previous entry. Everything else starts one,
/// which keeps formats without multi-line entries (nginx, syslog) working line by line.
fn is_continuation(line: &[u8]) -> bool {
    line.is_empty()
        || matches!(line.first(), Some(b' ' | b'\t'))
        || line.starts_with(b"Caused by:")
        || line.starts_with(b"Suppressed:")
        || line.starts_with(b"Traceback ")
        || line.starts_with(b"...")
        // Python, between the parts of a chained traceback.
        || line.starts_with(b"During handling of the above exception")
        || line.starts_with(b"The above exception was the direct cause")
        || names_an_exception(line)
}

/// `java.sql.SQLException: ...`, `django.db.utils.OperationalError: ...`, `ValueError: ...`:
/// the line that names the exception, which Java and Python print at the start of the line
/// (below the log line that reports it) instead of indenting it.
///
/// It must look like a class name (`Error` alone is just a word) and end in `Exception`,
/// `Error` or `Throwable`, followed by a colon or nothing.
fn names_an_exception(line: &[u8]) -> bool {
    let name_end = line
        .iter()
        .position(|&b| b == b':' || b == b' ')
        .unwrap_or(line.len());
    let name = &line[..name_end];
    let ends_like_an_exception = ["Exception", "Error", "Throwable"]
        .iter()
        .any(|suffix| name.len() > suffix.len() && name.ends_with(suffix.as_bytes()));
    let looks_like_a_class = name
        .iter()
        .all(|&b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'$' | b'_'));
    let nothing_else_on_the_name = name_end == line.len() || line[name_end] == b':';
    ends_like_an_exception && looks_like_a_class && nothing_else_on_the_name
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_recognizes_stack_trace_lines() {
        let auto = GroupRule::Auto;
        // Starts of entries.
        assert!(auto.starts_entry(b"03-Oct-2026 08:16:51.209 SEVERE Payment failed"));
        assert!(auto.starts_entry(b"Oct  3 08:14:02 host sshd[1]: accepted"));
        assert!(auto.starts_entry(b"10.0.0.1 - - GET / 200"));
        assert!(
            auto.starts_entry(b"Error: connection refused"),
            "a bare word is not a class name"
        );
        assert!(auto.starts_entry(b"ERROR something broke"));
        // Continuations.
        assert!(!auto.starts_entry(b"\tat com.example.GatewayClient.post(GatewayClient.java:88)"));
        assert!(!auto.starts_entry(b"    java.net.SocketTimeoutException: Read timed out"));
        assert!(!auto.starts_entry(b"Caused by: java.io.IOException: broken pipe"));
        assert!(!auto.starts_entry(b"Traceback (most recent call last):"));
        assert!(!auto.starts_entry(b"... 12 more"));
        // A blank line stays with the entry above it (Python separates chained tracebacks so).
        assert!(!auto.starts_entry(b""));
        assert!(
            !auto.starts_entry(
                b"The above exception was the direct cause of the following exception:"
            )
        );
        assert!(
            !auto.starts_entry(
                b"During handling of the above exception, another exception occurred:"
            )
        );
        // The line naming the exception: Java and Python don't indent it.
        assert!(
            !auto.starts_entry(
                b"java.sql.SQLTransientConnectionException: HikariPool-1 - timed out"
            )
        );
        assert!(!auto.starts_entry(b"java.lang.NullPointerException"));
        assert!(!auto.starts_entry(b"org.postgresql.util.PSQLException: FATAL: no slots"));
        assert!(!auto.starts_entry(b"django.db.utils.OperationalError: connection failed"));
        assert!(!auto.starts_entry(b"ValueError: invalid literal for int()"));
    }

    /// The defaults must fit the realistic logs in `examples/`: exactly the lines that begin
    /// with the log format's own prefix start an entry, and every trace line continues one.
    #[test]
    fn auto_fits_the_example_logs() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
        // (file, how a line that starts an entry begins)
        type BeginsAnEntry = fn(&str) -> bool;
        let cases: [(&str, BeginsAnEntry); 5] = [
            ("example_spring_boot.log", |line| line.starts_with("2026-")),
            ("example_postgres.log", |line| line.starts_with("2026-")),
            ("example_python_gunicorn.log", |line| {
                line.starts_with("[2026-")
            }),
            ("example_syslog.log", |line| line.starts_with("Oct ")),
            ("example_nginx_access.log", |line| {
                line.starts_with(|c: char| c.is_ascii_digit())
            }),
        ];
        for (file, begins_an_entry) in cases {
            let text = std::fs::read_to_string(dir.join(file)).unwrap();
            for (number, line) in text.lines().enumerate() {
                assert_eq!(
                    GroupRule::Auto.starts_entry(line.as_bytes()),
                    begins_an_entry(line),
                    "{file}:{}: {line:?}",
                    number + 1
                );
            }
        }
    }

    #[test]
    fn off_makes_every_line_an_entry() {
        assert!(GroupRule::Off.starts_entry(b"\tat anything"));
    }

    #[test]
    fn pattern_decides_by_regex() {
        let rule = GroupRule::parse(r"^\d{2}-\w{3}-\d{4}").unwrap();
        assert!(rule.starts_entry(b"03-Oct-2026 08:16:51 INFO x"));
        assert!(!rule.starts_entry(b"java.net.SocketTimeoutException"));
        assert!(!rule.starts_entry(b"Caused by: x"));
    }

    #[test]
    fn parse_and_describe_roundtrip() {
        assert_eq!(GroupRule::parse("off").unwrap().describe(), "off");
        assert_eq!(GroupRule::parse(" auto ").unwrap().describe(), "auto");
        let rule = GroupRule::parse(r"^\[\d+\]").unwrap();
        assert_eq!(
            GroupRule::parse(&rule.describe()).unwrap().describe(),
            r"^\[\d+\]"
        );
        assert!(GroupRule::parse("(").is_err());
    }
}
