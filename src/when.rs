//! A moment typed by the user for `:goto`: `08:16:50`, `8:16`, `2026-10-03 08:16` or just a date.
//!
//! It is read in the clock of the log itself (log timestamps without a zone are taken as UTC,
//! see `timestamp`), and it does not have to match a line: `:goto` lands on the first line at or
//! after it.

use std::sync::LazyLock;

use regex::Regex;

use crate::timestamp;

const DAY_MS: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct When {
    /// Midnight of the given date, in milliseconds since 1970. `None`: the day of the log.
    day_start: Option<i64>,
    /// Milliseconds since midnight.
    time_of_day: i64,
}

static FORMAT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:(\d{4})-(\d{1,2})-(\d{1,2})(?:[T ]+|$))?(?:(\d{1,2}):(\d{2})(?::(\d{2}))?(?:[.,]\d+)?)?$")
        .expect("the time pattern is valid")
});

impl When {
    /// `None` if the text is not a time or a date.
    pub fn parse(text: &str) -> Option<Self> {
        let c = FORMAT.captures(text.trim())?;
        let number = |i: usize| {
            c.get(i)
                .map(|m| m.as_str().parse::<i64>().ok())
                .unwrap_or(Some(0))
        };
        let (has_date, has_time) = (c.get(1).is_some(), c.get(4).is_some());
        if !has_date && !has_time {
            return None;
        }
        let (hour, minute, second) = (number(4)?, number(5)?, number(6)?);
        if hour > 23 || minute > 59 || second > 59 {
            return None;
        }
        let day_start = if has_date {
            Some(timestamp::day_start(number(1)?, number(2)?, number(3)?)?)
        } else {
            None
        };
        Some(Self {
            day_start,
            time_of_day: ((hour * 60 + minute) * 60 + second) * 1000,
        })
    }

    /// The moment in milliseconds since 1970. Without a date, the most recent such time of day
    /// that is not after `newest`, the time of the newest line of the log.
    pub fn resolve(&self, newest: i64) -> i64 {
        match self.day_start {
            Some(day) => day + self.time_of_day,
            None => {
                let today = newest - newest.rem_euclid(DAY_MS) + self.time_of_day;
                if today > newest {
                    today - DAY_MS
                } else {
                    today
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(text: &str) -> i64 {
        timestamp::parse(text, 0).unwrap()
    }

    #[test]
    fn reads_times_dates_and_both() {
        let newest = ms("2026-10-03 08:20:00");
        let at = |text: &str| When::parse(text).unwrap().resolve(newest);
        assert_eq!(at("08:16:50"), ms("2026-10-03 08:16:50"));
        assert_eq!(at("8:16"), ms("2026-10-03 08:16:00"));
        assert_eq!(at("2026-10-01 23:59"), ms("2026-10-01 23:59:00"));
        assert_eq!(at("2026-10-01T07:00:01"), ms("2026-10-01 07:00:01"));
        assert_eq!(at("2026-10-02"), ms("2026-10-02 00:00:00"));
        assert_eq!(at("08:16:50.250"), ms("2026-10-03 08:16:50"));
    }

    #[test]
    fn a_time_later_than_the_newest_line_means_the_day_before() {
        let newest = ms("2026-10-03 00:10:00");
        assert_eq!(
            When::parse("23:50").unwrap().resolve(newest),
            ms("2026-10-02 23:50:00")
        );
    }

    #[test]
    fn rejects_what_is_not_a_time() {
        for text in [
            "",
            "abc",
            "42",
            "25:00",
            "08:61",
            "2026-13-01",
            "2026-10-03 abc",
            "08:16 UTC",
        ] {
            assert_eq!(When::parse(text), None, "{text:?}");
        }
    }
}
