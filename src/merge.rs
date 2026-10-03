//! Putting the lines of several sources into one timeline.
//!
//! Lines arrive at different moments: a line written at 08:00:01 on host A can reach tilog after
//! a line written at 08:00:02 on host B. So a line isn't shown at once. It waits until every
//! source that is still talking has reported a time at or after it (a classic multi-way merge),
//! and is then released in timestamp order. A source that falls silent doesn't hold everyone
//! back for more than a moment, no line waits longer than `MAX_HOLD`, and every line waits at
//! least `MIN_HOLD`, so lines that are written together but travel separately can catch up.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};

use crate::timestamp;

/// A source that has sent nothing for this long is not waited for.
const IDLE: Duration = Duration::from_secs(2);

/// Every line waits at least this long before it is shown. Without it, a line from a source that
/// has been quiet would be shown at once, and a line stamped just before it, arriving a moment
/// later over another connection, would land *behind* it.
const MIN_HOLD: Duration = Duration::from_millis(500);

/// No line is held back longer than this, whatever the sources do.
const MAX_HOLD: Duration = Duration::from_secs(3);

/// Longest source label shown in front of each line.
const MAX_LABEL: usize = 16;

/// One source taking part in the merge.
struct Member {
    /// Identifies the source across the app (`Source::id`).
    id: u64,
    label: String,
    /// Added to this source's timestamps, to correct for a clock that is off or a different zone.
    offset_ms: i64,
    /// The time of the newest line seen. Lines without a time (the rest of a stack trace) take it.
    last_key: Option<i64>,
    last_arrival: Instant,
}

/// A line waiting for its turn.
struct Pending {
    key: i64,
    /// Arrival order. Lines with the same time keep it.
    seq: u64,
    member: usize,
    text: String,
    arrived: Instant,
}

pub struct Merger {
    members: Vec<Member>,
    pending: Vec<Pending>,
    next_seq: u64,
    /// The current time, for guessing the year of syslog lines.
    now_ms: i64,
}

/// What a source contributes when a merge is created.
pub struct MemberInit {
    pub id: u64,
    pub label: String,
    pub offset_ms: i64,
    /// The lines already in memory, oldest first.
    pub lines: Vec<String>,
}

impl Merger {
    /// Starts a merge. Returns it together with the lines the sources already have, merged
    /// into time order (as `(member index, line)`).
    pub fn start(members: Vec<MemberInit>) -> (Self, Vec<(usize, String)>) {
        let now = Instant::now();
        let now_ms = timestamp::now_ms();

        let mut merger = Self {
            members: members
                .iter()
                .map(|m| Member {
                    id: m.id,
                    label: m.label.clone(),
                    offset_ms: m.offset_ms,
                    last_key: None,
                    last_arrival: now,
                })
                .collect(),
            pending: Vec::new(),
            next_seq: 0,
            now_ms,
        };

        // The existing lines are all there at once, so they can simply be sorted.
        for (index, init) in members.into_iter().enumerate() {
            for line in init.lines {
                merger.feed_at(index, &line, now);
            }
        }
        let rows = merger.drain_sorted(|_| true);
        (merger, rows)
    }

    pub fn label(&self, member: usize) -> &str {
        &self.members[member].label
    }

    pub fn labels(&self) -> Vec<&str> {
        self.members.iter().map(|m| m.label.as_str()).collect()
    }

    /// Width of the label column: the longest label, up to `MAX_LABEL`.
    pub fn label_width(&self) -> usize {
        self.members
            .iter()
            .map(|m| m.label.chars().count())
            .max()
            .unwrap_or(0)
            .min(MAX_LABEL)
    }

    /// A new line from the source `id`. Lines of sources that aren't part of the merge are
    /// ignored.
    pub fn feed(&mut self, id: u64, line: &str, now: Instant) {
        if let Some(index) = self.members.iter().position(|m| m.id == id) {
            self.feed_at(index, line, now);
        }
    }

    fn feed_at(&mut self, member: usize, line: &str, now: Instant) {
        let m = &mut self.members[member];
        let key = match timestamp::parse(line, self.now_ms) {
            Some(ms) => ms + m.offset_ms,
            // No time on this line: it belongs with the one before it. At the very start of
            // a source there is none yet, so it goes first.
            None => m.last_key.unwrap_or(i64::MIN),
        };
        // A line with an earlier time than the last one (rare) must not drag the source back.
        m.last_key = Some(m.last_key.map_or(key, |last| last.max(key)));
        m.last_arrival = now;

        self.pending.push(Pending {
            key,
            seq: self.next_seq,
            member,
            text: line.to_string(),
            arrived: now,
        });
        self.next_seq += 1;
    }

