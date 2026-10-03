//! What the commands of the `:` prompt do.

use super::{Added, App, NO_SOURCE};
use crate::command;
use crate::command::Command;
use crate::completion;
use crate::group::GroupRule;
use crate::highlight::Highlight;
use crate::merge::describe_offset;
use crate::session;
use crate::source::Source;
use crate::tile::Tile;
use crate::timestamp;
use crate::when::When;

impl App {
    /// Parse a command line, then run it. Parsing and running are deliberately separate steps.
    pub(super) fn run_command_line(&mut self, text: &str) {
        match command::parse(text) {
            Ok(cmd) => self.execute(cmd),
            Err(err) => self.error(err.to_string()),
        }
    }

    /// The one place where commands change the app. Because `match` must be exhaustive,
    /// adding a variant to `Command` will not compile until it is handled here.
    pub(super) fn execute(&mut self, cmd: Command) {
        match cmd {
            Command::Help => {
                self.show_help = true;
                self.help_scroll = 0;
            }
            Command::Quit => self.quit = true,
            Command::Overview => self.tab = 0,
            Command::Add(path) => match self.add_path(&path) {
                Ok(Added::New) => {
                    self.info(format!("added {path} (select it and press Enter to open)"));
                }
                Ok(Added::Existing(index)) => {
                    self.info(format!(
                        "already open as tab {}: selected, not added again",
                        index + 1
                    ));
                }
                // `{:#}` prints the whole chain: "cannot open x: No such file or directory".
                Err(err) => self.error(format!("{err:#}")),
            },
            Command::Follow { all: true } => {
                for source in &mut self.sources {
                    source.follow_all();
                }
                self.info("following everywhere");
            }
            Command::Follow { all: false } => match self.with_target(Tile::jump_to_end) {
                Some(()) => self.info("following"),
                None => self.error(NO_SOURCE),
            },
            Command::Pause { all: true } => {
                for source in &mut self.sources {
                    source.pause_all();
                }
                self.info("paused everywhere (:follow all resumes)");
            }
            Command::Pause { all: false } => match self.with_target(Tile::pause) {
                Some(()) => self.info("paused (F or :follow resumes)"),
                None => self.error(NO_SOURCE),
            },
            Command::Top => match self.with_target(Tile::jump_to_start) {
                Some(()) => self.info("jumped to the first line"),
                None => self.error(NO_SOURCE),
            },
            Command::Goto(line) => match self.with_target(|tile| tile.jump_to_line(line)) {
                Some(()) => self.info(format!("jumped to line {line}")),
                None => self.error(NO_SOURCE),
            },
            Command::Write { path, force } => self.write(&path, force),
            Command::GotoTime(when) => self.goto_time(&when),
            Command::Clear => self.clear(),
            Command::Filter {
                pattern,
                regex,
                ignore_case,
                all: true,
            } => self.filter_everywhere(&pattern, regex, ignore_case),
            Command::Filter {
                pattern,
                regex,
                ignore_case,
                all: false,
            } => {
                // Filters live in a source's tab. From the overview, go to the selected
                // source first, then filter there.
                if self.tab == 0 {
                    if self.selected >= self.sources.len() {
                        self.error(NO_SOURCE);
                        return;
                    }
                    self.open_selected();
                }
                let Some(source) = self.current_source_mut() else {
                    return;
                };
                let result = source.open_filter(&pattern, regex, ignore_case);
                match result {
                    Ok(()) => self.info("new filter tile (Tab switches tiles, :close closes)"),
                    Err(err) => self.error(format!("{err:#}")),
                }
            }
            Command::Close => self.close(),
            Command::Highlight { pattern: None, .. } => {
                self.highlight = None;
                self.info("highlight cleared");
            }
            Command::Highlight {
                pattern: Some(pattern),
                regex,
                ignore_case,
            } => match Highlight::new(&pattern, regex, ignore_case) {
                Ok(highlight) => {
                    self.info(format!("highlighting {} (Esc clears)", highlight.label()));
                    self.highlight = Some(highlight);
                }
                Err(err) => self.error(format!("{err:#}")),
            },
            Command::Reload => match self.apply_config() {
                Ok(()) => self.info("config reloaded"),
                Err(err) => self.error(format!("config: {err:#}")),
            },
            Command::Group(rule) => self.group(rule),
            Command::Merge(words) => self.merge(&words),
            Command::Offset(ms) => self.offset(ms),
            Command::Save(name) => self.save(name),
            Command::Load(name) => {
                if let Err(err) = self.load_session(&name) {
                    self.error(format!("{err:#}"));
                }
            }
            Command::Sessions => match session::list() {
                Ok(names) if names.is_empty() => self.info("no saved sessions (use :save <name>)"),
                Ok(names) => self.info(format!("sessions: {}", names.join(", "))),
                Err(err) => self.error(format!("{err:#}")),
            },
        }
    }

