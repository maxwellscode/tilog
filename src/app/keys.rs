//! Keys and mouse. tilog has two modes, like `less` and `vim`:
//!
//! * **Normal**: every key navigates. `j k Space b g G ...` scroll, `Tab` selects a window.
//! * **Prompt**: opened by `/` (search), `&` (filter) or `:` (command). The status line becomes
//!   a text box. `Enter` runs it, `Esc` cancels.

mod mouse;
mod normal;
mod prompt;

use crate::app::{App, Areas};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What the open prompt is for.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Prompt {
    /// `/`: mark text, jump to the first match.
    Search,
    /// `&`: open a filter tile.
    Filter,
    /// `:`: run a command.
    Command,
}

impl Prompt {
    pub(super) fn prefix(self) -> char {
        match self {
            Self::Search => '/',
            Self::Filter => '&',
            Self::Command => ':',
        }
    }
}

impl App {
    pub(super) fn on_key(&mut self, key: KeyEvent, areas: &Areas) {
        if self.show_help {
            self.on_help_key(key, areas);
            return;
        }
        self.notice = None;
        self.selection = None;

        let target = self.target_index();
        let (height, width) = (areas.tile_height(target), areas.tile_width(target));
        match self.prompt {
            Some(prompt) => self.on_prompt_key(prompt, key, height),
            None => self.on_normal_key(key, height, width, areas.tile_height(0)),
        }
        // The box of matches stays while you move through it (arrows) or press Tab again;
        // any other key, typing included, closes it.
        if !matches!(key.code, KeyCode::Tab | KeyCode::Up | KeyCode::Down) {
            self.candidates.clear();
        }
        self.refresh_preview();
    }

    fn on_help_key(&mut self, key: KeyEvent, areas: &Areas) {
        let total = super::ui::help_lines(super::ui::help_inner_width(areas.screen.width)).len();
        let visible = (total + 2)
            .min(usize::from(areas.screen.height))
            .saturating_sub(2);
        let max = total.saturating_sub(visible);
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        match key.code {
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('q' | '?') | KeyCode::Esc => self.show_help = false,
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Enter => self.help_scroll += 1,
            KeyCode::Char('k') | KeyCode::Up => {
                self.help_scroll = self.help_scroll.saturating_sub(1);
            }
            KeyCode::Char(' ' | 'f') | KeyCode::PageDown => self.help_scroll += visible,
            KeyCode::Char('b') | KeyCode::PageUp => {
                self.help_scroll = self.help_scroll.saturating_sub(visible);
            }
            KeyCode::Char('g') | KeyCode::Home => self.help_scroll = 0,
            KeyCode::Char('G') | KeyCode::End => self.help_scroll = max,
            _ => {}
        }
        self.help_scroll = self.help_scroll.min(max);
    }
}
