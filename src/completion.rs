//! Tab completion for command arguments, shell style: complete as far as the choices agree,
//! and report the choices when several remain.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// The result of pressing Tab on a partial argument.
pub struct Completion {
    /// The argument with as much completed as is unambiguous.
    pub text: String,
    /// Every name that matched (directories end with `/`).
    pub candidates: Vec<String>,
}

/// Completes a file path: `logs/ap` becomes `logs/app.log`, `/var/l` becomes `/var/log/`.
/// `None` if nothing matches.
pub fn complete_path(partial: &str) -> Option<Completion> {
    if partial == "~" {
        return Some(Completion {
            text: "~/".into(),
            candidates: Vec::new(),
        });
    }

    // Split at the last slash: `dir_part` is what's already settled, `prefix` is being typed.
    let (dir_part, prefix) = match partial.rfind('/') {
        Some(slash) => partial.split_at(slash + 1),
        None => ("", partial),
    };

    let mut names: Vec<String> = fs::read_dir(expand_home(dir_part))
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let hidden_unasked = name.starts_with('.') && !prefix.starts_with('.');
            if !name.starts_with(prefix) || hidden_unasked {
                return None;
            }
            let suffix = if entry.path().is_dir() { "/" } else { "" };
            Some(format!("{name}{suffix}"))
        })
        .collect();
    if names.is_empty() {
        return None;
    }
    names.sort();

    Some(Completion {
        text: format!("{dir_part}{}", common_prefix(&names)),
        candidates: names,
    })
}

/// Completes one of a fixed set of names (such as saved sessions).
pub fn complete_from(partial: &str, names: &[String]) -> Option<Completion> {
    let mut matching: Vec<String> = names
        .iter()
        .filter(|name| name.starts_with(partial))
        .cloned()
        .collect();
    if matching.is_empty() {
        return None;
    }
    matching.sort();
    Some(Completion {
        text: common_prefix(&matching),
        candidates: matching,
    })
}

/// Completes the host of `ssh:host:/path`, from the names in `hosts` (see `ssh_hosts`).
/// `partial` is what follows `ssh:`, and may start with a user: `deploy@pro`.
///
/// When exactly one host fits, the `:` that separates it from the path is added too.
pub fn complete_ssh(partial: &str, hosts: &[String]) -> Option<Completion> {
    let (user, typed) = match partial.rfind('@') {
        Some(at) => partial.split_at(at + 1),
        None => ("", partial),
    };
    // ssh treats host names as case-insensitive, so matching is too.
    let typed = typed.to_lowercase();
    let mut matching: Vec<String> = hosts
        .iter()
        .filter(|host| host.to_lowercase().starts_with(&typed))
        .cloned()
        .collect();
    if matching.is_empty() {
        return None;
    }
    matching.sort();
    let colon = if matching.len() == 1 { ":" } else { "" };
    Some(Completion {
        text: format!("{user}{}{colon}", common_prefix(&matching)),
        candidates: matching,
    })
}

/// The host names a user has set up in `~/.ssh/config`, including files it `Include`s.
/// Patterns such as `Host *.example.com` are not names and are skipped. Empty if there is no
/// config.
pub fn ssh_hosts() -> Vec<String> {
    env::var_os("HOME")
        .map(|home| ssh_hosts_in(Path::new(&home)))
        .unwrap_or_default()
}

fn ssh_hosts_in(home: &Path) -> Vec<String> {
    let ssh_dir = home.join(".ssh");
    let mut hosts = Vec::new();
    read_ssh_config(&ssh_dir.join("config"), &ssh_dir, home, 0, &mut hosts);
    hosts.sort();
    hosts.dedup();
    hosts
}

fn read_ssh_config(
    file: &Path,
    ssh_dir: &Path,
    home: &Path,
    depth: usize,
    hosts: &mut Vec<String>,
) {
    // Include files may include each other; a loop must not run forever.
    if depth > 4 {
        return;
    }
    let Ok(text) = fs::read_to_string(file) else {
        return;
    };
    let config = parse_ssh_config(&text);
    hosts.extend(config.hosts);
    for pattern in config.includes {
        for included in expand_include(&pattern, ssh_dir, home) {
            read_ssh_config(&included, ssh_dir, home, depth + 1, hosts);
        }
    }
}

