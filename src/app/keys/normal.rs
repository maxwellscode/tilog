//! Normal mode: every key navigates.

use super::Prompt;
use crate::app::{App, H_STEP, NO_SOURCE};
use crate::layout::rotate;
use crate::tile::{Find, Link, Tile};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

impl App {
    pub(super) fn on_normal_key(
        &mut self,
        key: KeyEvent,
        height: usize,
        width: usize,
        main_height: usize,
    ) {
        let page = height.saturating_sub(1).max(1) as u64;
        let half = (height / 2).max(1) as u64;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        match key.code {
            KeyCode::Char('c') if ctrl => self.quit = true,

            // Tabs: Alt+arrows, or digits (0 = overview, 1 = first source, ...).
            KeyCode::Left if alt => self.go_to_tab(rotate(self.tab, self.tab_count(), -1)),
            KeyCode::Right if alt => self.go_to_tab(rotate(self.tab, self.tab_count(), 1)),
            KeyCode::Char(c) if c.is_ascii_digit() => {
                self.go_to_tab(usize::from(c as u8 - b'0'));
            }

            // The merged timeline, which can be a tenth tab that no digit reaches.
            KeyCode::Char('m') => self.go_to_merged(),

            // Prompts and help.
            KeyCode::Char('/') => self.open_prompt(Prompt::Search),
            KeyCode::Char('&') => self.open_prompt(Prompt::Filter),
            KeyCode::Char(':') => self.open_prompt(Prompt::Command),
            KeyCode::Char('?') => {
                self.show_help = true;
                self.help_scroll = 0;
            }

            // Next and previous search match.
            KeyCode::Char('n') => self.next_occurrence(true, height, main_height),
            // Between error lines.
            KeyCode::Char(']') => self.next_error(true, height),
            KeyCode::Char('[') => self.next_error(false, height),
            KeyCode::Char('N') => self.next_occurrence(false, height, main_height),

            // Leaving: back from a tab to the overview; quitting from the overview.
            KeyCode::Char('q') if self.tab == 0 => self.quit = true,
            KeyCode::Char('q') => self.tab = 0,
            KeyCode::Esc if self.highlight.is_some() => {
                self.highlight = None;
                self.info("highlight cleared");
            }
            KeyCode::Esc => self.tab = 0,

            // Selecting a window: on the overview a source, in a tab one of its tiles.
            KeyCode::Tab => self.cycle_selection(1),
            KeyCode::BackTab => self.cycle_selection(-1),
            KeyCode::Enter if self.tab == 0 => self.open_selected(),

            // Scrolling the selected window, with the keys of `less`.
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Enter => {
                self.with_target(|tile| tile.scroll_down(height, 1));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.with_target(|tile| tile.scroll_up(height, 1));
            }
            KeyCode::Char(' ' | 'f') | KeyCode::PageDown => {
                self.with_target(|tile| tile.scroll_down(height, page));
            }
            KeyCode::Char('b') | KeyCode::PageUp => {
                self.with_target(|tile| tile.scroll_up(height, page));
            }
            KeyCode::Char('d') => {
                self.with_target(|tile| tile.scroll_down(height, half));
            }
            KeyCode::Char('u') => {
                self.with_target(|tile| tile.scroll_up(height, half));
            }
            KeyCode::Char('g') | KeyCode::Home => {
                self.with_target(Tile::jump_to_start);
            }
            KeyCode::Char('G' | 'F') | KeyCode::End => {
                self.with_target(Tile::jump_to_end);
            }
            KeyCode::Char('h') | KeyCode::Left => {
                self.with_target(|tile| tile.scroll_left(H_STEP));
            }
            KeyCode::Char('l') | KeyCode::Right => {
                self.with_target(|tile| tile.scroll_right(height, width, H_STEP));
            }
            _ => {}
        }
    }

    /// `n` / `N`. In a filter's pane the main pane follows: with a search on, to the entry that
    /// has the next or previous hit; without one, to the next or previous entry of the filter.
    /// Anywhere else: the next or previous search match.
    pub(super) fn next_occurrence(&mut self, forward: bool, height: usize, main_height: usize) {
        let in_filter = self.tab > 0 && self.with_target(|tile| tile.is_filter()) == Some(true);
        let direction = if forward { Find::Next } else { Find::Previous };
        if !in_filter {
            self.next_match(direction, height);
            return;
        }

        // Where the entry lies in the source, and how many lines it has.
        let (at, rows, wrapped) = if self.highlight.is_some() {
            if !self.next_match(direction, height) {
                return;
            }
            match self.with_target(|tile| tile.link_of_match_row()).flatten() {
                Some((at, rows)) => (at, rows, None),
                None => return,
            }
        } else {
            let Some(step) = self
                .with_target(|tile| tile.step_match(forward, height))
                .flatten()
            else {
                return self.error("no matches yet");
            };
            let position = format!("match {}/{}", step.index + 1, step.total);
            (step.at, step.rows, step.wrapped.then_some(position))
        };

        let shown = self
            .tile_mut(0)
            .is_some_and(|main| main.show_link(at, rows, main_height));
        if !shown {
            return self.error(match at {
                Link::Offset(_) => "cannot show the match: the file is not readable",
                Link::Seq(_) => "that line is no longer held: only the newest lines are kept",
            });
        }
        if let Some(position) = wrapped {
            self.info(format!("{position} (wrapped around)"));
        }
    }

    /// `]` / `[`: jump to the next or previous error line of the focused tile.
    pub(super) fn next_error(&mut self, forward: bool, height: usize) {
        match self.with_target(|tile| tile.find_error(forward, height)) {
            Some(Some(false)) => {}
            Some(Some(true)) => self.info("wrapped around"),
            Some(None) => self.error("no error lines in what is loaded"),
            None => self.error(NO_SOURCE),
        }
    }

    /// `n` / `N`: jump to the next or previous match of the search. `true` if there was one.
    pub(super) fn next_match(&mut self, direction: Find, height: usize) -> bool {
        let Some(highlight) = self.highlight.clone() else {
            self.error("nothing to search for: press / first");
            return false;
        };
        match self.with_target(|tile| tile.find_match(&highlight, height, direction)) {
            Some(Some(false)) => return true,
            Some(Some(true)) => self.info("search wrapped around"),
            Some(None) => {
                self.error(format!("pattern not found: {}", highlight.label()));
                return false;
            }
            None => {
                self.error(NO_SOURCE);
                return false;
            }
        }
        true
    }
}
