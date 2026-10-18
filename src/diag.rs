//! What happened, for finding out why: with `TILOG_LOG=/some/file`, tilog writes one line per
//! event there (sources starting and ending, reconnects, rotations, errors, signals, panics).
//! Without it nothing is written anywhere. Passwords and log lines are never part of it.
//!
//! And panics: one on the main thread gives the terminal back and says where to report it;
//! one on a reader thread is written down, the screen stays, and only that source stops.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::panic::{self, PanicHookInfo};
use std::sync::{Mutex, OnceLock};
use std::thread;

use crate::timestamp;

/// Where reports go.
pub const ISSUES: &str = "https://github.com/maxwellscode/tilog/issues";

static FILE: OnceLock<Option<Mutex<File>>> = OnceLock::new();

/// Opens the file named by `TILOG_LOG`, if any. Called once, at the start.
pub fn init() {
    FILE.get_or_init(|| {
        let path = std::env::var_os("TILOG_LOG")?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .ok()?;
        Some(Mutex::new(file))
    });
}

/// Writes `text` to the diagnostic log, if there is one, with the time and the thread.
pub fn log(text: &str) {
    let Some(Some(file)) = FILE.get() else {
        return;
    };
    let now = timestamp::now_ms();
    let thread = thread::current();
    let line = format!(
        "{}.{:03} [{}] {text}\n",
        timestamp::utc_clock(now),
        now.rem_euclid(1000),
        thread.name().unwrap_or("reader")
    );
    if let Ok(mut file) = file.lock() {
        let _ = file.write_all(line.as_bytes());
    }
}

/// Takes over panics, on top of the hook that gives the terminal back (`ratatui` installs it,
/// for any thread). Call it once the screen is set up.
pub fn install_panic_hook() {
    let terminal_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info: &PanicHookInfo<'_>| {
        log(&format!("panic: {info}"));
        if thread::current().name() == Some("main") {
            terminal_hook(info);
            eprintln!(
                "\ntilog stopped because of a bug. Please report it at {ISSUES} with the \
                 message above{}.",
                if matches!(FILE.get(), Some(Some(_))) {
                    " and the TILOG_LOG file"
                } else {
                    ""
                }
            );
        }
        // On another thread, printing would scribble over the screen and giving the terminal
        // back would break it: the source of that thread ends, and its tile says so.
    }));
}
