//! Where a log comes from: a file, or a command whose output is the log.
//!
//! The text form is what you type, put in a session file or in `config.toml`:
//!
//! ```text
//! /var/log/app.log                     a file
//! ssh:prod-1:/var/log/app.log          `tail -F` on a remote host, over ssh
//! docker:api                           `docker logs -f` of a container
//! kube:pod-7                           `kubectl logs -f` (namespace/pod, namespace/pod/container)
//! cmd:journalctl -fu app               any command
//! -                                    standard input (`some-command | tilog`)
//! ```

use std::time::Duration;

use anyhow::{Result, bail};

#[derive(Debug, Clone, PartialEq)]
pub enum SourceSpec {
    File(String),
    Command(CommandSpec),
    /// Several other sources interleaved by time. It has no text form: it is made by `:merge`.
    Merged,
    /// Standard input, written `-`: `kubectl logs -f pod | tilog`.
    Stdin,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandSpec {
    kind: Kind,
}

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Ssh {
        host: String,
        path: String,
    },
    Docker {
        container: String,
    },
    Kube {
        namespace: Option<String>,
        pod: String,
        container: Option<String>,
    },
    Raw {
        command: String,
        merge_stderr: bool,
    },
}

/// When a command that ended is started again.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Restart {
    /// Whatever the exit status: a dropped connection and a finished stream both come back.
    Always,
    /// Only after a failure. A command that finishes successfully is simply done.
    OnFailure,
    /// Never: whatever it did, it is done. A file that is read once has nothing to wait for.
    Never,
}

impl SourceSpec {
    /// Anything that isn't `ssh:`, `docker:`, `kube:`/`k8s:` or `cmd:` is a file path (or a
    /// compressed one, `.gz`).
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        if text == "-" {
            return Ok(Self::Stdin);
        }
        if let Some((scheme, rest)) = text.split_once(':') {
            let kind = match scheme {
                "ssh" => parse_ssh(rest)?,
                "docker" => Kind::Docker {
                    container: safe_name(rest, "docker")?.to_string(),
                },
                "kube" | "k8s" => parse_kube(rest)?,
                "cmd" => parse_raw(rest)?,
                _ => return Ok(Self::File(text.to_string())),
            };
            return Ok(Self::Command(CommandSpec { kind }));
        }
        Ok(Self::File(text.to_string()))
    }

    /// The text `parse` turns back into this spec.
    pub fn describe(&self) -> String {
        match self {
            Self::File(path) => path.clone(),
            Self::Command(command) => command.describe(),
            Self::Merged => "merge:".to_string(),
            Self::Stdin => "-".to_string(),
        }
    }
}

fn parse_ssh(rest: &str) -> Result<Kind> {
    let Some((host, path)) = rest.split_once(':') else {
        bail!("ssh needs a host and a path: ssh:host:/path/to/log");
    };
    if path.is_empty() {
        bail!("ssh needs a path: ssh:{host}:/path/to/log");
    }
    Ok(Kind::Ssh {
        host: safe_name(host, "ssh host")?.to_string(),
        path: path.to_string(),
    })
}

fn parse_kube(rest: &str) -> Result<Kind> {
    let parts: Vec<&str> = rest.split('/').collect();
    let parts = parts
        .iter()
        .map(|part| safe_name(part, "kube"))
        .collect::<Result<Vec<_>>>()?;
    Ok(match parts[..] {
        [pod] => Kind::Kube {
            namespace: None,
            pod: pod.into(),
            container: None,
        },
        [namespace, pod] => Kind::Kube {
            namespace: Some(namespace.into()),
            pod: pod.into(),
            container: None,
        },
        [namespace, pod, container] => Kind::Kube {
            namespace: Some(namespace.into()),
            pod: pod.into(),
            container: Some(container.into()),
        },
        _ => bail!("kube takes pod, namespace/pod or namespace/pod/container"),
    })
}

fn parse_raw(rest: &str) -> Result<Kind> {
    if rest.trim().is_empty() {
        bail!("cmd needs a command: cmd:journalctl -fu app");
    }
    Ok(Kind::Raw {
        command: rest.trim().to_string(),
        merge_stderr: false,
    })
}

