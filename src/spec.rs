//! Where a log comes from: a file, or a command whose output is the log.
//!
//! The text form is what you type, put in a session file or in `config.toml`:
//!
//! ```text
//! /var/log/app.log                     a file
//! ssh:prod-1:/var/log/app.log          `tail -F` on a remote host, over ssh
//! ssh:prod-1:docker:api                `docker logs -f` of a container on a remote host
//! ssh:bastion:kube:shop/web-0          `kubectl logs -f` run on a remote host
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
    /// `docker logs` or `kubectl logs` run on another machine, over ssh: `ssh:web1:docker:api`.
    Remote {
        host: String,
        /// What runs there: a `Docker` or a `Kube`.
        inner: Box<Kind>,
    },
    /// A compressed log (`app.log.2.gz`), read with `gzip -dc`. It has an end: it is not
    /// followed and not restarted.
    Gzip {
        path: String,
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
                _ => return Ok(file_or_gzip(text)),
            };
            return Ok(Self::Command(CommandSpec { kind }));
        }
        Ok(file_or_gzip(text))
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

/// A path is a file, unless it is compressed: those are unpacked by `gzip`, because tilog reads
/// plain text.
fn file_or_gzip(path: &str) -> SourceSpec {
    if path.to_ascii_lowercase().ends_with(".gz") {
        return SourceSpec::Command(CommandSpec {
            kind: Kind::Gzip {
                path: path.to_string(),
            },
        });
    }
    SourceSpec::File(path.to_string())
}

