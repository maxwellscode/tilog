//! Finding the time a log line was written, so lines of different sources can be put in order.
//!
//! Recognized, near the start of the line:
//!
//! ```text
//! 2026-10-03T08:14:02.113Z          ISO 8601, also "2026-10-03 08:14:02,113" and "+02:00" zones
//! 03-Oct-2026 08:14:02.113          Tomcat / java.util.logging
//! [03/Oct/2026:08:14:02 +0200]      nginx and Apache access logs
//! Oct  3 08:14:02                   syslog (no year: the current one is assumed)
//! 2026/10/03 08:14:02               nginx error log (also the ISO form with slashes)
//! 1:M 03 Oct 2026 08:14:02.113      Redis
//! {"time":"2026-10-03T08:14:02Z"}   JSON lines, in a time / timestamp / @timestamp / ts field
//! {"time":1791015242113}            the same as a number: seconds, milliseconds (pino's default),
//!                                   microseconds or nanoseconds since 1970
//! ```
//!
//! The result is milliseconds since 1970 (UTC). A time without a zone is taken as it stands,
//! so two sources that write local time in different zones need `:offset` to line up.

use std::sync::LazyLock;

use regex::Regex;

/// How far into a line a timestamp may start. A timestamp deep inside a message is not the
/// time the line was written.
const SEARCH_CHARS: usize = 100;

struct Formats {
    iso: Regex,
    tomcat: Regex,
    clf: Regex,
    syslog: Regex,
    json: Regex,
    json_epoch: Regex,
}

// Compiled once, the first time a line is parsed.
static FORMATS: LazyLock<Formats> = LazyLock::new(|| {
    Formats {
    iso: Regex::new(
        r"^\W{0,3}(\d{4})[-/](\d{2})[-/](\d{2})[T ](\d{2}):(\d{2}):(\d{2})(?:[.,](\d{1,9}))?\s?(Z|[+-]\d{2}:?\d{2})?",
    )
    .expect("valid regex"),
    // Also Redis: "1:M 03 Oct 2026 08:14:02.113", with its "pid:role " in front.
    tomcat: Regex::new(
        r"^(?:\w+:\w )?\W{0,3}(\d{1,2})[- ]([A-Za-z]{3})[- ](\d{4}) (\d{2}):(\d{2}):(\d{2})(?:[.,](\d{1,9}))?",
    )
    .expect("valid regex"),
    clf: Regex::new(r"(\d{1,2})/([A-Za-z]{3})/(\d{4}):(\d{2}):(\d{2}):(\d{2}) ([+-]\d{4})")
        .expect("valid regex"),
    syslog: Regex::new(r"^([A-Za-z]{3}) +(\d{1,2}) (\d{2}):(\d{2}):(\d{2})").expect("valid regex"),
    json: Regex::new(
        r#""(?:@timestamp|timestamp|time|ts|datetime)"\s*:\s*"(\d{4})-(\d{2})-(\d{2})[T ](\d{2}):(\d{2}):(\d{2})(?:[.,](\d{1,9}))?(Z|[+-]\d{2}:?\d{2})?""#,
    )
    .expect("valid regex"),
    // 9 to 19 digits cover every unit from seconds to nanoseconds; a shorter number is a
    // counter or an id, not a time. The decimals are a fraction of a second.
    json_epoch: Regex::new(r#""(?:@timestamp|timestamp|time|ts|datetime)"\s*:\s*(\d{9,19})(?:\.(\d{1,9}))?\s*[,}]"#)
        .expect("valid regex"),
}
});

/// The current time in milliseconds since 1970.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(0))
}

/// The time of day, UTC, of a time in milliseconds since 1970: `08:14:02`.
pub fn utc_clock(ms: i64) -> String {
    let seconds_today = ms.div_euclid(1000).rem_euclid(86_400);
    format!(
        "{:02}:{:02}:{:02}",
        seconds_today / 3600,
        seconds_today % 3600 / 60,
        seconds_today % 60
    )
}