/// A host, container or pod name. It ends up as a command-line argument, so one starting with
/// `-` would be read as an *option* (`ssh:-oProxyCommand=...`). Session and config files can come
/// from other people, so this is refused rather than trusted.
fn safe_name<'a>(name: &'a str, what: &str) -> Result<&'a str> {
    if name.is_empty() {
        bail!("{what}: a name is missing");
    }
    if name.starts_with('-') || name.chars().any(char::is_whitespace) {
        bail!("{what}: \"{name}\" is not a valid name");
    }
    Ok(name)
}

impl CommandSpec {
    /// A short label for tabs and tiles.
    pub fn name(&self) -> String {
        match &self.kind {
            Kind::Ssh { host, path } => {
                format!(
                    "{host}:{}",
                    path.rsplit('/')
                        .next()
                        .filter(|s| !s.is_empty())
                        .unwrap_or(path)
                )
            }
            Kind::Docker { container } => container.clone(),
            Kind::Kube { pod, .. } => pod.clone(),
            Kind::Raw { command, .. } => command
                .split_whitespace()
                .next()
                .unwrap_or("command")
                .to_string(),
        }
    }

    pub fn describe(&self) -> String {
        match &self.kind {
            Kind::Ssh { host, path } => format!("ssh:{host}:{path}"),
            Kind::Docker { container } => format!("docker:{container}"),
            Kind::Kube {
                namespace,
                pod,
                container,
            } => {
                let parts: Vec<&str> = [namespace.as_deref(), Some(pod), container.as_deref()]
                    .into_iter()
                    .flatten()
                    .collect();
                format!("kube:{}", parts.join("/"))
            }
            Kind::Raw { command, .. } => format!("cmd:{command}"),
        }
    }

    /// The program and its arguments.
    ///
    /// The first start asks for the last `initial_lines` lines. A *restart* passes `since`, how
    /// long the connection was gone, so docker and kubectl replay only what was missed (plus a
    /// small margin, so a few lines may repeat but none are lost). ssh and plain commands can't
    /// do that, so after a restart they continue from now and the gap stays a gap.
    #[cfg(test)]
    pub fn argv(&self, initial_lines: usize, since: Option<Duration>) -> Vec<String> {
        self.argv_with(initial_lines, since, false)
    }

    /// Like `argv`. With `asking`, an ssh login may ask questions (a password, a host key)
    /// instead of failing: the caller must have a way to answer them (see `askpass`).
    pub fn argv_with(
        &self,
        initial_lines: usize,
        since: Option<Duration>,
        asking: bool,
    ) -> Vec<String> {
        let words = |items: &[&str]| items.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        match &self.kind {
            Kind::Ssh { host, path } => {
                let lines = if since.is_some() { 0 } else { initial_lines };
                ssh_argv(host, asking, remote_tail_command(path, lines))
            }
            Kind::Docker { container } => {
                let mut argv = words(&["docker", "logs", "-f"]);
                match since {
                    Some(gone) => {
                        argv.extend(["--since".to_string(), format!("{}s", gone.as_secs() + 1)])
                    }
                    None => argv.extend(["--tail".to_string(), initial_lines.to_string()]),
                }
                argv.push(container.clone());
                argv
            }
            Kind::Kube {
                namespace,
                pod,
                container,
            } => {
                let mut argv = words(&["kubectl", "logs", "-f"]);
                if let Some(namespace) = namespace {
                    argv.extend(["-n".to_string(), namespace.clone()]);
                }
                if let Some(container) = container {
                    argv.extend(["-c".to_string(), container.clone()]);
                }
                argv.push(match since {
                    Some(gone) => format!("--since={}s", gone.as_secs() + 1),
                    None => format!("--tail={initial_lines}"),
                });
                argv.push(pod.clone());
                argv
            }
            Kind::Raw { command, .. } => words(&["sh", "-c", command]),
        }
    }