fn parse_ssh(rest: &str) -> Result<Kind> {
    let Some((host, path)) = rest.split_once(':') else {
        bail!("ssh needs a host and a path: ssh:host:/path/to/log");
    };
    if path.is_empty() {
        bail!("ssh needs a path: ssh:{host}:/path/to/log");
    }
    let host = safe_name(host, "ssh host")?.to_string();
    // `ssh:host:docker:name` and `ssh:host:kube:pod` read the logs of a container or a pod on
    // that host. (A path always starts with a `/`, or at least not with these.)
    let tool = if let Some(container) = path.strip_prefix("docker:") {
        Some(Kind::Docker {
            container: safe_name(container, "docker")?.to_string(),
        })
    } else if let Some(pod) = path
        .strip_prefix("kube:")
        .or_else(|| path.strip_prefix("k8s:"))
    {
        Some(parse_kube(pod)?)
    } else {
        None
    };
    if let Some(inner) = tool {
        return Ok(Kind::Remote {
            host,
            inner: Box::new(inner),
        });
    }
    Ok(Kind::Ssh {
        host,
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
            Kind::Remote { host, inner } => {
                let inner = CommandSpec {
                    kind: (**inner).clone(),
                };
                format!("{host}:{}", inner.name())
            }
            Kind::Docker { container } => container.clone(),
            Kind::Kube { pod, .. } => pod.clone(),
            Kind::Raw { command, .. } => command
                .split_whitespace()
                .next()
                .unwrap_or("command")
                .to_string(),
            Kind::Gzip { path } => path.rsplit('/').next().unwrap_or(path).to_string(),
        }
    }

    pub fn describe(&self) -> String {
        match &self.kind {
            Kind::Ssh { host, path } => format!("ssh:{host}:{path}"),
            Kind::Remote { host, inner } => {
                let inner = CommandSpec {
                    kind: (**inner).clone(),
                };
                format!("ssh:{host}:{}", inner.describe())
            }
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
            Kind::Gzip { path } => path.clone(),
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
            Kind::Remote { host, inner } => {
                // The same command as on this machine, run there, replaying only what was missed
                // after a reconnect.
                let inner = CommandSpec {
                    kind: (**inner).clone(),
                };
                let command = inner.argv_with(initial_lines, since, false);
                ssh_argv(host, asking, remote_run_command(&command))
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
            Kind::Gzip { path } => words(&["gzip", "-dc", "--", path]),
        }
    }

    /// `docker logs` sends what the *container* wrote to stderr to its own stderr, and many apps
    /// log there. So for docker the two streams are merged. For the rest, stderr carries the
    /// tool's own errors ("Permission denied"), which are shown as status, not as log lines.
    pub fn merges_stderr(&self) -> bool {
        matches!(
            self.kind,
            Kind::Docker { .. }
                | Kind::Remote { .. }
                | Kind::Gzip { .. }
                | Kind::Raw {
                    merge_stderr: true,
                    ..
                }
        )
    }

    /// The host of an ssh source.
    pub fn ssh_host(&self) -> Option<&str> {
        match &self.kind {
            Kind::Ssh { host, .. } | Kind::Remote { host, .. } => Some(host),
            _ => None,
        }
    }

    /// The path of the compressed file, if this reads one.
    pub fn gzip_path(&self) -> Option<&str> {
        match &self.kind {
            Kind::Gzip { path } => Some(path),
            _ => None,
        }
    }

    /// Keep the command's stdin open (and empty) while it runs. See `remote_tail_command`.
    pub fn keeps_stdin_open(&self) -> bool {
        matches!(self.kind, Kind::Ssh { .. } | Kind::Remote { .. })
    }

    pub fn restart(&self) -> Restart {
        match self.kind {
            Kind::Raw { .. } => Restart::OnFailure,
            Kind::Gzip { .. } => Restart::Never,
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

/// The command run on the remote host for `docker logs -f` or `kubectl logs -f`: the command
/// itself, with its errors in its output, and a watcher that stops it when the connection
/// goes. (Without a terminal, nothing else tells a command on the other side that we left.)
/// The shell ends when the command does, with its exit status.
///
/// The watcher's own output goes nowhere: if it kept the output of the shell open, the
/// connection would stay up after the command had ended, and the failure would never be seen.
///
/// The watcher reads the connection through descriptor 3: a command started in the background
/// by a shell without job control gets `/dev/null` as its standard input, which would make the
/// watcher believe the connection was gone at once.
fn remote_run_command(command: &[String]) -> String {
    let words: Vec<String> = command.iter().map(|word| shell_quote(word)).collect();
    let script = format!(
        "exec 3<&0; {} 2>&1 & p=$!; (cat <&3; kill $p 2>/dev/null) >/dev/null 2>&1 & wait $p",
        words.join(" ")
    );
    format!("sh -c {}", shell_quote(&script))
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

    #[test]
    fn a_compressed_file_is_read_with_gzip() {
        let spec = command("/var/log/app.log.2.GZ");
        assert_eq!(
            spec.argv(1000, None),
            ["gzip", "-dc", "--", "/var/log/app.log.2.GZ"]
        );
        assert_eq!(spec.name(), "app.log.2.GZ");
        assert_eq!(spec.gzip_path(), Some("/var/log/app.log.2.GZ"));
        // It has an end, so it is not restarted when it finishes, and the explanation of a
        // failure (not in gzip format) is among its output.
        assert_eq!(spec.restart(), Restart::Never);
        assert!(spec.merges_stderr());
        // The text form comes back as the same thing.
        assert_eq!(
            SourceSpec::parse(&spec.describe()).unwrap(),
            SourceSpec::Command(spec)
        );
        // Plain files, and paths that only look like a scheme, are unchanged.
        assert_eq!(
            SourceSpec::parse("app.log").unwrap(),
            SourceSpec::File("app.log".into())
        );
        assert!(matches!(
            SourceSpec::parse("odd:name.log.gz").unwrap(),
            SourceSpec::Command(_)
        ));
    }

    #[test]
    fn docker_and_kubectl_can_run_on_another_host_over_ssh() {
        let docker = command("ssh:web1:docker:api");
        assert_eq!(docker.name(), "web1:api");
        assert_eq!(docker.describe(), "ssh:web1:docker:api");
        assert_eq!(docker.ssh_host(), Some("web1"));
        assert_eq!(docker.restart(), Restart::Always);
        assert!(docker.merges_stderr() && docker.keeps_stdin_open());
        // The text form comes back as the same thing.
        assert_eq!(
            SourceSpec::parse(&docker.describe()).unwrap(),
            SourceSpec::Command(docker)
        );

        let kube = command("ssh:bastion:kube:shop/web-0/app");
        assert_eq!(kube.name(), "bastion:web-0");
        assert_eq!(kube.describe(), "ssh:bastion:kube:shop/web-0/app");
        assert_eq!(
            command("ssh:bastion:k8s:web-0").describe(),
            "ssh:bastion:kube:web-0"
        );

        // A file path is still a file, whatever it is called.
        assert!(matches!(
            SourceSpec::parse("ssh:web1:/var/log/docker.log").unwrap(),
            SourceSpec::Command(c) if c.name() == "web1:docker.log"
        ));
        for bad in [
            "ssh:web1:docker:",
            "ssh:web1:docker:-x",
            "ssh:web1:kube:",
            "ssh:web1:kube:a/b/c/d",
        ] {
            assert!(SourceSpec::parse(bad).is_err(), "{bad}");
        }
    }

    /// Runs the command an ssh source sends to the other side, on this machine, with a fake
    /// `tool` first in the path that prints the arguments it was started with, one per line.
    fn run_remote(remote: &str, tool: &str) -> Vec<String> {
        use std::io::Read;
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!(
            "tilog-remote-{tool}-{}-{}",
            std::process::id(),
            remote.len()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join(tool);
        std::fs::write(
            &script,
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let path = format!(
            "{}:{}",
            dir.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut child = std::process::Command::new("sh")
            .args(["-c", remote])
            .env("PATH", path)
            .stdin(std::process::Stdio::piped()) // held open, like the ssh connection
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut out = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        drop(child.stdin.take()); // the connection goes
        assert!(child.wait().unwrap().success());
        std::fs::remove_dir_all(&dir).unwrap();
        out.lines().map(str::to_string).collect()
    }

    #[test]
    fn the_remote_command_is_the_local_one_with_its_errors_and_a_watcher() {
        let argv = command("ssh:web1:docker:api").argv(500, None);
        assert_eq!(&argv[..4], ["ssh", "-T", "-o", "BatchMode=yes"]);
        assert_eq!(argv[argv.len() - 2], "web1");
        let remote = argv.last().unwrap();
        assert!(remote.starts_with("sh -c '"), "{remote}");
        // Run for real: docker gets exactly the arguments it would get here, and the shell ends
        // when it does.
        assert_eq!(
            run_remote(remote, "docker"),
            ["logs", "-f", "--tail", "500", "api"]
        );

        // After a reconnect only what was missed is asked for, as for a local container.
        let again = command("ssh:web1:docker:api").argv(500, Some(Duration::from_secs(7)));
        assert_eq!(
            run_remote(again.last().unwrap(), "docker"),
            ["logs", "-f", "--since", "8s", "api"]
        );

        let kube = command("ssh:b:kube:shop/web-0/app").argv(100, None);
        assert_eq!(
            run_remote(kube.last().unwrap(), "kubectl"),
            [
                "logs",
                "-f",
                "-n",
                "shop",
                "-c",
                "app",
                "--tail=100",
                "web-0"
            ]
        );

        // With questions allowed, ssh may ask: the same options otherwise.
        let asking = command("ssh:web1:docker:api").argv_with(500, None, true);
        assert_eq!(&asking[..4], ["ssh", "-T", "-o", "BatchMode=no"]);
    }

    #[test]
    fn a_quote_in_a_name_cannot_break_out_of_the_remote_command() {
        // Names are checked to hold no whitespace, but a quote and a `;` are legal parts of
        // one; they must arrive as part of the name, not as a command of their own.
        let spec = command("ssh:web1:docker:a'b;echo-pwned");
        let remote = spec.argv(10, None).pop().unwrap();
        assert_eq!(
            run_remote(&remote, "docker"),
            ["logs", "-f", "--tail", "10", "a'b;echo-pwned"]
        );
    }
}
