//! The application: what is open, which tab is showing, and what commands do.
//!
//! This file holds the state (`App`) and the event loop. The behaviour is split by topic into
//! child modules, each adding methods to `App`: `keys` (keyboard and mouse), `ui` (drawing),
//! `commands` (the `:` commands), `sources` (opening sources, sessions), `selection` (mouse
//! selection and copying) and `areas` (the screen layout). Child modules can use this file's
//! private items: a module may see everything in the modules above it.

mod areas;
mod commands;
mod keys;
mod selection;
mod sources;
#[cfg(test)]
mod tests;
mod ui;

use self::areas::Areas;
use self::keys::Prompt;
use crate::config::NamedSource;
use crate::highlight::Highlight;
use crate::input::InputBox;
use crate::layout;
use crate::layout::rotate;
use crate::select::Selection;
use crate::source::Source;
use crate::theme::Theme;
use crate::tile::Tile;
use crate::timestamp;
use anyhow::Result;
use crossterm::event;
use crossterm::event::{Event, KeyEventKind};
use ratatui::DefaultTerminal;
use ratatui::layout::Rect;
use std::time::Duration;
use std::time::Instant;

/// The most logs open at once. The number keys `1`-`9` open them, and nine windows are the most
/// that stay readable on one screen (a 3 x 3 grid). The merged timeline is not counted: it is a
/// view of the others, and gets a tab of its own on top (the `m` key jumps to it).
pub const MAX_SOURCES: usize = 9;

/// Rows taken by a border (top and bottom), so not available for text.
const BORDER_ROWS: u16 = 2;

/// Lines moved per notch of the mouse wheel.
const WHEEL_LINES: u64 = 3;

/// Columns moved per sideways scroll step.
const H_STEP: usize = 8;

const NO_SOURCE: &str = "no source yet: add one with :add <path>";

/// A one-line message, shown on the status line until the next key press.
struct Notice {
    text: String,
    is_error: bool,
    /// When it was set, so it can fade after a while.
    since: Instant,
}

/// How long a message stays on the status line if no key is pressed. An error stays longer: it
/// is usually longer, and the one you must not miss.
const INFO_SECONDS: u64 = 3;
const ERROR_SECONDS: u64 = 5;

impl Notice {
    fn is_expired(&self, now: Instant) -> bool {
        let limit = if self.is_error {
            ERROR_SECONDS
        } else {
            INFO_SECONDS
        };
        now.duration_since(self.since) >= Duration::from_secs(limit)
    }
}

/// What `App::add_path` did.
#[derive(Debug, PartialEq)]
pub enum Added {
    /// A new source was opened.
    New,
    /// It was open already, as the source at this position; no second one was made.
    Existing(usize),
}

/// What the last drawn screen was made from, to tell when it is out of date: the sources'
/// activity, and the second (the clock, the rates and the retry countdowns change once a second).
#[derive(Default, PartialEq)]
struct Drawn {
    activity: u64,
    second: i64,
}

impl Drawn {
    fn of(app: &App) -> Self {
        Self {
            activity: app
                .sources
                .iter()
                .fold(0u64, |sum, source| sum.rotate_left(11) ^ source.activity()),
            second: timestamp::now_ms().div_euclid(1000),
        }
    }
}

/// What kind of argument Tab completes for a command.
enum ArgKind {
    Path,
    Session,
    /// The host in `ssh:host:/path`, from `~/.ssh/config`.
    Ssh,
}

/// The matches listed after Tab on an ambiguous argument, and which one is highlighted.
#[derive(Default)]
struct Candidates {
    items: Vec<String>,
    selected: usize,
    /// The prompt text in front of the part a match replaces (`add logs/`), and after it.
    base: String,
    suffix: String,
}

impl Candidates {
    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    fn clear(&mut self) {
        *self = Self::default();
    }

    /// Moves the highlight, wrapping around.
    fn step(&mut self, delta: isize) {
        self.selected = rotate(self.selected, self.items.len(), delta);
    }

    /// The prompt text with the highlighted match put in.
    fn chosen_text(&self) -> Option<String> {
        let item = self.items.get(self.selected)?;
        Some(format!("{}{item}{}", self.base, self.suffix))
    }
}