/// The time of `line` in milliseconds since 1970, if it starts with a recognized timestamp.
/// `now_ms` is the current time, used to guess the year of syslog lines.
pub fn parse(line: &str, now_ms: i64) -> Option<i64> {
    // Cut at a character boundary: `&line[..100]` could split a multi-byte character.
    let end = line
        .char_indices()
        .nth(SEARCH_CHARS)
        .map_or(line.len(), |(byte, _)| byte);
    let head = &line[..end];
    let formats = &*FORMATS;

    if let Some(c) = formats.iso.captures(head) {
        let zone = c.get(8).map_or(Some(0), |z| zone_ms(z.as_str()))?;
        return build(
            num(&c, 1)?,
            num(&c, 2)?,
            num(&c, 3)?,
            [num(&c, 4)?, num(&c, 5)?, num(&c, 6)?],
            fraction(&c, 7),
            zone,
        );
    }
    if let Some(c) = formats.tomcat.captures(head) {
        let month = month(c.get(2)?.as_str())?;
        return build(
            num(&c, 3)?,
            month,
            num(&c, 1)?,
            [num(&c, 4)?, num(&c, 5)?, num(&c, 6)?],
            fraction(&c, 7),
            0,
        );
    }
    if let Some(c) = formats.clf.captures(head) {
        let month = month(c.get(2)?.as_str())?;
        let zone = zone_ms(c.get(7)?.as_str())?;
        return build(
            num(&c, 3)?,
            month,
            num(&c, 1)?,
            [num(&c, 4)?, num(&c, 5)?, num(&c, 6)?],
            0,
            zone,
        );
    }
    if let Some(c) = formats.syslog.captures(head) {
        let month = month(c.get(1)?.as_str())?;
        let year = civil_year(now_ms);
        let time = [num(&c, 3)?, num(&c, 4)?, num(&c, 5)?];
        let this_year = build(year, month, num(&c, 2)?, time, 0, 0)?;
        // "Dec 31" read on January 1st belongs to last year.
        let day_ms = 86_400_000;
        return if this_year > now_ms + day_ms {
            build(year - 1, month, num(&c, 2)?, time, 0, 0)
        } else {
            Some(this_year)
        };
    }
    if let Some(c) = formats.json.captures(head) {
        let zone = c.get(8).map_or(Some(0), |z| zone_ms(z.as_str()))?;
        return build(
            num(&c, 1)?,
            num(&c, 2)?,
            num(&c, 3)?,
            [num(&c, 4)?, num(&c, 5)?, num(&c, 6)?],
            fraction(&c, 7),
            zone,
        );
    }
    if let Some(c) = formats.json_epoch.captures(head) {
        let number: i64 = c.get(1)?.as_str().parse().ok()?;
        return Some(epoch_to_ms(number, fraction(&c, 2)));
    }
    None
}

/// A time since 1970 in seconds, milliseconds, microseconds or nanoseconds, as milliseconds.
///
/// The unit follows from the size: as seconds, 10^11 is the year 5138, so anything below it
/// is seconds; as milliseconds, 10^14 is the same year, and so on. `fraction_ms` is the part
/// after a decimal point of a time in seconds (`1791015242.113`).
fn epoch_to_ms(number: i64, fraction_ms: i64) -> i64 {
    match number {
        ..100_000_000_000 => number * 1000 + fraction_ms,
        100_000_000_000..100_000_000_000_000 => number,
        100_000_000_000_000..100_000_000_000_000_000 => number / 1000,
        _ => number / 1_000_000,
    }
}

fn num(captures: &regex::Captures, group: usize) -> Option<i64> {
    captures.get(group)?.as_str().parse().ok()
}

/// A fraction of a second ("113", "5", "123456") as milliseconds.
fn fraction(captures: &regex::Captures, group: usize) -> i64 {
    let Some(digits) = captures.get(group) else {
        return 0;
    };
    let first_three: String = digits
        .as_str()
        .chars()
        .chain("00".chars())
        .take(3)
        .collect();
    first_three.parse().unwrap_or(0)
}

/// "Z", "+02:00", "+0200", "-0530" as milliseconds east of UTC.
fn zone_ms(text: &str) -> Option<i64> {
    if text == "Z" {
        return Some(0);
    }
    let sign = if text.starts_with('-') { -1 } else { 1 };
    let digits: String = text.chars().filter(char::is_ascii_digit).collect();
    let hours: i64 = digits.get(0..2)?.parse().ok()?;
    let minutes: i64 = digits.get(2..4)?.parse().ok()?;
    Some(sign * (hours * 60 + minutes) * 60_000)
}

fn month(name: &str) -> Option<i64> {
    const NAMES: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let position = NAMES.iter().position(|n| name.eq_ignore_ascii_case(n))?;
    Some(position as i64 + 1)
}

/// Milliseconds since 1970 for a calendar time, `zone` milliseconds east of UTC.
fn build(year: i64, month: i64, day: i64, hms: [i64; 3], millis: i64, zone: i64) -> Option<i64> {
    let [hour, minute, second] = hms;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    let seconds = ((days * 24 + hour) * 60 + minute) * 60 + second;
    Some(seconds * 1000 + millis - zone)
}

/// Midnight (UTC) at the start of a date, in milliseconds since 1970. `None` if it isn't a date.
pub fn day_start(year: i64, month: i64, day: i64) -> Option<i64> {
    build(year, month, day, [0, 0, 0], 0, 0)
}

