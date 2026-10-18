//! Lines a reconnected command sends again.
//!
//! After a dropped connection, `kubectl logs` and `docker logs` start again `--since` a moment
//! a little before the drop, so nothing is lost; the price is a few seconds of lines that were
//! already shown. These are recognized and left out: the first lines after a reconnect are held
//! back while they repeat the end of what came before, and dropped once they reach that end. If
//! they stop repeating it first, everything held back is passed on. A line is never lost; at
//! worst a repeat is shown.

use std::collections::VecDeque;
use std::hash::{DefaultHasher, Hash, Hasher};

/// How many of the latest lines are remembered (as hashes: 8 bytes each). More than a margin of
/// a few seconds holds even at thousands of lines per second.
const REMEMBERED: usize = 20_000;

#[derive(Default)]
pub struct Overlap {
    /// Hashes of the latest lines, oldest first.
    recent: VecDeque<u64>,
    /// After a reconnect: the lines held back so far, and where in `recent` they could start
    /// (each such start lines up with all of them). `None` when not checking.
    held: Option<(Vec<String>, Vec<usize>)>,
}

impl Overlap {
    /// The command was started again: what comes first may be a repeat.
    pub fn reconnected(&mut self) {
        self.held = Some((Vec::new(), Vec::new()));
    }

    /// Takes in a line from the command and returns the lines to show now (in order): none
    /// while it may still be a repeat, then either nothing (it was) or all that was held back.
    pub fn take(&mut self, line: String) -> Vec<String> {
        let hash = hash_of(&line);
        let Some((held, starts)) = self.held.as_mut() else {
            self.remember(hash);
            return vec![line];
        };

        // Which starts still line up: the first line can start anywhere it occurs, a later one
        // must be the next line after its start's run so far.
        let offset = held.len();
        let next: Vec<usize> = if offset == 0 {
            (0..self.recent.len())
                .filter(|&i| self.recent[i] == hash)
                .collect()
        } else {
            starts
                .iter()
                .copied()
                .filter(|&start| self.recent.get(start + offset) == Some(&hash))
                .collect()
        };
        held.push(line);

        if next.is_empty() {
            // Not a repeat after all: show everything that was held back, and stop checking.
            let (lines, _) = self.held.take().unwrap_or_default();
            for line in &lines {
                self.remember(hash_of(line));
            }
            return lines;
        }
        // A start whose run reached the newest remembered line: all held lines were repeats.
        if next
            .iter()
            .any(|&start| start + offset + 1 == self.recent.len())
        {
            self.held = None;
            return Vec::new();
        }
        *starts = next;
        Vec::new()
    }

    /// The output ended while lines were held back: they are shown after all.
    pub fn flush(&mut self) -> Vec<String> {
        let (lines, _) = self.held.take().unwrap_or_default();
        for line in &lines {
            self.remember(hash_of(line));
        }
        lines
    }

    fn remember(&mut self, hash: u64) {
        if self.recent.len() == REMEMBERED {
            self.recent.pop_front();
        }
        self.recent.push_back(hash);
    }
}

fn hash_of(line: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    line.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feeds `lines` and returns what was shown.
    fn feed(overlap: &mut Overlap, lines: &[&str]) -> Vec<String> {
        lines
            .iter()
            .flat_map(|line| overlap.take((*line).to_string()))
            .collect()
    }

    #[test]
    fn lines_pass_straight_through_without_a_reconnect() {
        let mut overlap = Overlap::default();
        assert_eq!(feed(&mut overlap, &["a", "a", "b"]), ["a", "a", "b"]);
    }

    #[test]
    fn the_repeated_end_after_a_reconnect_is_left_out() {
        let mut overlap = Overlap::default();
        feed(&mut overlap, &["t1", "t2", "t3", "t4"]);
        overlap.reconnected();
        // `--since` went back two lines: those two are repeats, the rest is new.
        assert_eq!(feed(&mut overlap, &["t3", "t4", "t5", "t6"]), ["t5", "t6"]);
    }

    #[test]
    fn a_new_start_with_nothing_in_common_is_shown_whole() {
        let mut overlap = Overlap::default();
        feed(&mut overlap, &["t1", "t2"]);
        overlap.reconnected();
        // A restarted pod: a log of its own.
        assert_eq!(feed(&mut overlap, &["n1", "n2"]), ["n1", "n2"]);
    }

    #[test]
    fn lines_that_only_look_like_the_end_for_a_while_are_not_lost() {
        let mut overlap = Overlap::default();
        feed(&mut overlap, &["a", "b", "c", "d"]);
        overlap.reconnected();
        // `b c` repeat the middle, then something new follows instead of `d`: nothing goes.
        assert_eq!(feed(&mut overlap, &["b", "c", "x"]), ["b", "c", "x"]);
    }

    #[test]
    fn repeated_identical_lines_line_up_with_the_newest_end() {
        let mut overlap = Overlap::default();
        feed(&mut overlap, &["ping", "ping", "ping"]);
        overlap.reconnected();
        // One `ping` reaches the end at once: it was a repeat, and checking stops there. What
        // follows is shown, even if it is another `ping` (a repeat or new: it can't be told).
        assert_eq!(
            feed(&mut overlap, &["ping", "ping", "pong"]),
            ["ping", "pong"]
        );
    }

    #[test]
    fn lines_held_when_the_output_ends_are_shown() {
        let mut overlap = Overlap::default();
        feed(&mut overlap, &["a", "b", "c"]);
        overlap.reconnected();
        assert!(feed(&mut overlap, &["a", "b"]).is_empty(), "held back");
        assert_eq!(overlap.flush(), ["a", "b"]);
    }

    #[test]
    fn only_so_many_lines_are_remembered() {
        let mut overlap = Overlap::default();
        for n in 0..REMEMBERED + 10 {
            overlap.take(n.to_string());
        }
        assert_eq!(overlap.recent.len(), REMEMBERED);
    }
}
