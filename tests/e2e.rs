//! End-to-end tests: the real binary in a pseudo-terminal, read back as a screen.
//!
//! The unit tests cover the parts; these cover what only shows when they run together: the
//! terminal is set up and given back, a file is followed through a rotation, commands stop when
//! tilog does (after `q` and after a signal), and what `:write` and `:save` leave behind.

#![allow(unsafe_code)]

use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

const ROWS: u16 = 24;
const COLS: u16 = 100;
/// Long enough for a slow CI machine; a passing test is done well before.
const PATIENCE: Duration = Duration::from_secs(10);

/// A folder of its own for one test, removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("tilog-e2e-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }

    /// A shell script that writes its process id to `pid`, prints a line and then stays quiet:
    /// like `tail -F` on a log that nobody writes to.
    fn quiet_command(&self) -> (PathBuf, PathBuf) {
        let (script, pid) = (self.path("quiet.sh"), self.path("quiet.pid"));
        let text = format!(
            "#!/bin/sh\necho $$ > '{}'\necho quiet-command-started\nexec sleep 300\n",
            pid.display()
        );
        fs::write(&script, text).unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        (script, pid)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// tilog running in a pseudo-terminal of `ROWS` x `COLS`.
struct Tilog {
    child: Child,
    master: File,
    screen: Arc<Mutex<vt100::Parser>>,
    /// Everything tilog wrote, to check the escape codes that give the terminal back.
    raw: Arc<Mutex<Vec<u8>>>,
}

impl Tilog {
    fn start(scratch: &Scratch, args: &[&str]) -> Self {
        Self::start_with(scratch, args, &[])
    }

    fn start_with(scratch: &Scratch, args: &[&str], env: &[(&str, &str)]) -> Self {
        let (master, slave) = open_pty();
        let slave_copy = |fd: &OwnedFd| Stdio::from(fd.try_clone().unwrap());
        let mut command = Command::new(env!("CARGO_BIN_EXE_tilog"));
        command
            .args(args)
            .current_dir(&scratch.0)
            .env("XDG_CONFIG_HOME", scratch.path("config"))
            .env("TERM", "xterm-256color")
            .env_remove("NO_COLOR")
            .envs(env.iter().copied())
            .stdin(slave_copy(&slave))
            .stdout(slave_copy(&slave))
            .stderr(slave_copy(&slave));
        // SAFETY: `setsid` and `ioctl` are async-signal-safe. The child leads a session of its
        // own and takes the pty (its stdin by now) as its terminal, like a login shell would.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                libc::ioctl(0, libc::TIOCSCTTY as _, 0);
                Ok(())
            });
        }
        let child = command.spawn().unwrap();
        drop(command);
        drop(slave);

        let screen = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
        let raw = Arc::new(Mutex::new(Vec::new()));
        let mut reader = master.try_clone().unwrap();
        let (screen_in, raw_in) = (screen.clone(), raw.clone());
        thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            // Ends with an error (EIO on Linux) once tilog and its terminal are gone.
            while let Ok(n @ 1..) = reader.read(&mut buffer) {
                screen_in.lock().unwrap().process(&buffer[..n]);
                raw_in.lock().unwrap().extend_from_slice(&buffer[..n]);
            }
        });
        Self {
            child,
            master,
            screen,
            raw,
        }
    }

    fn screen(&self) -> String {
        self.screen.lock().unwrap().screen().contents()
    }

    /// Waits until the screen shows `text`, and fails with the screen if it never does.
    fn wait_for(&self, text: &str) {
        let start = Instant::now();
        while start.elapsed() < PATIENCE {
            if self.screen().contains(text) {
                return;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("never saw {text:?} on the screen:\n{}", self.screen());
    }

    fn keys(&mut self, keys: &str) {
        self.master.write_all(keys.as_bytes()).unwrap();
        self.master.flush().unwrap();
        // Separate writes must not arrive as one: `Esc` then `/` would read as Alt+/.
        thread::sleep(Duration::from_millis(100));
    }

    /// A `:` command, typed and entered.
    fn command(&mut self, text: &str) {
        self.keys(":");
        self.keys(text);
        self.keys("\r");
    }

    fn signal(&self, signal: i32) {
        // SAFETY: `kill` takes two plain integers.
        unsafe {
            libc::kill(self.child.id() as i32, signal);
        }
    }

    fn wait_exit(&mut self) -> ExitStatus {
        let start = Instant::now();
        while start.elapsed() < PATIENCE {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("tilog did not end:\n{}", self.screen());
    }

    fn raw_output(&self) -> String {
        String::from_utf8_lossy(&self.raw.lock().unwrap()).into_owned()
    }
}

impl Drop for Tilog {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn open_pty() -> (File, OwnedFd) {
    let (mut master, mut slave) = (0, 0);
    let mut size = libc::winsize {
        ws_row: ROWS,
        ws_col: COLS,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: `openpty` writes two descriptors into the integers it is given and reads the
    // size; it does not keep the pointers.
    let done = unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &raw mut size,
        )
    };
    assert_eq!(done, 0, "openpty: {}", std::io::Error::last_os_error());
    // SAFETY: both descriptors were just opened and are owned by nothing else.
    unsafe { (File::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) }
}

/// Whether the process `pid` still exists (a zombie waiting to be reaped counts as gone).
fn running(pid: i32) -> bool {
    let state = Command::new("ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let state = String::from_utf8_lossy(&state.stdout);
    !state.trim().is_empty() && !state.trim_start().starts_with('Z')
}

/// Waits for the process id the quiet command writes when it starts.
fn pid_in(file: &Path) -> i32 {
    let start = Instant::now();
    while start.elapsed() < PATIENCE {
        if let Some(pid) = fs::read_to_string(file)
            .ok()
            .and_then(|text| text.trim().parse().ok())
        {
            return pid;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("the command never wrote its pid");
}

/// Waits for `pid` to be gone; fails (after stopping it) if it is still there.
fn assert_ends(pid: i32, after: &str) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(3) {
        if !running(pid) {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    // SAFETY: `kill` takes two plain integers. The test's own leftover is cleaned up.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
    panic!("the command tilog started was still running {after}");
}

fn numbered_lines(count: usize) -> String {
    (1..=count).map(|n| format!("line {n}\n")).collect()
}

#[test]
fn a_file_opens_at_its_end_and_q_gives_the_terminal_back() {
    let scratch = Scratch::new("open");
    fs::write(scratch.path("app.log"), numbered_lines(5000)).unwrap();
    let mut tilog = Tilog::start(&scratch, &["app.log"]);
    tilog.keys("\r"); // open the source from the overview
    tilog.wait_for("line 5000");
    assert!(!tilog.screen().contains("line 1\n"), "opened at the end");

    tilog.keys("q");
    assert_eq!(tilog.wait_exit().code(), Some(0));
    let raw = tilog.raw_output();
    assert!(raw.contains("\x1b[?1049l"), "left the alternate screen");
    assert!(raw.contains("\x1b[?1000l"), "mouse reporting turned off");
    assert!(raw.contains("\x1b[?25h"), "cursor shown again");
}

#[test]
fn a_file_is_followed_through_a_rotation() {
    let scratch = Scratch::new("rotate");
    let log = scratch.path("app.log");
    fs::write(&log, "before rotation\n").unwrap();
    let mut tilog = Tilog::start(&scratch, &["app.log"]);
    tilog.keys("\r");
    tilog.wait_for("before rotation");

    fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .unwrap()
        .write_all(b"appended line\n")
        .unwrap();
    tilog.wait_for("appended line");

    // Rotated like logrotate does by default: renamed away, and a new file in its place.
    fs::rename(&log, scratch.path("app.log.1")).unwrap();
    fs::write(&log, "after rotation\n").unwrap();
    tilog.wait_for("after rotation");

    // And truncated in place, as `copytruncate` does.
    fs::write(&log, "after truncation\n").unwrap();
    tilog.wait_for("after truncation");
}

#[test]
fn write_saves_the_lines_of_the_tile() {
    let scratch = Scratch::new("write");
    fs::write(scratch.path("app.log"), numbered_lines(20)).unwrap();
    let mut tilog = Tilog::start(&scratch, &["app.log"]);
    tilog.keys("\r");
    tilog.wait_for("line 20");
    tilog.command("write saved.txt");
    tilog.wait_for("wrote 20 lines");
    assert_eq!(
        fs::read_to_string(scratch.path("saved.txt")).unwrap(),
        numbered_lines(20)
    );

    // Not over an existing file, unless asked.
    tilog.command("write saved.txt");
    tilog.wait_for("exists (:write -f replaces it)");
}

#[test]
fn a_saved_session_opens_the_same_sources_again() {
    let scratch = Scratch::new("session");
    fs::write(scratch.path("first.log"), "from the first log\n").unwrap();
    fs::write(scratch.path("second.log"), "from the second log\n").unwrap();
    let mut tilog = Tilog::start(&scratch, &["first.log", "second.log"]);
    tilog.wait_for("from the second log");
    tilog.command("save e2e");
    tilog.wait_for("saved");
    tilog.keys("q");
    assert_eq!(tilog.wait_exit().code(), Some(0));
    assert!(scratch.path("config/tilog/sessions/e2e.toml").exists());

    let tilog = Tilog::start(&scratch, &["-s", "e2e"]);
    tilog.wait_for("from the first log");
    tilog.wait_for("from the second log");
}

#[test]
fn a_command_stops_when_tilog_quits() {
    let scratch = Scratch::new("quit-stops");
    let (script, pid_file) = scratch.quiet_command();
    let source = format!("cmd:{}", script.display());
    let mut tilog = Tilog::start(&scratch, &[&source]);
    tilog.wait_for("quiet-command-started");
    let pid = pid_in(&pid_file);

    tilog.keys("q");
    assert_eq!(tilog.wait_exit().code(), Some(0));
    assert_ends(pid, "after q");
}

#[test]
fn a_closed_terminal_or_kill_stops_the_commands_too() {
    for (signal, name) in [(libc::SIGHUP, "SIGHUP"), (libc::SIGTERM, "SIGTERM")] {
        let scratch = Scratch::new(&format!("signal-{signal}"));
        let (script, pid_file) = scratch.quiet_command();
        let source = format!("cmd:{}", script.display());
        let mut tilog = Tilog::start(&scratch, &[&source]);
        tilog.wait_for("quiet-command-started");
        let pid = pid_in(&pid_file);

        tilog.signal(signal);
        assert_eq!(tilog.wait_exit().code(), Some(128 + signal), "{name}");
        assert_ends(pid, &format!("after {name}"));
    }
}

/// Only Linux can promise this: the kernel kills the command when its parent dies. It is the
/// command tilog starts itself (here `ssh`, a stand-in on `PATH`); see `die_with_us`.
#[cfg(target_os = "linux")]
#[test]
fn even_kill_9_leaves_no_ssh_behind_on_linux() {
    let scratch = Scratch::new("kill-9");
    let (script, pid_file) = scratch.quiet_command();
    let bin = scratch.path("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::rename(&script, bin.join("ssh")).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let mut tilog = Tilog::start_with(&scratch, &["ssh:web1:/var/log/app.log"], &[("PATH", &path)]);
    tilog.wait_for("quiet-command-started");
    let pid = pid_in(&pid_file);

    tilog.signal(libc::SIGKILL);
    tilog.wait_exit();
    assert_ends(pid, "after kill -9");
}

#[test]
fn tilog_log_records_what_happened_but_no_log_lines() {
    let scratch = Scratch::new("diag");
    let script = scratch.path("failing.sh");
    fs::write(
        &script,
        "#!/bin/sh\necho private-log-line\necho broken >&2\nexit 3\n",
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let log = scratch.path("diag.log");
    let source = format!("cmd:{}", script.display());
    let mut tilog = Tilog::start_with(
        &scratch,
        &[&source],
        &[("TILOG_LOG", log.to_str().unwrap())],
    );
    tilog.wait_for("private-log-line");
    tilog.wait_for("retry in");
    tilog.keys("q");
    assert_eq!(tilog.wait_exit().code(), Some(0));

    let text = fs::read_to_string(&log).unwrap();
    for expected in [
        "tilog 0.1.0 started",
        ": starting",
        "ended: exit 3: broken",
        "trying again in",
    ] {
        assert!(
            text.contains(expected),
            "{expected:?} missing from:\n{text}"
        );
    }
    assert!(
        text.trim_end().ends_with("] ended"),
        "the end is noted:\n{text}"
    );
    assert!(
        !text.contains("private-log-line"),
        "no log lines in it:\n{text}"
    );
}

#[test]
fn output_into_a_pipe_is_refused_with_a_plain_message() {
    let scratch = Scratch::new("pipe");
    fs::write(scratch.path("app.log"), "x\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_tilog"))
        .arg("app.log")
        .current_dir(&scratch.0)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("must be a terminal"), "{stderr}");
    assert!(
        output.stdout.is_empty(),
        "no escape codes written into the pipe"
    );
}

#[test]
fn a_missing_file_is_refused_before_the_screen_is_drawn() {
    let scratch = Scratch::new("missing");
    let mut tilog = Tilog::start(&scratch, &["no-such.log"]);
    assert_eq!(tilog.wait_exit().code(), Some(1));
    // The reader thread may still be taking in the last bytes.
    thread::sleep(Duration::from_millis(200));
    let raw = tilog.raw_output();
    assert!(raw.contains("no-such.log"), "{raw}");
    assert!(
        !raw.contains("\x1b[?1049h"),
        "the screen was never taken over"
    );
}