    /// `:write`: the focused tile's lines go to a file. An existing file is only replaced with
    /// `-f`.
    pub(super) fn write(&mut self, path: &str, force: bool) {
        let path = completion::expand_home(path);
        let result = self.with_target(|tile| {
            tile.write_to(&path, force)
                .map(|count| (count, tile.holds_whole_log()))
        });
        match result {
            Some(Ok((count, whole))) => {
                let note = if whole {
                    ""
                } else {
                    " (the loaded part: a filter writes all its matches)"
                };
                self.info(format!("wrote {count} lines to {}{note}", path.display()));
            }
            Some(Err(err)) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                self.error(format!("{} exists (:write -f replaces it)", path.display()));
            }
            Some(Err(err)) => self.error(format!("cannot write {}: {err}", path.display())),
            None => self.error(NO_SOURCE),
        }
    }

    /// `:goto <time>`: the focused tile shows the line at or after that moment.
    pub(super) fn goto_time(&mut self, when: &When) {
        match self.with_target(|tile| tile.jump_to_time(when)) {
            Some(Ok(landed)) if landed.past_end => self.info(format!(
                "after the last line: showing the last one with a time, {}",
                timestamp::utc_clock(landed.time)
            )),
            Some(Ok(landed)) => {
                self.info(format!("jumped to {}", timestamp::utc_clock(landed.time)));
            }
            Some(Err(why)) => self.error(why),
            None => self.error(NO_SOURCE),
        }
    }

    /// `:filter -a`: the same new filter tile on every source, each from its whole log. The
    /// view stays where it is; the tiles are there when you open the sources.
    pub(super) fn filter_everywhere(&mut self, pattern: &str, regex: bool, ignore_case: bool) {
        let (mut opened, mut failed) = (0, None);
        for source in self.sources.iter_mut().filter(|source| !source.is_merged()) {
            match source.open_root_filter(pattern, regex, ignore_case) {
                Ok(()) => opened += 1,
                Err(err) => failed = failed.or(Some(format!("{err:#}"))),
            }
        }
        match (opened, failed) {
            (0, None) => self.error(NO_SOURCE),
            (0, Some(err)) => self.error(err),
            (n, None) => self.info(format!("new filter tile on {n} sources")),
            (n, Some(err)) => {
                self.error(format!("filter opened on {n} sources, not on all: {err}"))
            }
        }
    }

    /// In a source tab: closes all filter tiles. On the overview: empties the selected view.
    pub(super) fn clear(&mut self) {
        if let Some(source) = self.current_source_mut() {
            let closed = source.close_filters();
            match closed {
                0 => self.info("no filter tiles to clear"),
                1 => self.info("closed 1 filter tile"),
                n => self.info(format!("closed {n} filter tiles")),
            }
            return;
        }
        match self.with_target(Tile::clear) {
            Some(_) => self.info("view cleared"),
            None => self.error(NO_SOURCE),
        }
    }

    /// In a tab: closes the focused filter tile, or the whole source if the main tile is
    /// focused. On the overview: closes the selected source.
    pub(super) fn close(&mut self) {
        // A focused filter tile just closes. (A guard like `if source.close_focused()` in a
        // `match` arm can't borrow mutably, hence the plain `if let`.)
        if let Some(source) = self.current_source_mut()
            && source.close_focused()
        {
            self.info("tile closed");
            return;
        }

        // Otherwise the source itself goes: the one whose tab is open, or the selected one.
        let index = self.tab.checked_sub(1).unwrap_or(self.selected);
        if index >= self.sources.len() {
            self.error(NO_SOURCE);
            return;
        }
        // Dropping the source drops its tiles and channels; their threads stop by themselves,
        // and a command's process is killed.
        let source = self.sources.remove(index);
        self.tab = 0;
        self.selected = self.selected.min(self.sources.len().saturating_sub(1));
        self.info(format!("closed {}", source.name()));
    }

    /// `:merge`: one new tab with the chosen sources interleaved by timestamp. There is only
    /// ever one, so merging again replaces it (which is also how a changed `:offset` is applied).
    pub(super) fn merge(&mut self, words: &[String]) {
        // Resolve the words to source ids first: positions shift when the old timeline goes.
        let mut ids = Vec::new();
        if words.is_empty() {
            ids.extend(
                self.sources
                    .iter()
                    .filter(|s| !s.is_merged())
                    .map(Source::id),
            );
        }
        for word in words {
            let found = match word.parse::<usize>() {
                Ok(tab) => tab.checked_sub(1).and_then(|index| self.sources.get(index)),
                Err(_) => self.sources.iter().find(|source| source.name() == word),
            };
            match found {
                Some(source) if !source.is_merged() => {
                    if !ids.contains(&source.id()) {
                        ids.push(source.id());
                    }
                }
                _ => {
                    self.error(format!(
                        "no source \"{word}\" (use the number in front of a tab, or its name)"
                    ));
                    return;
                }
            }
        }
        if ids.len() < 2 {
            self.error(
                "merging needs at least two sources: :merge for all, or :merge 1 2 (tab numbers)",
            );
            return;
        }
        self.sources.retain(|source| !source.is_merged());
        let members: Vec<&Source> = ids
            .iter()
            .filter_map(|id| self.sources.iter().find(|source| source.id() == *id))
            .collect();
        let merged = Source::merged(&members);
        self.info(format!("merged {} sources by timestamp", members.len()));

        self.sources.push(merged);
        self.tab = self.sources.len(); // open it
        self.selected = self.selected.min(self.sources.len() - 1);
    }

    /// `:offset`: shifts the timestamps of the source in question, for the next `:merge`.
    pub(super) fn offset(&mut self, ms: i64) {
        let index = self.tab.checked_sub(1).unwrap_or(self.selected);
        let Some(source) = self.sources.get_mut(index) else {
            self.error(NO_SOURCE);
            return;
        };
        if source.is_merged() {
            self.error("a merged timeline has no clock of its own: set :offset on a source");
            return;
        }
        source.set_clock_offset_ms(ms);
        let text = format!(
            "clock offset of {}: {} (applied by the next :merge)",
            source.name(),
            describe_offset(ms)
        );
        self.info(text);
    }

    /// Shows or changes how lines form entries, for the open source (on the overview: the
    /// selected one).
    pub(super) fn group(&mut self, rule: Option<String>) {
        let index = self.tab.checked_sub(1).unwrap_or(self.selected);
        let Some(source) = self.sources.get_mut(index) else {
            self.error(NO_SOURCE);
            return;
        };

        let message = match rule {
            None => Ok(format!(
                "grouping for {}: {}",
                source.name(),
                source.group().describe()
            )),
            Some(text) => GroupRule::parse(&text)
                .and_then(|rule| source.set_group(rule))
                .map(|()| {
                    format!(
                        "grouping: {} (filter tiles rebuilt)",
                        source.group().describe()
                    )
                }),
        };
        match message {
            Ok(text) => self.info(text),
            Err(err) => self.error(format!("{err:#}")),
        }
    }

    pub(super) fn save(&mut self, name: Option<String>) {
        let Some(name) = name.or_else(|| self.session_name.clone()) else {
            self.error("usage: :save <name>");
            return;
        };
        match self.snapshot().save(&name) {
            Ok(path) => {
                self.session_name = Some(name);
                self.info(format!("saved {}", path.display()));
            }
            Err(err) => self.error(format!("{err:#}")),
        }
    }
}
