use std::fs;
use std::mem;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Result, bail};

use crate::filter::{Filter, FilterSpec};
use crate::group::GroupRule;
use crate::layout::rotate;
use crate::merge::{self, MemberInit};
use crate::session::{FilterTile, SourceState};
use crate::spec::SourceSpec;
use crate::tile::Tile;

/// Maximum number of lines kept in memory for a source's main tile.
const MAX_LINES: usize = 10_000;

/// Most filter tiles per source (the main tile is not counted).
const MAX_FILTER_TILES: usize = 6;

/// Hands out `Source::id`s.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// One log (a file, or the output of a command) and everything shown for it: the main tile plus
/// any filter tiles forked from it.
///
/// This is the "one ingest per source" idea. The main tile (`tiles[0]`) reads the source once.
/// The overview and the source's own tab both show that same tile, so a source costs the same
/// no matter how many places display it.
pub struct Source {
    /// Never changes and is never reused, unlike the position in the list of sources, which
    /// shifts when one is closed. A merged timeline follows its members by this.
    id: u64,
    spec: SourceSpec,
    /// What makes two sources the same one, so the same log isn't opened twice: the resolved
    /// path of a file, the text of a command. See `Source::identity_of`.
    identity: String,
    /// Where it comes from, as text: the file path, or `ssh:host:/path`, `docker:name`, ...
    path: String,
    /// Short label for tabs and overview tiles: the file name, the container, or a name that
    /// was chosen in the configuration.
    name: String,
    custom_name: Option<String>,
    /// `tiles[0]` is always the main tile.
    tiles: Vec<Tile>,
    /// Index into `tiles` of the tile that receives scroll keys and commands.
    focus: usize,
    /// Which lines belong together as one entry, for the filter tiles.
    group: GroupRule,
    /// Added to this source's timestamps when it is merged with others (clock skew, time zone).
    clock_offset_ms: i64,
    /// For a merged timeline: the ids of the sources it is made of.
    members: Vec<u64>,
    /// New lines of the main tile since they were last taken, kept only on request.
    collect_fresh: bool,
    fresh: Vec<String>,
    /// Lines per second over the last sampling interval, and the line count at its start.
    rate: f64,
    last_total: u64,
}

impl Source {
    /// Opens a file, or a command such as `ssh:host:/path`, `docker:name`, `kube:pod`.
    /// (The app goes through `open_named`, which also takes a name from the configuration.)
    #[cfg(test)]
    pub fn open(text: &str) -> Result<Self> {
        Self::open_named(text, None)
    }

    /// Like `open`, with a chosen label for the tab (from a named source in the config).
    pub fn open_named(text: &str, name: Option<&str>) -> Result<Self> {
        let spec = SourceSpec::parse(text)?;
        let (main, derived_name) = match &spec {
            SourceSpec::File(path) => {
                let name = Path::new(path)
                    .file_name()
                    .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
                (Tile::source(path, MAX_LINES)?, name)
            }
            SourceSpec::Command(command) => {
                (Tile::stream(command.clone(), MAX_LINES), command.name())
            }
            SourceSpec::Stdin => (Tile::stdin(MAX_LINES)?, "stdin".to_string()),
            SourceSpec::Merged => bail!("a merged timeline is made with :merge"),
        };
        // Standard input has no path to show; the others are shown as they are typed.
        let shown = match spec {
            SourceSpec::Stdin => "stdin".to_string(),
            _ => spec.describe(),
        };
        Ok(Self::assemble(
            shown,
            name.map_or(derived_name, str::to_string),
            name,
            spec,
            main,
        ))
    }

    /// A source that counts as standard input but reads nothing: tests can't use the real one.
    #[cfg(test)]
    pub fn fake_stdin() -> Self {
        let SourceSpec::Command(command) = SourceSpec::parse("cmd:true").unwrap() else {
            unreachable!()
        };
        let main = Tile::stream(command, MAX_LINES);
        Self::assemble(
            "stdin".into(),
            "stdin".into(),
            None,
            SourceSpec::Stdin,
            main,
        )
    }

