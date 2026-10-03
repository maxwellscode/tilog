//! What counts as an error line, for the `[` and `]` keys that jump between them.
//!
//! It is the same idea as the built-in color rules: a severe level in any of the common
//! spellings (`ERROR`, `level=error`, `"level":50`, nginx's `[crit]`, klog's `E1003`), or an
//! HTTP 5xx status. Warnings are not included: they are too frequent to jump between.

use std::sync::LazyLock;

use crate::highlight::Highlight;

/// Uppercase words must match exactly (`error` in prose is not an error line), while a level
/// written as a field is matched in any case.
const PATTERN: &str = concat!(
    r"\b(?:ERROR|SEVERE|FATAL|CRITICAL|PANIC)\b",
    r#"|(?i:\b(?:level|lvl|severity)["']?\s*[=:]\s*["']?(?:fatal|crit(?:ical)?|panic|emerg|alert|err(?:or)?)\b)"#,
    r"|\[(?:emerg|alert|crit|error)\]",
    r#"|"level"\s*:\s*(?:50|60)\b"#,
    r"|^[EF]\d{4} ",
    r#"|"\s5\d\d\s"#,
);

static SEVERE: LazyLock<Highlight> =
    LazyLock::new(|| Highlight::new(PATTERN, true, false).expect("the severity pattern is valid"));

/// Matches the lines worth jumping to.
pub fn errors() -> &'static Highlight {
    &SEVERE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_errors_in_the_common_spellings() {
        for line in [
            "2026-10-03 08:16:51 ERROR [x] boom",
            "03-Oct-2026 08:16:51.209 SEVERE [http-nio-8080-exec-7] boom",
            "ts=1 level=error msg=boom",
            r#"{"level":"ERROR","msg":"boom"}"#,
            r#"{"level":50,"msg":"boom"}"#,
            "2026/10/03 08:14:00 [crit] 1#1: boom",
            "E1003 08:14:00.1 controller.go:1] boom",
            r#"10.0.0.1 - - [03/Oct/2026:08:14:00 +0000] "GET /x HTTP/1.1" 503 12"#,
        ] {
            assert!(errors().is_match(line), "{line}");
        }
    }

    #[test]
    fn ignores_warnings_success_and_the_word_in_prose() {
        for line in [
            "2026-10-03 08:16:51 WARN [x] slow",
            "ts=1 level=warn msg=slow",
            "2026-10-03 08:16:51 INFO there was an error in the past",
            r#"{"level":30,"msg":"ok"}"#,
            r#"10.0.0.1 - - [03/Oct/2026:08:14:00 +0000] "GET /x HTTP/1.1" 200 5000"#,
            "retrying with error_count=3",
        ] {
            assert!(!errors().is_match(line), "{line}");
        }
    }
}
