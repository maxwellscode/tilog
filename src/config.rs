//! The configuration file: `~/.config/tilog/config.toml`.
//!
//! The built-in color rules are a TOML file too (`default_config.toml`, compiled into the
//! binary). It is read by the same code as the user's file, so there is one format and one
//! parser, and `tilog --print-config` can print a complete, working example.

use std::env;
use std::fs;
use std::io::ErrorKind;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::Deserialize;

/// The built-in configuration, as text.
pub const DEFAULT_CONFIG: &str = include_str!("default_config.toml");

// `deny_unknown_fields` turns a typo such as `[[rules]]` or `foreground = "red"` into an
// error that names the key, instead of silently ignoring it.

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub colors: Colors,
    #[serde(default, rename = "rule")]
    pub rules: Vec<RuleConfig>,
    #[serde(default, rename = "source")]
    pub sources: Vec<NamedSource>,
}

/// A source with a name, so `tilog prod-api` or `/add prod-api` opens it. Sharing this file
/// shares the list of places to look at.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NamedSource {
    pub name: String,
    /// Where it comes from: a path, `ssh:host:/path`, `docker:name`, `kube:pod`, `cmd:...`.
    pub spec: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Colors {
    /// Color the log lines at all.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Start from tilog's built-in rules; the file's own rules come after them.
    #[serde(default = "yes")]
    pub defaults: bool,
    pub highlight_fg: Option<String>,
    pub highlight_bg: Option<String>,
    /// Background of the entry `n` / `N` brought you to.
    pub marked_bg: Option<String>,
}

impl Default for Colors {
    fn default() -> Self {
        Self {
            enabled: true,
            defaults: true,
            highlight_fg: None,
            highlight_bg: None,
            marked_bg: None,
        }
    }
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuleConfig {
    pub name: Option<String>,
    pub pattern: String,
    pub fg: Option<String>,
    pub bg: Option<String>,
    #[serde(default)]
    pub bold: bool,
    #[serde(default)]
    pub italic: bool,
    #[serde(default)]
    pub dim: bool,
    #[serde(default)]
    pub underline: bool,
    #[serde(default)]
    pub scope: Scope,
}

/// What a rule colors when its pattern matches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Just the matched text.
    #[default]
    Match,
    /// The whole line.
    Line,
}

impl Config {
    pub fn parse(text: &str) -> Result<Self> {
        toml::from_str(text).context("invalid configuration")
    }

    /// The rules to apply, in order: the built-in ones (unless switched off), then the file's.
    pub fn rules_in_effect(&self) -> Vec<RuleConfig> {
        let mut rules = if self.colors.defaults {
            builtin_rules()
        } else {
            Vec::new()
        };
        rules.extend(self.rules.iter().cloned());
        rules
    }
}

fn builtin_rules() -> Vec<RuleConfig> {
    Config::parse(DEFAULT_CONFIG)
        .expect("the built-in configuration is valid")
        .rules
}

/// `$XDG_CONFIG_HOME/tilog`, or `~/.config/tilog`.
pub fn dir() -> Result<PathBuf> {
    let base = env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .context("cannot find a config directory (HOME is not set)")?;
    Ok(base.join("tilog"))
}

/// Reads the user's configuration. No file simply means the defaults.
pub fn load() -> Result<Config> {
    let path = dir()?.join("config.toml");
    match fs::read_to_string(&path) {
        Ok(text) => Config::parse(&text).with_context(|| path.display().to_string()),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| format!("cannot read {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_built_in_configuration_parses_and_is_complete() {
        let config = Config::parse(DEFAULT_CONFIG).unwrap();
        assert!(config.colors.enabled);
        assert!(
            !config.colors.defaults,
            "the printed file must not double up when used as is"
        );
        assert!(config.rules.len() >= 8);
        assert!(config.rules.iter().any(|rule| rule.scope == Scope::Line));
    }

    #[test]
    fn an_empty_file_means_defaults() {
        let config = Config::parse("").unwrap();
        assert!(config.colors.enabled && config.colors.defaults);
        assert_eq!(config.rules_in_effect().len(), builtin_rules().len());
    }

    #[test]
    fn user_rules_come_after_the_built_in_ones() {
        let config = Config::parse("[[rule]]\npattern = 'x'\nfg = \"red\"\n").unwrap();
        let rules = config.rules_in_effect();
        assert_eq!(rules.len(), builtin_rules().len() + 1);
        assert_eq!(rules.last().unwrap().pattern, "x");

        let only_mine =
            Config::parse("[colors]\ndefaults = false\n[[rule]]\npattern = 'x'\n").unwrap();
        assert_eq!(only_mine.rules_in_effect().len(), 1);
    }

    #[test]
    fn named_sources() {
        let text = "[[source]]\nname = \"api\"\nspec = \"ssh:prod-1:/var/log/api.log\"\n\n[[source]]\nname = \"db\"\nspec = \"docker:postgres\"\n";
        let config = Config::parse(text).unwrap();
        assert_eq!(config.sources.len(), 2);
        assert_eq!(
            (
                config.sources[1].name.as_str(),
                config.sources[1].spec.as_str()
            ),
            ("db", "docker:postgres")
        );
        assert!(
            Config::parse("[[source]]\nname = \"x\"\n").is_err(),
            "a source needs a spec"
        );
    }

    #[test]
    fn typos_are_reported_not_ignored() {
        assert!(Config::parse("[[rules]]\npattern = 'x'\n").is_err());
        assert!(Config::parse("[[rule]]\npattern = 'x'\nforeground = \"red\"\n").is_err());
        assert!(
            Config::parse("[[rule]]\nfg = \"red\"\n").is_err(),
            "a rule needs a pattern"
        );
        assert!(Config::parse("[[rule]]\npattern = 'x'\nscope = \"word\"\n").is_err());
    }
}