    /// A timeline that interleaves `sources` by timestamp, as a source of its own.
    pub fn merged(sources: &[&Source]) -> Self {
        let names: Vec<&str> = sources.iter().map(|source| source.name.as_str()).collect();
        let labels = merge::short_labels(&names);
        let members = sources
            .iter()
            .zip(labels)
            .map(|(source, label)| MemberInit {
                id: source.id,
                label,
                offset_ms: source.clock_offset_ms,
                lines: source.tiles[0].snapshot(),
            })
            .collect();
        let main = Tile::merged(members);

        let names: Vec<&str> = sources.iter().map(|s| s.name.as_str()).collect();
        let mut merged = Self::assemble(
            format!("merged: {}", names.join(" + ")),
            "merged".to_string(),
            None,
            SourceSpec::Merged,
            main,
        );
        merged.members = sources.iter().map(|source| source.id).collect();
        merged
    }

    fn assemble(
        path: String,
        name: String,
        custom_name: Option<&str>,
        spec: SourceSpec,
        main: Tile,
    ) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        Self {
            id,
            identity: identity_of_spec(&spec, id),
            spec,
            path,
            name,
            custom_name: custom_name.map(str::to_string),
            tiles: vec![main],
            focus: 0,
            group: GroupRule::default(),
            clock_offset_ms: 0,
            members: Vec::new(),
            collect_fresh: false,
            fresh: Vec::new(),
            rate: 0.0,
            last_total: 0,
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn identity(&self) -> &str {
        &self.identity
    }

    /// The identity a source opened from `text` would have, without opening it. `a.log`,
    /// `./a.log`, its absolute path and a symlink to it are one file; `docker:api` is one
    /// container however often it is typed.
    pub fn identity_of(text: &str) -> Result<String> {
        Ok(identity_of_spec(&SourceSpec::parse(text)?, 0))
    }

    /// Is this standard input? It can't be saved in a session or reopened: there is one pipe,
    /// and it is read once.
    pub fn is_stdin(&self) -> bool {
        matches!(self.spec, SourceSpec::Stdin)
    }

    pub fn is_merged(&self) -> bool {
        matches!(self.spec, SourceSpec::Merged)
    }

    /// The ids of the sources a merged timeline is made of.
    pub fn members(&self) -> &[u64] {
        &self.members
    }

    pub fn set_clock_offset_ms(&mut self, offset: i64) {
        self.clock_offset_ms = offset;
    }

    /// Ask for the new lines of the main tile to be kept for `take_fresh`.
    pub fn set_collect_fresh(&mut self, collect: bool) {
        self.collect_fresh = collect;
    }

    /// The lines that arrived since the last call (only while `set_collect_fresh(true)`).
    pub fn take_fresh(&mut self) -> Vec<String> {
        mem::take(&mut self.fresh)
    }

    /// For a merged timeline: hands it the new lines of the sources it follows.
    pub fn merge_in(&mut self, fresh: &[(u64, Vec<String>)]) {
        for (id, lines) in fresh {
            self.tiles[0].merge_feed(*id, lines);
        }
    }

    /// Rebuilds a source from a saved session: reopens the file and recreates its filter tiles.
    pub fn from_state(state: &SourceState) -> Result<Self> {
        let mut source = Self::open_named(&state.path, state.name.as_deref())?;
        source.group = GroupRule::parse(&state.group)?;
        source.clock_offset_ms = state.offset_ms;
        for tile in &state.filters {
            source.push_filter(Filter::from_specs(&tile.chain)?)?;
        }
        source.set_focus(state.focus);
        Ok(source)
    }

    /// What to write into a session file. The path is made absolute, so the session loads
    /// the same files no matter which directory tilog is started from.
    pub fn to_state(&self) -> SourceState {
        // Only a file has a path to make absolute; a command is stored as it is.
        let path = match &self.spec {
            SourceSpec::File(path) => fs::canonicalize(path).map_or_else(
                |_| path.clone(),
                |absolute| absolute.to_string_lossy().into_owned(),
            ),
            SourceSpec::Command(command) => command.describe(),
            SourceSpec::Stdin => "-".to_string(),
            SourceSpec::Merged => "merge:".to_string(), // filled in by the app, which knows positions
        };
        let filters = self
            .tiles
            .iter()
            .filter_map(Tile::filter)
            .map(|filter| FilterTile {
                chain: filter.specs(),
            })
            .collect();
        SourceState {
            path,
            name: self.custom_name.clone(),
            offset_ms: self.clock_offset_ms,
            focus: self.focus,
            group: self.group.describe(),
            filters,
        }
    }

    /// Lines per second received during the last sampling interval.
    /// Changes whenever anything in this source's tiles changed on its own (see
    /// `Tile::activity`).
    pub fn activity(&self) -> u64 {
        self.tiles
            .iter()
            .fold(0u64, |sum, tile| sum.rotate_left(7) ^ tile.activity())
    }

    pub fn rate(&self) -> f64 {
        self.rate
    }

    /// Called about once a second with the seconds since the previous call.
    pub fn update_rate(&mut self, seconds: f64) {
        let total = self.main().total_lines();
        self.rate = total.saturating_sub(self.last_total) as f64 / seconds;
        self.last_total = total;
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tiles(&self) -> &[Tile] {
        &self.tiles
    }

    pub fn focus(&self) -> usize {
        self.focus
    }

    pub fn main(&self) -> &Tile {
        &self.tiles[0]
    }

    /// Every tile of this source stops following, where it is.
    pub fn pause_all(&mut self) {
        for tile in &mut self.tiles {
            tile.pause();
        }
    }

    /// Every tile of this source jumps to its newest line and follows.
    pub fn follow_all(&mut self) {
        for tile in &mut self.tiles {
            tile.jump_to_end();
        }
    }

    pub fn tile_mut(&mut self, index: usize) -> Option<&mut Tile> {
        self.tiles.get_mut(index)
    }

    pub fn set_focus(&mut self, index: usize) {
        if index < self.tiles.len() {
            self.focus = index;
        }
    }

    pub fn cycle_focus(&mut self, delta: isize) {
        self.focus = rotate(self.focus, self.tiles.len(), delta);
    }

    /// Takes in what the background threads delivered. Returns `true` if more may be waiting.
    ///
    /// Filters on a command's output have no thread of their own: the new lines of the main
    /// tile are handed to them here.
    pub fn pump(&mut self) -> bool {
        let feeding = self.collect_fresh || self.tiles.iter().any(Tile::wants_lines);
        let mut fresh = Vec::new();
        let mut busy = self.tiles[0].pump_collecting(feeding.then_some(&mut fresh));
        for tile in &mut self.tiles[1..] {
            tile.feed(&fresh);
            busy |= tile.pump();
        }
        if self.collect_fresh {
            self.fresh.append(&mut fresh);
        }
        busy
    }

    /// Forks the focused tile: the new tile shows what the focused tile's filter matches
    /// (nothing to inherit when forking the main tile) *and* the new pattern.
    pub fn open_filter(&mut self, pattern: &str, regex: bool, ignore_case: bool) -> Result<()> {
        let parent = self.tiles[self.focus].filter();
        let filter = Filter::new(parent, pattern, regex, ignore_case)?;
        self.push_filter(filter)
    }

    /// Like `open_filter`, but always from the whole log, whatever tile is focused.
    pub fn open_root_filter(
        &mut self,
        pattern: &str,
        regex: bool,
        ignore_case: bool,
    ) -> Result<()> {
        self.push_filter(Filter::new(None, pattern, regex, ignore_case)?)
    }

    /// Adds a tile for `filter` and focuses it.
    fn push_filter(&mut self, filter: Filter) -> Result<()> {
        if self.tiles.len() > MAX_FILTER_TILES {
            bail!("at most {MAX_FILTER_TILES} filter tiles per source (use :close)");
        }
        let tile = match &self.spec {
            SourceSpec::File(path) => Tile::filtered(path, filter, self.group.clone())?,
            // A command (or a merged timeline) has no file to scan: the filter starts from the
            // lines in memory. Rows of a merged timeline start with a source label, which is
            // shown but not searched.
            SourceSpec::Command(_) | SourceSpec::Stdin | SourceSpec::Merged => {
                Tile::stream_filtered(
                    filter,
                    self.group.clone(),
                    &self.tiles[0].snapshot(),
                    self.tiles[0].oldest_seq(),
                    self.tiles[0].label_cols(),
                )
            }
        };
        self.tiles.push(tile);
        self.focus = self.tiles.len() - 1;
        Ok(())
    }

    pub fn group(&self) -> &GroupRule {
        &self.group
    }

    /// Changes which lines form an entry. Filter tiles are rebuilt, because what they have
    /// indexed so far was cut into entries by the old rule.
    pub fn set_group(&mut self, rule: GroupRule) -> Result<()> {
        let chains: Vec<Vec<FilterSpec>> = self
            .tiles
            .iter()
            .filter_map(Tile::filter)
            .map(Filter::specs)
            .collect();
        let focus = self.focus;

        self.tiles.truncate(1); // dropping the old tiles stops their scanner threads
        self.group = rule;
        for chain in chains {
            self.push_filter(Filter::from_specs(&chain)?)?;
        }
        self.set_focus(focus);
        Ok(())
    }

    /// Closes every filter tile, leaving the main tile. Returns how many were closed.
    pub fn close_filters(&mut self) -> usize {
        let closed = self.tiles.len() - 1;
        self.tiles.truncate(1);
        self.focus = 0;
        closed
    }

    /// Closes the focused filter tile. Returns `false` if the main tile is focused: it is the
    /// source itself and can't be closed on its own.
    ///
    /// Removing the tile drops it, which drops its channel. The scanner thread notices on its
    /// next send and stops by itself.
    pub fn close_focused(&mut self) -> bool {
        if self.focus == 0 {
            return false;
        }
        self.tiles.remove(self.focus);
        self.focus -= 1;
        true
    }
}

/// See `Source::identity_of`. `id` only matters for a merged timeline, which is never the same
/// as another source.
fn identity_of_spec(spec: &SourceSpec, id: u64) -> String {
    match spec {
        // `canonicalize` makes the path absolute and follows symlinks. If it fails the file
        // doesn't exist, which opening it reports in its own way.
        SourceSpec::File(path) => fs::canonicalize(path).map_or_else(
            |_| path.clone(),
            |absolute| absolute.to_string_lossy().into_owned(),
        ),
        SourceSpec::Command(command) => command.describe(),
        SourceSpec::Stdin => "stdin:".to_string(),
        SourceSpec::Merged => format!("merge:{id}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forks_and_closes_filter_tiles() {
        let path =
            std::env::temp_dir().join(format!("tilog-source-test-{}.log", std::process::id()));
        std::fs::write(&path, "INFO a\nSEVERE b\n").unwrap();

        let mut source = Source::open(path.to_str().unwrap()).unwrap();
        assert_eq!(source.name(), path.file_name().unwrap().to_str().unwrap());
        assert!(
            !source.close_focused(),
            "the main tile can't be closed on its own"
        );

        source.open_filter("SEVERE", false, false).unwrap();
        assert_eq!((source.tiles().len(), source.focus()), (2, 1));
        assert!(source.open_filter("(", true, false).is_err());

        source.cycle_focus(1);
        assert_eq!(source.focus(), 0);
        source.cycle_focus(-1);
        assert!(source.close_focused());
        assert_eq!((source.tiles().len(), source.focus()), (1, 0));

        assert!(Source::open("/definitely/not/here.log").is_err());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn close_filters_and_session_state_roundtrip() {
        let path =
            std::env::temp_dir().join(format!("tilog-state-test-{}.log", std::process::id()));
        std::fs::write(&path, "INFO a\nSEVERE b\n").unwrap();

        let mut source = Source::open(path.to_str().unwrap()).unwrap();
        source.open_filter("SEVERE", false, false).unwrap();
        source.open_filter("b", false, true).unwrap();

        // Save and restore: same filter chains, same focus.
        let state = source.to_state();
        assert_eq!(state.filters.len(), 2);
        assert_eq!(
            state.filters[1].chain.len(),
            2,
            "the second filter inherits the first"
        );
        let restored = Source::from_state(&state).unwrap();
        assert_eq!(restored.tiles().len(), 3);
        assert_eq!(restored.focus(), 2);
        assert_eq!(
            restored.tiles()[2].filter().unwrap().label(),
            "SEVERE + b (i)"
        );

        assert_eq!(source.close_filters(), 2);
        assert_eq!((source.tiles().len(), source.focus()), (1, 0));
        assert_eq!(source.close_filters(), 0);
        std::fs::remove_file(&path).unwrap();
    }
}
