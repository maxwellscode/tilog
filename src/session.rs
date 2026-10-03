//! Saving and loading the layout, like tmux sessions: which files are open, which filter tiles
//! exist, and which tab is showing. Stored as TOML, one file per session.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::config;
use crate::filter::FilterSpec;

// `Serialize` / `Deserialize` are traits that `#[derive]` implements for us: serde walks the
// struct field by field. No hand-written parsing code, and the file format is the struct.

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub tab: usize,
    pub selected: usize,
    pub sources: Vec<SourceState>,
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct SourceState {
    /// A file path, or `ssh:host:/path`, `docker:name`, `kube:pod`, `cmd:...`.
    pub path: String,
    /// A label chosen in the configuration, if any.
    #[serde(default)]
    pub name: Option<String>,
    /// Clock offset in milliseconds, for merging with other sources.
    #[serde(default)]
    pub offset_ms: i64,
    #[serde(default)]
    pub focus: usize,
    /// How lines are grouped into entries: `auto`, `off`, or a regex (see `GroupRule`).
    #[serde(default = "default_group")]
    pub group: String,
    /// One entry per filter tile, in tile order.
    #[serde(default)]
    pub filters: Vec<FilterTile>,
}

/// Sessions saved before grouping existed have no `group` key; they get the default.
fn default_group() -> String {
    "auto".to_string()
}

/// A filter tile as the chain of conditions it applies (inherited ones first).
#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct FilterTile {
    pub chain: Vec<FilterSpec>,
}

impl Session {
    pub fn save(&self, name: &str) -> Result<PathBuf> {
        self.save_in(&dir()?, name)
    }

    pub fn load(name: &str) -> Result<Self> {
        Self::load_from(&dir()?, name)
    }

    pub fn save_in(&self, dir: &Path, name: &str) -> Result<PathBuf> {
        validate_name(name)?;
        fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        let path = dir.join(format!("{name}.toml"));
        let text = toml::to_string_pretty(self).context("cannot serialize the session")?;
        fs::write(&path, text).with_context(|| format!("cannot write {}", path.display()))?;
        Ok(path)
    }

    pub fn load_from(dir: &Path, name: &str) -> Result<Self> {
        validate_name(name)?;
        let path = dir.join(format!("{name}.toml"));
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => bail!("no session named \"{name}\""),
            Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
        };
        toml::from_str(&text).with_context(|| format!("{} is not a valid session", path.display()))
    }
}

/// `<config dir>/sessions`, i.e. `~/.config/tilog/sessions`.
pub fn dir() -> Result<PathBuf> {
    Ok(config::dir()?.join("sessions"))
}

/// Names of the saved sessions, sorted. No sessions directory yet means none saved.
pub fn list() -> Result<Vec<String>> {
    list_in(&dir()?)
}

pub fn list_in(dir: &Path) -> Result<Vec<String>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", dir.display())),
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .filter_map(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    Ok(names)
}

/// The name becomes a file name, so keep it boring: no slashes, no `..`.
pub fn validate_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !valid {
        bail!("invalid session name \"{name}\" (letters, digits, - _ . only)");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    fn sample() -> Session {
        Session {
            tab: 1,
            selected: 0,
            sources: vec![
                SourceState {
                    path: "/var/log/tomcat/catalina.out".into(),
                    name: None,
                    offset_ms: 7_200_000,
                    focus: 2,
                    group: r"^\d{2}-\w{3}-\d{4}".into(),
                    filters: vec![
                        FilterTile {
                            chain: vec![FilterSpec {
                                pattern: "SEVERE".into(),
                                regex: false,
                                ignore_case: false,
                            }],
                        },
                        FilterTile {
                            chain: vec![
                                FilterSpec {
                                    pattern: "SEVERE".into(),
                                    regex: false,
                                    ignore_case: false,
                                },
                                FilterSpec {
                                    pattern: r"order \d+".into(),
                                    regex: true,
                                    ignore_case: true,
                                },
                            ],
                        },
                    ],
                },
                SourceState {
                    path: "ssh:prod-2:/var/log/nginx/access.log".into(),
                    name: Some("edge".into()),
                    offset_ms: 0,
                    focus: 0,
                    group: "off".into(),
                    filters: vec![],
                },
            ],
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        env::temp_dir().join(format!("tilog-session-{tag}-{}", std::process::id()))
    }

    #[test]
    fn roundtrips_through_a_file() {
        let dir = temp_dir("roundtrip");
        let session = sample();
        let path = session.save_in(&dir, "work").unwrap();
        assert!(path.ends_with("work.toml"));

        assert_eq!(Session::load_from(&dir, "work").unwrap(), session);
        assert_eq!(list_in(&dir).unwrap(), ["work"]);
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn old_sessions_without_a_group_key_still_load() {
        let text = "tab = 0\nselected = 0\n\n[[sources]]\npath = \"/x.log\"\n";
        let session: Session = toml::from_str(text).unwrap();
        assert_eq!(session.sources[0].group, "auto");
        assert!(session.sources[0].filters.is_empty());
    }

    #[test]
    fn missing_session_and_missing_directory() {
        let dir = temp_dir("missing");
        assert!(list_in(&dir).unwrap().is_empty());
        let err = Session::load_from(&dir, "nope").unwrap_err().to_string();
        assert_eq!(err, "no session named \"nope\"");
    }

    #[test]
    fn rejects_names_that_could_escape_the_directory() {
        for bad in ["", "../x", "a/b", ".hidden", "with space"] {
            assert!(validate_name(bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(validate_name("prod-deploy_2.1").is_ok());
    }
}