/// All state of the program lives in this struct (Rust's closest thing to a "class's fields").
pub struct App {
    sources: Vec<Source>,
    /// 0 is the overview; tab `n` shows `sources[n - 1]`.
    tab: usize,
    /// The selected window in the overview (an index into `sources`).
    selected: usize,
    /// `None`: normal mode, where keys navigate. `Some`: a prompt is open on the status line
    /// (search, filter or command) and keys edit its text.
    prompt: Option<Prompt>,
    /// The text of the open prompt.
    input: InputBox,
    /// Highlighted row of the command menu (see `menu_items`).
    menu_selected: usize,
    /// Matches listed after a Tab press on an ambiguous argument.
    candidates: Candidates,
    notice: Option<Notice>,
    /// How log lines are colored (from the configuration file).
    theme: Theme,
    /// Sources named in the configuration file.
    named_sources: Vec<NamedSource>,
    /// Text marked in every window until cleared (set by `/` search, or `:highlight`).
    highlight: Option<Highlight>,
    /// The same while still typing a search or filter: marked live.
    preview: Option<Highlight>,
    /// Name of the session last loaded or saved, so a bare `:save` knows where to write.
    session_name: Option<String>,
    /// The text being selected with the mouse, or just selected (until the next key or click).
    selection: Option<Selection>,
    show_help: bool,
    /// How far the help screen is scrolled.
    help_scroll: usize,
    quit: bool,
    last_sample: Instant,
    /// `NO_COLOR` is set: the screen is drawn without color (see `ui::mono`).
    no_color: bool,
}

