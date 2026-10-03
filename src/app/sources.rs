//! Opening, naming and restoring sources: `add_path`, the source limit, sessions, the config.

use super::{Added, App, MAX_SOURCES};
use crate::config;
use crate::session::Session;
use crate::source::Source;
use crate::theme::Theme;
use anyhow::Result;
use anyhow::bail;
use std::collections::HashMap;

impl App {
    /// Opens a source and selects it on the overview. `text` is a file, `ssh:host:/path`,
    /// `docker:name`, `kube:pod`, `cmd:...`, or the name of a source from the configuration.
    ///
    /// A log that is already open is not opened again: that one is selected instead.
    pub fn add_path(&mut self, text: &str) -> Result<Added> {
        let named = self.named_sources.iter().find(|source| source.name == text);
        let (spec, name) = match named {
            Some(named) => (named.spec.as_str(), Some(named.name.as_str())),
            None => (text, None),
        };

        // Before the limit is checked: asking for something that is open is fine when full.
        let identity = Source::identity_of(spec)?;
        if let Some(index) = self
            .sources
            .iter()
            .position(|source| source.identity() == identity)
        {
            self.selected = index;
            return Ok(Added::Existing(index));
        }
        if self.is_full() {
            bail!(
                "at most {MAX_SOURCES} sources (the keys 1-{MAX_SOURCES}): close one first with :close"
            );
        }

        self.sources.push(Source::open_named(spec, name)?);
        self.selected = self.sources.len() - 1;
        Ok(Added::New)
    }

    /// Is there no room for another source? (The merged timeline takes no room.)
    pub fn is_full(&self) -> bool {
        self.sources
            .iter()
            .filter(|source| !source.is_merged())
            .count()
            >= MAX_SOURCES
    }

    /// `m`: the tab of the merged timeline, if there is one.
    pub(super) fn go_to_merged(&mut self) {
        match self.sources.iter().position(Source::is_merged) {
            Some(index) => self.tab = index + 1,
            None => self.error("no merged timeline yet: :merge builds one"),
        }
    }

    /// Replaces everything with a saved session. Files that can't be opened any more are
    /// skipped (and reported) rather than failing the whole load.
    pub fn load_session(&mut self, name: &str) -> Result<()> {
        let session = Session::load(name)?;

        let mut sources: Vec<Source> = Vec::new();
        let mut problems = Vec::new();
        // Where each saved source ended up, for merged timelines to find their members. They
        // are built last, because their members must exist first.
        let mut new_position: HashMap<usize, usize> = HashMap::new();
        let mut merges: Vec<Vec<usize>> = Vec::new();

        for (saved, state) in session.sources.iter().enumerate() {
            if let Some(members) = state.path.strip_prefix("merge:") {
                merges.push(
                    members
                        .split(',')
                        .filter_map(|m| m.trim().parse().ok())
                        .collect(),
                );
                continue;
            }
            if sources.len() >= MAX_SOURCES {
                problems.push(format!(
                    "more than {MAX_SOURCES} sources, skipped {}",
                    state.path
                ));
                continue;
            }
            let duplicate = Source::identity_of(&state.path)
                .is_ok_and(|identity| sources.iter().any(|source| source.identity() == identity));
            if duplicate {
                problems.push(format!("{} is in the session twice, skipped", state.path));
                continue;
            }
            match Source::from_state(state) {
                Ok(source) => {
                    new_position.insert(saved, sources.len());
                    sources.push(source);
                }
                Err(err) => problems.push(format!("{err:#}")),
            }
        }
        for members in merges {
            let parts: Vec<&Source> = members
                .iter()
                .filter_map(|saved| new_position.get(saved))
                .map(|&position| &sources[position])
                .collect();
            if parts.len() < 2 {
                problems.push("a merged timeline lost its sources".to_string());
                continue;
            }
            let merged = Source::merged(&parts);
            sources.push(merged);
        }

        // Assigning drops the old sources; their threads stop by themselves.
        self.sources = sources;
        self.tab = session.tab.min(self.sources.len());
        self.selected = session.selected.min(self.sources.len().saturating_sub(1));
        self.session_name = Some(name.to_string());

        match problems.first() {
            None => self.info(format!("loaded session {name}")),
            Some(first) => {
                self.error(format!(
                    "loaded {name}, skipped {}: {first}",
                    problems.len()
                ));
            }
        }
        Ok(())
    }

    /// Reads the configuration file: colors and named sources. A broken file changes nothing
    /// (the previous colors stay), so a typo never leaves you without a screen.
    pub fn apply_config(&mut self) -> Result<()> {
        let config = config::load()?;
        self.theme = Theme::from_config(&config)?;
        self.named_sources = config.sources;
        Ok(())
    }

    pub(super) fn snapshot(&self) -> Session {
        let sources = self
            .sources
            .iter()
            .map(|source| {
                let mut state = source.to_state();
                if source.is_merged() {
                    // A merged timeline is stored as the positions of its members in this list
                    // (ids don't survive a restart).
                    let positions: Vec<String> = source
                        .members()
                        .iter()
                        .filter_map(|id| self.sources.iter().position(|s| s.id() == *id))
                        .map(|position| position.to_string())
                        .collect();
                    state.path = format!("merge:{}", positions.join(","));
                }
                state
            })
            .collect();
        Session {
            tab: self.tab,
            selected: self.selected,
            sources,
        }
    }
}