    /// `docker logs` sends what the *container* wrote to stderr to its own stderr, and many apps
    /// log there. So for docker the two streams are merged. For the rest, stderr carries the
    /// tool's own errors ("Permission denied"), which are shown as status, not as log lines.
    pub fn merges_stderr(&self) -> bool {
        matches!(
            self.kind,
            Kind::Docker { .. }
                | Kind::Raw {
                    merge_stderr: true,
                    ..
                }
        )
    }

    /// The host of an ssh source.
    pub fn ssh_host(&self) -> Option<&str> {
        match &self.kind {
            Kind::Ssh { host, .. } => Some(host),
            _ => None,
        }
    }

    /// Keep the command's stdin open (and empty) while it runs. See `remote_tail_command`.
    pub fn keeps_stdin_open(&self) -> bool {
        matches!(self.kind, Kind::Ssh { .. })
    }

    pub fn restart(&self) -> Restart {
        match self.kind {
            Kind::Raw { .. } => Restart::OnFailure,
            _ => Restart::Always,
        }
    }

    #[cfg(test)]
    pub fn raw_for_test(command: &str, merge_stderr: bool) -> Self {
        Self {
            kind: Kind::Raw {
                command: command.to_string(),
                merge_stderr,
            },
        }
    }
}

/// The command run on the remote host. A bare `tail -F` would keep running there after the
/// connection is gone (without a terminal nothing tells it to stop until it next writes).
/// So `tail` is started in the background and a `cat` waits on stdin: when the connection
/// closes, stdin ends, `cat` returns and the `tail` is killed.
/// `ssh` with the options every source of ours uses, running `remote_command` on `host`.
///
/// `-T`: no terminal. `BatchMode`: never ask for a password or a passphrase on the terminal the
/// screen is drawn on (with `asking`, the question goes to tilog instead: see `askpass`). The
/// keepalives make a dead connection fail in about 45 s instead of hanging.
fn ssh_argv(host: &str, asking: bool, remote_command: String) -> Vec<String> {
    let batch = if asking {
        "BatchMode=no"
    } else {
        "BatchMode=yes"
    };
    let mut argv: Vec<String> = [
        "ssh",
        "-T",
        "-o",
        batch,
        "-o",
        "ConnectTimeout=10",
        "-o",
        "ServerAliveInterval=15",
        "-o",
        "ServerAliveCountMax=3",
    ]
    .iter()
    .map(|word| (*word).to_string())
    .collect();
    argv.push(host.to_string());
    argv.push(remote_command);
    argv
}

fn remote_tail_command(path: &str, lines: usize) -> String {
    let script = format!(
        "tail -n {lines} -F {} & p=$!; cat >/dev/null; kill $p",
        shell_quote(path)
    );
    format!("sh -c {}", shell_quote(&script))
}