impl App {
    /// An "associated function" (like a static method). `new` is only a convention.
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
            tab: 0,
            selected: 0,
            prompt: None,
            input: InputBox::default(),
            menu_selected: 0,
            candidates: Candidates::default(),
            notice: None,
            theme: Theme::builtin(),
            named_sources: Vec::new(),
            highlight: None,
            preview: None,
            session_name: None,
            selection: None,
            show_help: false,
            help_scroll: 0,
            quit: false,
            last_sample: Instant::now(),
            no_color: ui::mono::requested(),
        }
    }

    /// Shows `text` as an error on the status line.
    pub fn show_error(&mut self, text: &str) {
        self.error(text);
    }

    /// `mut self`: we own the App and may modify it. We still consume it, as before.
    pub fn run(mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        // What the screen showed last time: nothing is drawn again until something changes.
        // Redrawing costs more than everything else the program does while idle.
        let mut drawn = Drawn::default();
        let mut input_seen = true;
        while !self.quit {
            // Take in what the background threads delivered. While there is a backlog (a big
            // file being loaded), don't idle: come straight back for the next chunk.
            let busy = self.pump_sources();
            self.sample_rates();
            input_seen |= self.expire_notice(Instant::now());

            let now = Drawn::of(&self);
            if input_seen || now != drawn {
                terminal.draw(|frame| {
                    self.render(frame);
                    if self.no_color {
                        ui::mono::strip_colors(frame.buffer_mut());
                    }
                })?;
                drawn = now;
                input_seen = false;
            }

            // Otherwise wait for input at most 100 ms, then loop again to check for new data.
            let timeout = if busy {
                Duration::ZERO
            } else {
                Duration::from_millis(100)
            };
            if event::poll(timeout)? {
                let size = terminal.size()?;
                let areas = self.areas(Rect::new(0, 0, size.width, size.height));
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => self.on_key(key, &areas),
                    Event::Mouse(mouse) => self.on_mouse(mouse, &areas),
                    _ => {}
                }
                // Any input, a resize included, may have changed the screen.
                input_seen = true;
            }
        }
        Ok(())
    }

    /// Takes in what the background threads delivered. Returns `true` if more may be waiting.
    ///
    /// A merged timeline has no thread of its own: the new lines of the sources it is made of
    /// are collected here and handed to it, after which it releases the ones whose turn has come.
    fn pump_sources(&mut self) -> bool {
        let merging = self.sources.iter().any(Source::is_merged);
        let mut busy = false;
        let mut fresh: Vec<(u64, Vec<String>)> = Vec::new();

        for source in self.sources.iter_mut().filter(|source| !source.is_merged()) {
            source.set_collect_fresh(merging);
            busy |= source.pump();
            if merging {
                fresh.push((source.id(), source.take_fresh()));
            }
        }
        for source in self.sources.iter_mut().filter(|source| source.is_merged()) {
            source.merge_in(&fresh);
            busy |= source.pump();
        }
        busy
    }

    /// About once a second, let every source work out its lines-per-second.
    fn sample_rates(&mut self) {
        let elapsed = self.last_sample.elapsed();
        if elapsed >= Duration::from_secs(1) {
            for source in &mut self.sources {
                source.update_rate(elapsed.as_secs_f64());
            }
            self.last_sample = Instant::now();
        }
    }

    // ---- what is showing ---------------------------------------------------------------

    /// The source whose tab is showing, or `None` on the overview.
    fn current_source(&self) -> Option<&Source> {
        self.tab
            .checked_sub(1)
            .and_then(|index| self.sources.get(index))
    }

    fn current_source_mut(&mut self) -> Option<&mut Source> {
        self.tab
            .checked_sub(1)
            .and_then(|index| self.sources.get_mut(index))
    }

    fn areas(&self, screen: Rect) -> Areas {
        let mut areas = Areas::new(screen);
        areas.tiles = match self.current_source() {
            None => layout::grid(areas.body, self.sources.len()),
            Some(source) => layout::main_and_filters(areas.body, source.tiles().len()),
        };
        areas
    }

    /// Index (into the current tab's tiles) of the tile that keys and commands act on.
    fn target_index(&self) -> usize {
        self.current_source().map_or(self.selected, Source::focus)
    }

    // The methods below index `self.sources` directly instead of calling
    // `current_source_mut()`. A method call borrows *all* of `self`, so the other match arm
    // could no longer touch `self.sources` or `self.selected`; a field access borrows only
    // that field.
    fn tile_mut(&mut self, index: usize) -> Option<&mut Tile> {
        match self.tab.checked_sub(1) {
            Some(source) => self.sources.get_mut(source)?.tile_mut(index),
            // On the overview, tile `i` is the main tile of source `i`.
            None => self.sources.get_mut(index)?.tile_mut(0),
        }
    }

    /// Tile `index` of the current tab, for reading.
    fn tile_ref(&self, index: usize) -> Option<&Tile> {
        match self.tab.checked_sub(1) {
            Some(source) => self.sources.get(source)?.tiles().get(index),
            None => self.sources.get(index).map(Source::main),
        }
    }

    /// Runs `f` on the target tile. `None` if there is no tile (empty overview).
    /// Generic over the closure's return type `R`, so callers can get a result back.
    fn with_target<R>(&mut self, f: impl FnOnce(&mut Tile) -> R) -> Option<R> {
        let index = self.target_index();
        self.tile_mut(index).map(f)
    }

    fn select_tile(&mut self, index: usize) {
        match self.tab.checked_sub(1) {
            Some(source) => {
                if let Some(source) = self.sources.get_mut(source) {
                    source.set_focus(index);
                }
            }
            None if index < self.sources.len() => self.selected = index,
            None => {}
        }
    }

    fn cycle_selection(&mut self, delta: isize) {
        match self.tab.checked_sub(1) {
            Some(source) => {
                if let Some(source) = self.sources.get_mut(source) {
                    source.cycle_focus(delta);
                }
            }
            None => self.selected = rotate(self.selected, self.sources.len(), delta),
        }
    }

    /// Tabs are the overview plus one per source.
    fn tab_count(&self) -> usize {
        self.sources.len() + 1
    }

    fn go_to_tab(&mut self, tab: usize) {
        if tab < self.tab_count() {
            self.tab = tab;
        }
    }

    /// Enter on the overview: open the selected source's tab.
    fn open_selected(&mut self) {
        if self.tab == 0 && self.selected < self.sources.len() {
            self.tab = self.selected + 1;
        }
    }

    /// What to mark right now: the text being typed, else the one kept by Enter.
    fn active_highlight(&self) -> Option<&Highlight> {
        self.preview.as_ref().or(self.highlight.as_ref())
    }

    // ---- commands ----------------------------------------------------------------------

    // `impl Into<String>` accepts both `&str` and `String` arguments.
    /// Takes the message off the status line once it has been there long enough.
    /// Returns `true` if the message was taken off, so the screen needs drawing again.
    fn expire_notice(&mut self, now: Instant) -> bool {
        let expired = self
            .notice
            .as_ref()
            .is_some_and(|notice| notice.is_expired(now));
        if expired {
            self.notice = None;
        }
        expired
    }

    fn info(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            is_error: false,
            since: Instant::now(),
        });
    }

    fn error(&mut self, text: impl Into<String>) {
        self.notice = Some(Notice {
            text: text.into(),
            is_error: true,
            since: Instant::now(),
        });
    }
}