    /// The lines that may be shown now, in time order.
    pub fn release(&mut self, now: Instant) -> Vec<(usize, String)> {
        // Nothing earlier can still arrive from a source that is talking, once it has reported
        // a later time. So the line may go if it is not later than the *slowest* such source.
        let watermark = self
            .members
            .iter()
            .filter(|m| now.saturating_duration_since(m.last_arrival) < IDLE)
            .filter_map(|m| m.last_key)
            .min();

        self.drain_sorted(|line| {
            let age = now.saturating_duration_since(line.arrived);
            let turn_has_come = watermark.is_none_or(|mark| line.key <= mark);
            (age >= MIN_HOLD && turn_has_come) || age >= MAX_HOLD
        })
    }

    /// Removes the pending lines that satisfy `ready`, and returns them in time order.
    fn drain_sorted(&mut self, ready: impl Fn(&Pending) -> bool) -> Vec<(usize, String)> {
        let (mut out, keep): (Vec<Pending>, Vec<Pending>) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|line| ready(line));
        self.pending = keep;
        out.sort_by_key(|line| (line.key, line.seq));
        out.into_iter()
            .map(|line| (line.member, line.text))
            .collect()
    }
}

/// Short labels for the column in front of merged rows: `.log` is dropped, and so is whatever
/// all the names start with, up to the last separator (`example_nginx.log` and
/// `example_redis.log` become `nginx` and `redis`). Names that differ from the start are kept.
pub fn short_labels(names: &[&str]) -> Vec<String> {
    let trimmed: Vec<&str> = names
        .iter()
        .map(|name| name.strip_suffix(".log").unwrap_or(name))
        .collect();
    if trimmed.len() < 2 {
        return trimmed.iter().map(|name| (*name).to_string()).collect();
    }

    // The longest common start, cut back to just after a separator so words stay whole.
    let first = trimmed[0];
    let mut common = first.len();
    for name in &trimmed[1..] {
        let shared: usize = first
            .chars()
            .zip(name.chars())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len_utf8())
            .sum();
        common = common.min(shared);
    }
    let mut cut = first[..common]
        .rfind(['_', '-', '.', ':', '/'])
        .map_or(0, |at| at + 1);
    // What is left must still say something: `api-1` and `api-2` must not become `1` and `2`.
    if trimmed
        .iter()
        .any(|name| !name[cut..].chars().any(char::is_alphabetic))
    {
        cut = 0;
    }
    trimmed
        .iter()
        .map(|name| {
            let rest = &name[cut..];
            // Never reduce a name to nothing.
            if rest.is_empty() {
                (*name).to_string()
            } else {
                rest.to_string()
            }
        })
        .collect()
}

/// One merged row: the label in a fixed-width column, a bar, then the line. The label is cut or
/// padded to `width`, so every row has the same layout.
pub fn format_row(label: &str, width: usize, text: &str) -> String {
    let label: String = label.chars().take(width).collect();
    format!("{label:<width$} │ {text}")
}

/// Characters taken by everything in front of the text in a merged row.
pub fn row_prefix_len(width: usize) -> usize {
    width + 3 // the label column, a space, the bar, a space
}

/// Parses a clock offset such as `+2h`, `-30m`, `+90s`, `+500ms` or `0` into milliseconds.
pub fn parse_offset(text: &str) -> Result<i64> {
    let text = text.trim();
    if text == "0" {
        return Ok(0);
    }
    let (sign, rest) = match text.chars().next() {
        Some('+') => (1, &text[1..]),
        Some('-') => (-1, &text[1..]),
        _ => (1, text),
    };
    let digits_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let (digits, unit) = rest.split_at(digits_end);
    let Ok(amount) = digits.parse::<i64>() else {
        bail!("an offset looks like +2h, -30m, +90s or +500ms");
    };
    let unit_ms = match unit {
        "ms" => 1,
        "s" => 1000,
        "m" => 60_000,
        "h" => 3_600_000,
        _ => bail!("an offset looks like +2h, -30m, +90s or +500ms"),
    };
    Ok(sign * amount * unit_ms)
}

