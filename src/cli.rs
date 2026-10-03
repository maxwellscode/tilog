//! Command line: program metadata and argument parsing.

use anyhow::{Result, bail};

// `env!` reads these from Cargo.toml *at compile time*, so they can never go out of date.
pub const NAME: &str = env!("CARGO_PKG_NAME");
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const DESCRIPTION: &str = env!("CARGO_PKG_DESCRIPTION");

#[derive(Debug, PartialEq)]
pub enum Cli {
    Run {
        session: Option<String>,
        paths: Vec<String>,
    },
    Help,
    Version,
    PrintConfig,
}

pub fn usage() -> String {
    format!(
        "{NAME} v{VERSION}\n{DESCRIPTION}\n\n\
         USAGE:\n    {NAME} [OPTIONS] [SOURCE]...\n\n\
         SOURCE is a file, a name from the config, or:\n    \
         ssh:[user@]host:/path   docker:container   kube:[namespace/]pod[/container]   cmd:COMMAND\n\n\
         OPTIONS:\n    \
         -s, --session <NAME>  Load a saved session (see :save inside {NAME})\n    \
         -c, --command <CMD>   Follow the output of a command (same as cmd:CMD)\n    \
         -h, --help            Print this help\n    \
         -V, --version         Print the version\n    \
         --print-config        Print the built-in configuration (colors) as a starting point\n\n\
         CONFIG:\n    ~/.config/tilog/config.toml  (try: {NAME} --print-config > that file)"
    )
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Cli> {
    let mut session = None;
    let mut paths = Vec::new();
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "-h" | "--help" => return Ok(Cli::Help),
            "-V" | "--version" => return Ok(Cli::Version),
            "--print-config" => return Ok(Cli::PrintConfig),
            "-c" | "--command" => match args.next() {
                Some(command) => paths.push(format!("cmd:{command}")),
                None => bail!("{arg} needs a command"),
            },
            "-s" | "--session" => match args.next() {
                Some(name) => session = Some(name),
                None => bail!("{arg} needs a session name"),
            },
            // `--session=name` form.
            other if other.starts_with("--session=") => {
                session = Some(other["--session=".len()..].to_string());
            }
            other if other.starts_with('-') && other != "-" => {
                bail!("unknown option {other} (try --help)")
            }
            _ => paths.push(arg),
        }
    }
    Ok(Cli::Run { session, paths })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Cli> {
        parse(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn parses_paths_and_session() {
        assert_eq!(
            parse_strs(&["a.log", "-s", "work", "b.log"]).unwrap(),
            Cli::Run {
                session: Some("work".into()),
                paths: vec!["a.log".into(), "b.log".into()]
            }
        );
        assert_eq!(
            parse_strs(&["--session=prod"]).unwrap(),
            Cli::Run {
                session: Some("prod".into()),
                paths: vec![]
            }
        );
        assert_eq!(
            parse_strs(&[]).unwrap(),
            Cli::Run {
                session: None,
                paths: vec![]
            }
        );
    }

    #[test]
    fn commands_become_cmd_sources() {
        assert_eq!(
            parse_strs(&["-c", "journalctl -fu app", "ssh:h:/x", "a.log"]).unwrap(),
            Cli::Run {
                session: None,
                paths: vec![
                    "cmd:journalctl -fu app".into(),
                    "ssh:h:/x".into(),
                    "a.log".into()
                ]
            }
        );
        assert!(parse_strs(&["-c"]).is_err());
    }

    #[test]
    fn parses_flags_and_errors() {
        assert_eq!(parse_strs(&["-V"]).unwrap(), Cli::Version);
        assert_eq!(parse_strs(&["--print-config"]).unwrap(), Cli::PrintConfig);
        assert_eq!(parse_strs(&["x.log", "--help"]).unwrap(), Cli::Help);
        assert!(parse_strs(&["--session"]).is_err());
        assert!(parse_strs(&["--bogus"]).is_err());
    }

    #[test]
    fn metadata_comes_from_cargo_toml() {
        assert_eq!(NAME, "tilog");
        assert!(!VERSION.is_empty() && !DESCRIPTION.is_empty());
        assert!(usage().starts_with(&format!("tilog v{VERSION}")));
    }
}