struct SshConfig {
    hosts: Vec<String>,
    includes: Vec<String>,
}

/// The `Host` names and `Include` patterns of one config file's text.
fn parse_ssh_config(text: &str) -> SshConfig {
    let mut config = SshConfig {
        hosts: Vec::new(),
        includes: Vec::new(),
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // "Keyword value" or "Keyword=value".
        let is_separator = |c: char| c.is_whitespace() || c == '=';
        let Some((keyword, rest)) = line.split_once(is_separator) else {
            continue;
        };
        let rest = rest.trim_start_matches(is_separator);

        match keyword.to_ascii_lowercase().as_str() {
            "host" => {
                for alias in rest.split_whitespace() {
                    if !alias.contains(['*', '?', '!']) {
                        config.hosts.push(alias.trim_matches('"').to_string());
                    }
                }
            }
            "include" => config
                .includes
                .extend(rest.split_whitespace().map(str::to_string)),
            _ => {}
        }
    }
    config
}

/// The files an `Include` line refers to. Relative paths are relative to `~/.ssh`, and `*` may
/// appear in the file name (`Include config.d/*`).
fn expand_include(pattern: &str, ssh_dir: &Path, home: &Path) -> Vec<PathBuf> {
    let path = match pattern.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if Path::new(pattern).is_absolute() => PathBuf::from(pattern),
        None => ssh_dir.join(pattern),
    };
    let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
        return Vec::new();
    };
    if !name.contains('*') {
        return vec![path];
    }
    let mut found: Vec<PathBuf> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|n| wildcard_matches(name, n))
        })
        .map(|entry| entry.path())
        .collect();
    found.sort();
    found
}

/// Does `name` fit `pattern`, where `*` stands for any text?
fn wildcard_matches(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((head, rest)) => name.strip_prefix(head).is_some_and(|tail| {
            (0..=tail.len())
                .filter(|&i| tail.is_char_boundary(i))
                .any(|i| wildcard_matches(rest, &tail[i..]))
        }),
    }
}

/// `""` and `"~/x"` need the home directory spelled out before the file system understands them.
pub(crate) fn expand_home(dir: &str) -> PathBuf {
    if dir.is_empty() {
        return PathBuf::from(".");
    }
    match (dir.strip_prefix("~/"), env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(dir),
    }
}