/// Days since 1970-01-01 for a date in the proleptic Gregorian calendar (Howard Hinnant's
/// algorithm: years are counted from March, so the leap day is the last day of the year).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The calendar year of a time given in milliseconds since 1970.
fn civil_year(ms: i64) -> i64 {
    let days = ms.div_euclid(86_400_000) + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let year = year_of_era + era * 400;
    if month_index >= 10 { year + 1 } else { year }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::group::GroupRule;

    const NOW: i64 = 1_791_015_242_113; // 2026-10-03T08:14:02.113Z
    const AT_0814: i64 = 1_791_015_242_000; // 2026-10-03 08:14:02

    #[test]
    fn the_clock_shows_utc_time_of_day() {
        assert_eq!(utc_clock(NOW), "08:14:02");
        assert_eq!(utc_clock(0), "00:00:00");
        assert_eq!(utc_clock(86_399_999), "23:59:59");
        assert_eq!(
            utc_clock(86_400_000),
            "00:00:00",
            "midnight starts the next day"
        );
        assert_eq!(utc_clock(-1000), "23:59:59", "before 1970 still works");
        assert!(
            now_ms() > NOW,
            "the real clock is past the date of the examples"
        );
    }

    #[test]
    fn calendar_arithmetic() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(
            build(2000, 2, 29, [0, 0, 0], 0, 0),
            Some(951_782_400_000),
            "a leap day"
        );
        assert_eq!(
            build(1969, 12, 31, [23, 59, 59], 0, 0),
            Some(-1000),
            "before 1970"
        );
        assert_eq!(civil_year(NOW), 2026);
        assert_eq!(civil_year(0), 1970);
        assert_eq!(civil_year(951_782_400_000), 2000);
        assert_eq!(civil_year(1_704_067_199_000), 2023); // 2023-12-31T23:59:59Z
        assert_eq!(civil_year(1_704_067_200_000), 2024); // and one second later
    }

    #[test]
    fn iso_in_its_variants() {
        assert_eq!(parse("2026-10-03T08:14:02.113Z INFO x", NOW), Some(NOW));
        assert_eq!(parse("2026-10-03 08:14:02,113 INFO x", NOW), Some(NOW));
        assert_eq!(parse("2026-10-03 08:14:02 INFO x", NOW), Some(AT_0814));
        assert_eq!(parse("2026-10-03T10:14:02+02:00 x", NOW), Some(AT_0814));
        assert_eq!(parse("2026-10-03T03:44:02-0430 x", NOW), Some(AT_0814));
        assert_eq!(
            parse("[2026-10-03 08:14:02] x", NOW),
            Some(AT_0814),
            "in brackets"
        );
    }

    #[test]
    fn nginx_error_redis_json_and_kubernetes() {
        assert_eq!(
            parse(
                "2026/10/03 08:14:02 [error] 29#29: *1 upstream timed out",
                NOW
            ),
            Some(AT_0814)
        );
        assert_eq!(
            parse(
                "1:M 03 Oct 2026 08:14:02.113 * Ready to accept connections",
                NOW
            ),
            Some(NOW)
        );
        assert_eq!(
            parse("71:C 03 Oct 2026 08:14:02.113 * DB saved on disk", NOW),
            Some(NOW)
        );
        assert_eq!(
            parse("03 Oct 2026 08:14:02.113 * without a prefix", NOW),
            Some(NOW)
        );
        // `kubectl logs --timestamps`: nanoseconds.
        assert_eq!(
            parse("2026-10-03T08:14:02.113456789Z level=info msg=x", NOW),
            Some(NOW)
        );

        let json = r#"{"level":"info","time":"2026-10-03T08:14:02.113Z","msg":"hi"}"#;
        assert_eq!(parse(json, NOW), Some(NOW));
        assert_eq!(
            parse(
                r#"{"@timestamp":"2026-10-03T10:14:02+02:00","message":"x"}"#,
                NOW
            ),
            Some(AT_0814)
        );
        assert_eq!(
            parse(r#"{"ts":"2026-10-03 08:14:02","a":1}"#, NOW),
            Some(AT_0814)
        );
        // A date in some other field is not the time of the line.
        assert_eq!(
            parse(r#"{"msg":"x","expires":"2026-10-03T08:14:02Z"}"#, NOW),
            None
        );
    }

    #[test]
    fn json_with_a_numeric_time() {
        // The same instant in each unit.
        let seconds = r#"{"ts":1791015242,"msg":"x"}"#;
        assert_eq!(parse(seconds, NOW), Some(AT_0814));
        assert_eq!(parse(r#"{"ts":1791015242.113,"msg":"x"}"#, NOW), Some(NOW));
        assert_eq!(
            parse(r#"{"level":30,"time":1791015242113,"pid":1}"#, NOW),
            Some(NOW),
            "pino"
        );
        assert_eq!(
            parse(r#"{"time":1791015242113456,"a":1}"#, NOW),
            Some(NOW),
            "microseconds"
        );
        assert_eq!(
            parse(r#"{"timestamp":1791015242113456789}"#, NOW),
            Some(NOW),
            "nanoseconds"
        );
        assert_eq!(
            parse(r#"{"@timestamp": 1791015242113 , "a":1}"#, NOW),
            Some(NOW),
            "spaces"
        );

        // Numbers that are not a time: too short, another key, or a string that isn't a date.
        assert_eq!(parse(r#"{"time":42,"msg":"x"}"#, NOW), None);
        assert_eq!(parse(r#"{"count":1791015242113,"msg":"x"}"#, NOW), None);
        assert_eq!(parse(r#"{"time":"yesterday","msg":"x"}"#, NOW), None);
    }

    #[test]
    fn the_unit_follows_from_the_size() {
        assert_eq!(epoch_to_ms(1_791_015_242, 0), 1_791_015_242_000);
        assert_eq!(epoch_to_ms(1_791_015_242, 113), NOW);
        assert_eq!(epoch_to_ms(1_791_015_242_113, 0), NOW);
        assert_eq!(epoch_to_ms(1_791_015_242_113_456, 0), NOW);
        assert_eq!(epoch_to_ms(1_791_015_242_113_456_789, 0), NOW);
        assert_eq!(
            epoch_to_ms(999_999_999, 0),
            999_999_999_000,
            "September 2001, in seconds"
        );
    }

    #[test]
    fn fractions_of_a_second() {
        assert_eq!(parse("2026-10-03 08:14:02.5 x", NOW), Some(AT_0814 + 500));
        assert_eq!(
            parse("2026-10-03 08:14:02.123456789 x", NOW),
            Some(AT_0814 + 123)
        );
    }

    #[test]
    fn tomcat_access_log_and_syslog() {
        assert_eq!(
            parse("03-Oct-2026 08:14:02.113 INFO [main] x", NOW),
            Some(NOW)
        );
        assert_eq!(
            parse(
                r#"10.0.0.1 - - [03/Oct/2026:10:14:02 +0200] "GET / HTTP/1.1" 200"#,
                NOW
            ),
            Some(AT_0814)
        );
        assert_eq!(
            parse("Oct  3 08:14:02 host sshd[1]: accepted", NOW),
            Some(AT_0814)
        );
    }

    #[test]
    fn syslog_lines_from_late_december_belong_to_last_year() {
        let new_years_day = 1_767_312_000_000; // 2026-01-02T00:00:00Z
        let december = parse("Dec 31 23:00:00 host x", new_years_day).unwrap();
        assert!(december < new_years_day, "must not land in the future");
        assert_eq!(civil_year(december), 2025);
    }

    #[test]
    fn lines_without_a_timestamp() {
        assert_eq!(parse("\tat com.example.Foo.bar(Foo.java:1)", NOW), None);
        assert_eq!(parse("Caused by: java.io.IOException", NOW), None);
        assert_eq!(parse("", NOW), None);
        // Far into the line is a message, not the time the line was written.
        let late = format!("{}2026-10-03 08:14:02", "x".repeat(150));
        assert_eq!(parse(&late, NOW), None);
        // Impossible dates are not timestamps.
        assert_eq!(parse("2026-13-45 99:99:99 x", NOW), None);
    }

    /// The files in `examples/` are what people try tilog with; their lines must be recognized,
    /// or `:merge` would silently put a source out of order.
    #[test]
    fn every_example_log_has_recognized_timestamps() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples");
        let mut checked = 0;
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|ext| ext != "log") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            assert!(!lines.is_empty(), "{} is empty", path.display());

            // Every line that starts an entry has a time. The rest of an entry (a stack trace)
            // doesn't need one: it takes the time of the line above it.
            for (number, line) in lines.iter().enumerate() {
                if GroupRule::Auto.starts_entry(line.as_bytes()) {
                    assert!(
                        parse(line, NOW).is_some(),
                        "{}:{}: an entry starts here but has no timestamp: {line:?}",
                        path.display(),
                        number + 1
                    );
                }
            }
            checked += 1;
        }
        assert!(checked >= 10, "expected the example logs, found {checked}");
    }

    #[test]
    fn multibyte_text_does_not_break_the_cut() {
        let text = format!("{} 2026-10-03 08:14:02", "ä".repeat(120));
        assert_eq!(parse(&text, NOW), None);
    }
}