/// Quotes `text` for a POSIX shell: inside single quotes nothing is special, except the single
/// quote itself, which is written as `'\''`.
fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(text: &str) -> CommandSpec {
        match SourceSpec::parse(text).unwrap() {
            SourceSpec::Command(command) => command,
            SourceSpec::File(_) | SourceSpec::Merged | SourceSpec::Stdin => {
                panic!("{text} should be a command")
            }
        }
    }

    #[test]
    fn plain_text_is_a_file() {
        assert_eq!(
            SourceSpec::parse("/var/log/app.log").unwrap(),
            SourceSpec::File("/var/log/app.log".into())
        );
        assert_eq!(
            SourceSpec::parse("logs/a:b.log").unwrap(),
            SourceSpec::File("logs/a:b.log".into())
        );
        assert_eq!(
            SourceSpec::parse("C:/x").unwrap(),
            SourceSpec::File("C:/x".into())
        );
    }

    #[test]
    fn parses_and_describes_every_kind() {
        for text in [
            "ssh:me@prod-1:/var/log/app.log",
            "docker:api",
            "kube:pod-7",
            "kube:shop/pod-7",
            "kube:shop/pod-7/sidecar",
            "cmd:journalctl -fu app",
        ] {
            assert_eq!(SourceSpec::parse(text).unwrap().describe(), text);
        }
        assert_eq!(
            command("k8s:pod-7").describe(),
            "kube:pod-7",
            "k8s is an alias"
        );
    }

    #[test]
    fn rejects_bad_input() {
        for bad in [
            "ssh:host",                // no path
            "ssh::/x",                 // no host
            "ssh:-oProxyCommand=x:/p", // an option in disguise
            "docker:",
            "docker:--privileged",
            "kube:a/b/c/d",
            "kube:ns//pod",
            "cmd:  ",
        ] {
            assert!(
                SourceSpec::parse(bad).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn labels() {
        assert_eq!(
            command("ssh:prod-1:/var/log/app.log").name(),
            "prod-1:app.log"
        );
        assert_eq!(command("docker:api").name(), "api");
        assert_eq!(command("kube:shop/pod-7/side").name(), "pod-7");
        assert_eq!(command("cmd:journalctl -fu app").name(), "journalctl");
    }

    #[test]
    fn builds_the_commands() {
        let docker = command("docker:api");
        assert_eq!(
            docker.argv(500, None),
            ["docker", "logs", "-f", "--tail", "500", "api"]
        );
        assert_eq!(
            docker.argv(500, Some(Duration::from_secs(7))),
            ["docker", "logs", "-f", "--since", "8s", "api"]
        );

        let kube = command("kube:shop/pod-7/side");
        assert_eq!(
            kube.argv(500, None),
            [
                "kubectl",
                "logs",
                "-f",
                "-n",
                "shop",
                "-c",
                "side",
                "--tail=500",
                "pod-7"
            ]
        );
        assert_eq!(
            command("kube:pod-7").argv(1, Some(Duration::from_secs(0))),
            ["kubectl", "logs", "-f", "--since=1s", "pod-7"]
        );

        assert_eq!(
            command("cmd:echo a | wc").argv(1, None),
            ["sh", "-c", "echo a | wc"]
        );
    }

    #[test]
    fn ssh_never_prompts_and_cleans_up_the_remote_tail() {
        let argv = command("ssh:prod-1:/var/log/my app.log").argv(1000, None);
        assert_eq!(argv[0], "ssh");
        assert!(argv.windows(2).any(|pair| pair == ["-o", "BatchMode=yes"]));
        assert_eq!(argv[argv.len() - 2], "prod-1");
        // The path with a space survives two levels of quoting.
        assert_eq!(
            argv.last().unwrap(),
            r#"sh -c 'tail -n 1000 -F '\''/var/log/my app.log'\'' & p=$!; cat >/dev/null; kill $p'"#
        );
        // After a reconnect it continues from now instead of repeating 1000 lines.
        let again = command("ssh:prod-1:/x").argv(1000, Some(Duration::from_secs(5)));
        assert!(again.last().unwrap().contains("tail -n 0 -F"));
    }

    #[test]
    fn shell_quoting() {
        assert_eq!(shell_quote("plain"), "'plain'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(shell_quote("$(rm -rf /)"), "'$(rm -rf /)'");
    }

    #[test]
    fn behaviour_flags() {
        assert!(command("docker:api").merges_stderr());
        assert!(!command("kube:p").merges_stderr());
        assert!(command("ssh:h:/p").keeps_stdin_open());
        assert!(!command("docker:api").keeps_stdin_open());
        assert_eq!(command("ssh:h:/p").restart(), Restart::Always);
        assert_eq!(command("cmd:true").restart(), Restart::OnFailure);
    }

    #[test]
    fn a_dash_is_standard_input() {
        assert_eq!(SourceSpec::parse("-").unwrap(), SourceSpec::Stdin);
        assert_eq!(SourceSpec::parse(" - ").unwrap(), SourceSpec::Stdin);
        assert_eq!(SourceSpec::Stdin.describe(), "-");
        // Only the lone dash: a file can still have a name that starts with one.
        assert_eq!(
            SourceSpec::parse("-x.log").unwrap(),
            SourceSpec::File("-x.log".into())
        );
        assert_eq!(
            SourceSpec::parse("./-").unwrap(),
            SourceSpec::File("./-".into())
        );
    }
}