/// The longest string all `names` start with.
fn common_prefix(names: &[String]) -> String {
    let Some(first) = names.first() else {
        return String::new();
    };
    let mut len = first.len();
    for name in &names[1..] {
        // Compare char by char so a multi-byte character is never cut in half.
        let shared: usize = first
            .chars()
            .zip(name.chars())
            .take_while(|(a, b)| a == b)
            .map(|(a, _)| a.len_utf8())
            .sum();
        len = len.min(shared);
    }
    first[..len].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn common_prefix_of_names() {
        assert_eq!(common_prefix(&names(&["alpha.log", "alpine.log"])), "alp");
        assert_eq!(common_prefix(&names(&["only"])), "only");
        assert_eq!(common_prefix(&names(&["äb", "äc"])), "ä");
        assert_eq!(common_prefix(&names(&["a", "b"])), "");
        assert_eq!(common_prefix(&[]), "");
    }

    #[test]
    fn completes_from_a_fixed_set() {
        let sessions = names(&["prod", "prod-eu", "staging"]);
        let done = complete_from("pr", &sessions).unwrap();
        assert_eq!((done.text.as_str(), done.candidates.len()), ("prod", 2));
        assert_eq!(complete_from("s", &sessions).unwrap().text, "staging");
        assert!(complete_from("x", &sessions).is_none());
    }

    const SSH_CONFIG: &str = "# my servers
Host prod-1 prod-2
    HostName 10.0.0.1
    User deploy

host=staging
    HostName staging.example.com

Host *.internal bastion-* !secret
    ProxyJump jump

Host \"quoted\"
";

    #[test]
    fn reads_host_names_and_includes_from_an_ssh_config() {
        let config = parse_ssh_config(&format!(
            "{SSH_CONFIG}Include config.d/*\nINCLUDE ~/extra\n"
        ));
        assert_eq!(
            config.hosts,
            ["prod-1", "prod-2", "staging", "quoted"],
            "wildcards are skipped"
        );
        assert_eq!(config.includes, ["config.d/*", "~/extra"]);
    }

    #[test]
    fn follows_include_files_and_survives_a_loop() {
        let home = env::temp_dir().join(format!("tilog-ssh-test-{}", std::process::id()));
        let ssh = home.join(".ssh");
        fs::create_dir_all(ssh.join("config.d")).unwrap();
        fs::write(
            ssh.join("config"),
            "Host main\nInclude config.d/*.conf\nInclude config\n",
        )
        .unwrap();
        fs::write(ssh.join("config.d/a.conf"), "Host from-a\n").unwrap();
        fs::write(ssh.join("config.d/b.conf"), "Host from-b main\n").unwrap();
        fs::write(ssh.join("config.d/ignored.txt"), "Host not-included\n").unwrap();

        // "Include config" includes the file itself: the depth limit stops it.
        assert_eq!(ssh_hosts_in(&home), ["from-a", "from-b", "main"]);
        assert!(
            ssh_hosts_in(&home.join("nowhere")).is_empty(),
            "no config is fine"
        );
        fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn matches_wildcards_in_include_names() {
        assert!(wildcard_matches("*.conf", "a.conf"));
        assert!(wildcard_matches("*", "anything"));
        assert!(wildcard_matches("a*c*e", "abcde"));
        assert!(!wildcard_matches("*.conf", "a.txt"));
        assert!(!wildcard_matches("a*", "ba"));
    }

    #[test]
    fn completes_ssh_hosts_with_or_without_a_user() {
        let hosts: Vec<String> = ["prod-1", "prod-2", "Staging", "bastion"]
            .map(String::from)
            .to_vec();

        let ambiguous = complete_ssh("pr", &hosts).unwrap();
        assert_eq!(ambiguous.text, "prod-");
        assert_eq!(ambiguous.candidates, ["prod-1", "prod-2"]);

        // One match: finished, and the colon for the path is added.
        assert_eq!(
            complete_ssh("deploy@ba", &hosts).unwrap().text,
            "deploy@bastion:"
        );
        // Case does not matter for matching; the host keeps its own spelling.
        assert_eq!(complete_ssh("stag", &hosts).unwrap().text, "Staging:");
        // Empty: everything.
        assert_eq!(complete_ssh("", &hosts).unwrap().candidates.len(), 4);
        assert!(complete_ssh("zzz", &hosts).is_none());
    }

    #[test]
    fn completes_paths() {
        let dir = env::temp_dir().join(format!("tilog-completion-test-{}", std::process::id()));
        fs::create_dir_all(dir.join("logs")).unwrap();
        for file in ["alpha.log", "alpine.log", "beta.log", ".hidden.log"] {
            fs::write(dir.join(file), "").unwrap();
        }
        let base = dir.to_str().unwrap();

        // Ambiguous: completes the shared part ("alp") and lists both.
        let done = complete_path(&format!("{base}/al")).unwrap();
        assert_eq!(done.text, format!("{base}/alp"));
        assert_eq!(done.candidates, ["alpha.log", "alpine.log"]);

        // Unique file; unique directory gets a trailing slash.
        assert_eq!(
            complete_path(&format!("{base}/b")).unwrap().text,
            format!("{base}/beta.log")
        );
        assert_eq!(
            complete_path(&format!("{base}/lo")).unwrap().text,
            format!("{base}/logs/")
        );

        // Hidden files only when asked for; no match gives None.
        assert_eq!(
            complete_path(&format!("{base}/")).unwrap().candidates.len(),
            4
        );
        assert_eq!(
            complete_path(&format!("{base}/.h")).unwrap().text,
            format!("{base}/.hidden.log")
        );
        assert!(complete_path(&format!("{base}/zzz")).is_none());

        fs::remove_dir_all(&dir).unwrap();
    }
}