/// An offset as text, the inverse of `parse_offset` for display: `+2h`, `-30m`, `0`.
pub fn describe_offset(ms: i64) -> String {
    let sign = if ms < 0 { '-' } else { '+' };
    let abs = ms.abs();
    match abs {
        0 => "0".to_string(),
        _ if abs % 3_600_000 == 0 => format!("{sign}{}h", abs / 3_600_000),
        _ if abs % 60_000 == 0 => format!("{sign}{}m", abs / 60_000),
        _ if abs % 1000 == 0 => format!("{sign}{}s", abs / 1000),
        _ => format!("{sign}{abs}ms"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init(id: u64, label: &str, lines: &[&str]) -> MemberInit {
        MemberInit {
            id,
            label: label.to_string(),
            offset_ms: 0,
            lines: lines.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn texts(rows: &[(usize, String)]) -> Vec<&str> {
        rows.iter().map(|(_, text)| text.as_str()).collect()
    }

    #[test]
    fn the_lines_a_merge_starts_with_are_sorted_by_time() {
        let (_, rows) = Merger::start(vec![
            init(
                1,
                "web",
                &["2026-10-03 08:00:01 web a", "2026-10-03 08:00:04 web b"],
            ),
            init(
                2,
                "db",
                &["2026-10-03 08:00:02 db a", "2026-10-03 08:00:03 db b"],
            ),
        ]);
        assert_eq!(
            texts(&rows),
            [
                "2026-10-03 08:00:01 web a",
                "2026-10-03 08:00:02 db a",
                "2026-10-03 08:00:03 db b",
                "2026-10-03 08:00:04 web b"
            ]
        );
        assert_eq!(
            rows.iter().map(|(member, _)| *member).collect::<Vec<_>>(),
            [0, 1, 1, 0]
        );
    }

    #[test]
    fn a_stack_trace_stays_behind_its_header() {
        let (_, rows) = Merger::start(vec![
            init(
                1,
                "web",
                &[
                    "2026-10-03 08:00:01 ERROR boom",
                    "\tat a.B(B.java:1)",
                    "\tat c.D(D.java:2)",
                ],
            ),
            init(
                2,
                "db",
                &["2026-10-03 08:00:01 query", "2026-10-03 08:00:02 later"],
            ),
        ]);
        // Same second: arrival order decides, and the trace is not split from its header.
        assert_eq!(
            texts(&rows),
            [
                "2026-10-03 08:00:01 ERROR boom",
                "\tat a.B(B.java:1)",
                "\tat c.D(D.java:2)",
                "2026-10-03 08:00:01 query",
                "2026-10-03 08:00:02 later",
            ]
        );
    }

    #[test]
    fn a_clock_offset_moves_a_whole_source() {
        let (_, rows) = Merger::start(vec![
            MemberInit {
                offset_ms: 2 * 3_600_000,
                ..init(1, "utc+2", &["2026-10-03 06:00:00 local time"])
            },
            init(
                2,
                "utc",
                &["2026-10-03 07:59:59 before", "2026-10-03 08:00:01 after"],
            ),
        ]);
        // The "utc+2" line says 06:00 but means 08:00 UTC, so it sorts between the other two.
        assert_eq!(
            texts(&rows),
            [
                "2026-10-03 07:59:59 before",
                "2026-10-03 06:00:00 local time",
                "2026-10-03 08:00:01 after"
            ]
        );
    }

    #[test]
    fn live_lines_wait_for_a_slower_source_and_come_out_in_order() {
        let (mut merger, _) = Merger::start(vec![init(1, "a", &[]), init(2, "b", &[])]);
        let t0 = Instant::now();

        // Source a is ahead to :05; b has only reached :01. Nothing past :01 may be shown yet.
        merger.feed(1, "2026-10-03 08:00:05 a late", t0);
        merger.feed(2, "2026-10-03 08:00:01 b early", t0);
        assert!(
            merger.release(t0).is_empty(),
            "every line waits a moment first"
        );
        let settled = t0 + MIN_HOLD;
        assert_eq!(
            texts(&merger.release(settled)),
            ["2026-10-03 08:00:01 b early"]
        );

        // b catches up with a line that belongs before a's: now both can go, in order.
        merger.feed(2, "2026-10-03 08:00:03 b middle", settled);
        merger.feed(2, "2026-10-03 08:00:06 b next", settled);
        let settled_again = settled + MIN_HOLD;
        assert_eq!(
            texts(&merger.release(settled_again)),
            ["2026-10-03 08:00:03 b middle", "2026-10-03 08:00:05 a late"]
        );
    }

    #[test]
    fn a_silent_source_does_not_hold_the_others_back() {
        let (mut merger, _) = Merger::start(vec![init(1, "busy", &[]), init(2, "quiet", &[])]);
        let t0 = Instant::now();
        merger.feed(2, "2026-10-03 08:00:00 quiet once", t0);
        merger.feed(1, "2026-10-03 08:00:09 busy now", t0);
        assert_eq!(
            texts(&merger.release(t0 + MIN_HOLD)),
            ["2026-10-03 08:00:00 quiet once"]
        );

        // Two seconds on, "quiet" is idle: it is no longer waited for.
        let later = t0 + IDLE + Duration::from_millis(100);
        merger.feed(1, "2026-10-03 08:00:10 busy again", later);
        assert_eq!(
            texts(&merger.release(later + MIN_HOLD)),
            [
                "2026-10-03 08:00:09 busy now",
                "2026-10-03 08:00:10 busy again"
            ]
        );

        // A line from a source that was quiet is not shown at once: one stamped just before it,
        // arriving a moment later, still gets in front of it.
        let t1 = later + Duration::from_secs(10);
        merger.feed(1, "2026-10-03 08:00:20 busy", t1);
        merger.feed(
            2,
            "2026-10-03 08:00:19 quiet, a moment later",
            t1 + Duration::from_millis(300),
        );
        assert_eq!(
            texts(&merger.release(t1 + Duration::from_millis(300) + MIN_HOLD)),
            ["2026-10-03 08:00:19 quiet, a moment later"],
            "the later line waits: the source that just spoke may have more up to its time"
        );
        // Once that source falls quiet again, nothing holds the later line back.
        assert_eq!(
            texts(&merger.release(t1 + IDLE + Duration::from_millis(400))),
            ["2026-10-03 08:00:20 busy"]
        );
    }

    #[test]
    fn nothing_waits_forever_and_unknown_sources_are_ignored() {
        let (mut merger, _) = Merger::start(vec![init(1, "a", &[]), init(2, "b", &[])]);
        let t0 = Instant::now();
        merger.feed(9, "2026-10-03 08:00:00 stranger", t0);
        merger.feed(1, "2026-10-03 08:00:09 a", t0);
        merger.feed(2, "2026-10-03 08:00:01 b", t0);
        // Both talk steadily, but the line from a stays behind b's clock...
        assert_eq!(
            texts(&merger.release(t0 + MIN_HOLD)),
            ["2026-10-03 08:00:01 b"]
        );
        // ...until it has waited MAX_HOLD, or both go quiet.
        let much_later = t0 + MAX_HOLD + Duration::from_millis(1);
        assert_eq!(
            texts(&merger.release(much_later)),
            ["2026-10-03 08:00:09 a"]
        );
    }

    #[test]
    fn labels_lose_the_extension_and_the_shared_start() {
        let short = |names: &[&str]| short_labels(names);
        assert_eq!(
            short(&[
                "example_nginx_access.log",
                "example_nginx_error.log",
                "example_spring_boot.log"
            ]),
            ["nginx_access", "nginx_error", "spring_boot"]
        );
        assert_eq!(short(&["web.log", "db.log"]), ["web", "db"]);
        // A shared start that isn't a whole word stays: "api-1" / "api-2" keep their numbers.
        assert_eq!(short(&["api-1.log", "api-2.log"]), ["api-1", "api-2"]);
        assert_eq!(short(&["prod-web", "prod-db"]), ["web", "db"]);
        // Nothing in common, identical names, or a single name: nothing is lost.
        assert_eq!(short(&["alpha", "beta"]), ["alpha", "beta"]);
        assert_eq!(short(&["same.log", "same.log"]), ["same", "same"]);
        assert_eq!(short(&["only.log"]), ["only"]);
        assert_eq!(short(&["app_a", "app_"]), ["app_a", "app_"]);
    }

    #[test]
    fn rows_have_a_fixed_label_column() {
        assert_eq!(format_row("web", 6, "hello"), "web    │ hello");
        assert_eq!(format_row("a-very-long-name", 6, "x"), "a-very │ x");
        assert_eq!(row_prefix_len(6), "web    │ ".chars().count());
    }

    #[test]
    fn label_width_is_the_longest_label_but_capped() {
        let (merger, _) = Merger::start(vec![
            init(1, "web", &[]),
            init(2, "database-primary-01-xyz", &[]),
        ]);
        assert_eq!(merger.label_width(), MAX_LABEL);
        assert_eq!(merger.labels(), ["web", "database-primary-01-xyz"]);
    }

    #[test]
    fn offsets_parse_and_print() {
        assert_eq!(parse_offset("+2h").unwrap(), 7_200_000);
        assert_eq!(parse_offset("-30m").unwrap(), -1_800_000);
        assert_eq!(parse_offset("90s").unwrap(), 90_000);
        assert_eq!(parse_offset("+500ms").unwrap(), 500);
        assert_eq!(parse_offset("0").unwrap(), 0);
        for bad in ["", "2", "+h", "1x", "abc", "+1.5h"] {
            assert!(parse_offset(bad).is_err(), "{bad:?} should be rejected");
        }
        for ms in [0, 7_200_000, -1_800_000, 90_000, 500, -3_600_000] {
            assert_eq!(parse_offset(&describe_offset(ms)).unwrap(), ms);
        }
    }
}
