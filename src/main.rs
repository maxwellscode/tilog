mod app;
mod askpass;
mod buffer;
mod cli;
mod clipboard;
mod command;
mod completion;
mod config;
mod filter;
mod filter_view;
mod group;
mod highlight;
mod history;
mod input;
mod layout;
mod line;
mod lines;
mod merge;
mod pipe;
mod select;
mod session;
mod severity;
mod source;
mod spec;
mod stream;
mod stream_filter;
mod tail;
mod theme;
mod tile;
mod timestamp;
mod viewport;
mod when;

use std::env;
use std::io::{self, IsTerminal};

use anyhow::Result;
use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;

use crate::app::{Added, App, MAX_SOURCES};
use crate::cli::{Cli, NAME, VERSION};

/// Turns mouse reporting on, and off again when dropped.
///
/// `Drop` runs when a value goes out of scope, *including* while unwinding from a panic, so the
/// terminal can't be left sending mouse escape codes into your shell. This pattern is called
/// RAII.
struct MouseCapture;

impl MouseCapture {
    fn enable() -> Result<Self> {
        execute!(io::stdout(), EnableMouseCapture)?;
        Ok(Self)
    }
}

impl Drop for MouseCapture {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableMouseCapture);
    }
}

fn main() -> Result<()> {
    // `ssh` starts tilog as its helper to ask a question: see `askpass`.
    if env::var_os(askpass::ENV_SOCKET).is_some() {
        std::process::exit(askpass::run_helper());
    }
    match cli::parse(env::args().skip(1))? {
        Cli::Help => println!("{}", cli::usage()),
        Cli::Version => println!("{NAME} {VERSION}"),
        Cli::PrintConfig => print!("{}", config::DEFAULT_CONFIG),
        Cli::Run { session, paths } => {
            let mut app = App::new();
            // The config first: named sources must be known before paths are opened. A problem
            // in it is shown at the end, so the messages of the steps below don't hide it.
            let config_error = app.apply_config().err();
            // Like `less`: with nothing else to open and something piped in, read the pipe.
            let mut paths = paths;
            let read_pipe = paths.is_empty() && session.is_none() && !io::stdin().is_terminal();
            // The session next, then the sources from the command line on top of it.
            if let Some(name) = session {
                app.load_session(&name)?;
            }
            if read_pipe {
                paths.push("-".to_string());
            }
            let (mut skipped, mut repeated) = (0, 0);
            for path in &paths {
                // The same log twice, or more files than there is room for (`tilog *.log`),
                // is not an error: open what fits and say so.
                if app.is_full() {
                    skipped += 1;
                    continue;
                }
                if app.add_path(path)? != Added::New {
                    repeated += 1;
                }
            }
            if skipped > 0 {
                app.show_error(&format!(
                    "opened {MAX_SOURCES} sources, skipped {skipped} more (limit {MAX_SOURCES})"
                ));
            } else if repeated > 0 {
                app.show_error(&format!(
                    "{repeated} source(s) were given twice and are open once"
                ));
            }
            if let Some(err) = config_error {
                app.show_error(&format!("config: {err:#}"));
            }

            ratatui::run(|terminal| {
                let _mouse = MouseCapture::enable()?; // dropped when this closure ends
                app.run(terminal)
            })?;
        }
    }
    Ok(())
}
