//! A command as a log source: runs `ssh`, `docker logs`, `kubectl logs` or any command, reads
//! its output as lines, and starts it again when it ends.
//!
//! One *supervisor* thread per source does the work. Dropping the `StreamGuard` kills the
//! command's whole process group right away, so closing a tile or quitting never leaves an
//! `ssh` or `kubectl` running in the background.

use std::io::{BufReader, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::line::CappedLine;
use crate::spec::{CommandSpec, Restart};
use crate::tail::{CHANNEL_BOUND, Status, TailMsg};

/// The wait before restarting doubles each time, up to this many seconds.
const MAX_BACKOFF_SECS: u64 = 30;

/// A run at least this long counts as healthy: the next wait starts from 1 s again.
const HEALTHY_AFTER: Duration = Duration::from_secs(10);

/// Added to "how long we were gone" when asking docker or kubectl to replay what was missed.
const SINCE_MARGIN: Duration = Duration::from_secs(2);

/// How long to wait before starting a command again: 1, 2, 4, 8, 16, then 30 seconds, for as
/// long as it keeps failing. A run that lasted `HEALTHY_AFTER` or more counts as a working
/// connection that dropped, not as a failure to connect, and starts the waits over at 1 second.
struct Backoff {
    attempt: u32,
}

impl Backoff {
    fn new() -> Self {
        Self { attempt: 0 }
    }

    /// The seconds to wait after a run that lasted `ran_for`.
    fn next_delay(&mut self, ran_for: Duration) -> u64 {
        if ran_for >= HEALTHY_AFTER {
            self.attempt = 0;
        }
        let delay = (1u64 << self.attempt.min(5)).min(MAX_BACKOFF_SECS);
        self.attempt += 1;
        delay
    }
}

/// Owned by the tile. Dropping it stops the source.
pub struct StreamGuard {
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Kill now, on this thread. Waiting for the supervisor would not do: when the program
        // exits, its threads are cut off before they get to clean up.
        if let Ok(mut slot) = self.child.lock()
            && let Some(child) = slot.as_mut()
        {
            kill_group(child);
        }
    }
}

/// Kills the command and everything it started: it leads its own process group, and the signal
/// goes to the group (`sh -c 'a | b'` has helpers that a plain `kill` of the shell would leave).
fn kill_group(child: &mut Child) {
    if let Ok(pid) = i32::try_from(child.id()) {
        // SAFETY: `killpg` takes two plain integers and has no memory-safety requirements.
        // It fails (and we ignore that) if the group is already gone.
        #[allow(unsafe_code)]
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

/// Starts supervising `spec`. Lines and status changes arrive on the returned channel.
pub fn spawn(spec: CommandSpec, initial_lines: usize) -> (Receiver<TailMsg>, StreamGuard) {
    let (tx, rx) = mpsc::sync_channel(CHANNEL_BOUND);
    let stop = Arc::new(AtomicBool::new(false));
    let child = Arc::new(Mutex::new(None));
    let guard = StreamGuard {
        stop: stop.clone(),
        child: child.clone(),
    };
    thread::spawn(move || supervise(&spec, initial_lines, &tx, &stop, &child));
    (rx, guard)
}

/// How one run of the command ended.
enum Outcome {
    /// Stopped by the guard, or the UI is gone. Nothing more to do.
    Stopped,
    /// It could not be started at all (program not installed, ...). Retrying won't help.
    CannotStart(String),
    /// It ran and ended.
    Ended { success: bool, why: String },
}

fn supervise(
    spec: &CommandSpec,
    initial_lines: usize,
    tx: &SyncSender<TailMsg>,
    stop: &AtomicBool,
    slot: &Mutex<Option<Child>>,
) {
    let send_status = |status: Status| tx.send(TailMsg::Status(status)).is_ok();
    let mut backoff = Backoff::new();
    let mut gone_since: Option<Instant> = None;

    while !stop.load(Ordering::SeqCst) {
        if !send_status(Status::Connecting) {
            return;
        }
        let since = gone_since.map(|moment| moment.elapsed() + SINCE_MARGIN);
        let started = Instant::now();

        let (success, why) = match run_once(spec, initial_lines, since, tx, stop, slot) {
            Outcome::Stopped => return,
            Outcome::CannotStart(why) => {
                send_status(Status::Ended { why });
                return;
            }
            Outcome::Ended { success, why } => (success, why),
        };

        if success && spec.restart() == Restart::OnFailure {
            send_status(Status::Ended {
                why: "finished".to_string(),
            });
            return;
        }

        // Wait, then start again.
        let delay = backoff.next_delay(started.elapsed());
        gone_since = Some(Instant::now());

        for remaining in (1..=delay).rev() {
            if !send_status(Status::Retrying {
                in_secs: remaining,
                why: why.clone(),
            }) {
                return;
            }
            if sleep_unless_stopped(Duration::from_secs(1), stop) {
                return;
            }
        }
        // A marker in the log itself, so a gap (or a few repeated lines) is not a mystery.
        let marker = format!("── connection lost ({why}), reconnecting ──");
        if tx
            .send(TailMsg::Line {
                offset: 0,
                text: marker,
            })
            .is_err()
        {
            return;
        }
    }
}

/// Sleeps in short steps. Returns `true` if the source was stopped meanwhile.
fn sleep_unless_stopped(total: Duration, stop: &AtomicBool) -> bool {
    let step = Duration::from_millis(100);
    let mut slept = Duration::ZERO;
    while slept < total {
        if stop.load(Ordering::SeqCst) {
            return true;
        }
        thread::sleep(step);
        slept += step;
    }
    stop.load(Ordering::SeqCst)
}

/// Starts the command once and reads it to the end.
fn run_once(
    spec: &CommandSpec,
    initial_lines: usize,
    since: Option<Duration>,
    tx: &SyncSender<TailMsg>,
    stop: &AtomicBool,
    slot: &Mutex<Option<Child>>,
) -> Outcome {
    let argv = spec.argv(initial_lines, since);
    let mut command = Command::new(&argv[0]);
    // Its own process group: see `kill_group`.
    command.args(&argv[1..]).process_group(0);
    command.stdin(if spec.keeps_stdin_open() {
        Stdio::piped()
    } else {
        Stdio::null()
    });

    // Output wiring. Merged: one pipe receives both streams. Separate: two pipes, and stderr
    // is only read for the last line, which explains a failure.
    let mut merged_reader = None;
    if spec.merges_stderr() {
        let (reader, writer) = match std::io::pipe() {
            Ok(pipe) => pipe,
            Err(e) => return Outcome::CannotStart(format!("cannot create a pipe: {e}")),
        };
        let writer_copy = match writer.try_clone() {
            Ok(copy) => copy,
            Err(e) => return Outcome::CannotStart(format!("cannot create a pipe: {e}")),
        };
        command.stdout(writer_copy).stderr(writer);
        merged_reader = Some(reader);
    } else {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => return Outcome::CannotStart(format!("cannot start {}: {e}", argv[0])),
    };
    // `command` still holds our copies of the pipe's write end. Dropping it lets the reader see
    // the end of the output once the child is gone.
    drop(command);

    let stdout: Box<dyn Read + Send> = match merged_reader {
        Some(reader) => Box::new(reader),
        None => Box::new(child.stdout.take().expect("stdout was piped")),
    };
    let stderr_reader = child.stderr.take().map(|stderr| {
        thread::spawn(move || {
            let mut last = String::new();
            let (mut reader, mut line) = (BufReader::new(stderr), CappedLine::new());
            loop {
                line.clear();
                if !matches!(line.read_from(&mut reader), Ok(read) if read > 0) {
                    return last;
                }
                let text = line.to_text();
                if !text.trim().is_empty() {
                    last = text;
                }
            }
        })
    });
    // Held until this function returns: closing it is how the remote side learns we are gone.
    let _stdin = child.stdin.take();

    // Hand the child to the guard, so it can kill it from the outside.
    if let Ok(mut held) = slot.lock() {
        *held = Some(child);
    }
    if stop.load(Ordering::SeqCst) {
        if let Ok(mut held) = slot.lock()
            && let Some(child) = held.as_mut()
        {
            kill_group(child);
        }
        return Outcome::Stopped;
    }
    if tx.send(TailMsg::Status(Status::Connected)).is_err() {
        return Outcome::Stopped;
    }

    let mut reader = BufReader::new(stdout);
    let mut line = CappedLine::new();
    // With stderr merged into the output, the tool's own error message (docker's "No such
    // container") is among the lines. The last one explains a failed exit.
    let merged = spec.merges_stderr();
    let mut last_output = String::new();
    loop {
        line.clear();
        match line.read_from(&mut reader) {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let text = line.to_text();
                if merged && !text.trim().is_empty() {
                    last_output.clone_from(&text);
                }
                if tx.send(TailMsg::Line { offset: 0, text }).is_err() {
                    return Outcome::Stopped; // the guard is dropping us anyway
                }
            }
        }
    }

    // The output ended. Reap the process and find out why.
    let child = slot.lock().ok().and_then(|mut held| held.take());
    let status = child.map(|mut child| child.wait());
    if stop.load(Ordering::SeqCst) {
        return Outcome::Stopped;
    }
    let last_stderr = stderr_reader
        .and_then(|handle| handle.join().ok())
        .unwrap_or_default();

    let (success, mut why) = match status {
        Some(Ok(status)) => (
            status.success(),
            status
                .code()
                .map_or("killed by a signal".to_string(), |code| {
                    format!("exit {code}")
                }),
        ),
        Some(Err(e)) => (false, format!("cannot wait for the command: {e}")),
        None => (false, "the command vanished".to_string()),
    };
    if !last_stderr.is_empty() {
        why = format!("{why}: {last_stderr}");
    } else if merged && !success && !last_output.is_empty() {
        why = format!("{why}: {last_output}");
    }
    Outcome::Ended { success, why }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::SourceSpec;

    const WAIT: Duration = Duration::from_secs(5);

    fn raw(command: &str) -> CommandSpec {
        match SourceSpec::parse(&format!("cmd:{command}")).unwrap() {
            SourceSpec::Command(spec) => spec,
            SourceSpec::File(_) | SourceSpec::Merged | SourceSpec::Stdin => unreachable!(),
        }
    }

    /// The next message, as a short string: `line:text`, `status:...`.
    fn next(rx: &Receiver<TailMsg>) -> String {
        match rx.recv_timeout(WAIT).expect("no message in time") {
            TailMsg::Line { text, .. } => format!("line:{text}"),
            TailMsg::Rotated => "rotated".to_string(),
            TailMsg::Status(Status::Connecting) => "connecting".to_string(),
            TailMsg::Status(Status::Connected) => "connected".to_string(),
            TailMsg::Status(Status::Retrying { why, .. }) => format!("retrying:{why}"),
            TailMsg::Status(Status::Ended { why }) => format!("ended:{why}"),
        }
    }

    #[test]
    fn the_wait_grows_while_it_keeps_failing_and_stops_at_thirty_seconds() {
        let mut backoff = Backoff::new();
        let quick = Duration::from_millis(300);
        let waits: Vec<u64> = (0..8).map(|_| backoff.next_delay(quick)).collect();
        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30, 30]);
    }

    #[test]
    fn a_connection_that_held_for_ten_seconds_starts_the_waits_over() {
        let mut backoff = Backoff::new();
        let quick = Duration::from_secs(1);
        for _ in 0..5 {
            backoff.next_delay(quick); // 1, 2, 4, 8, 16
        }
        // It connected, ran for a while, then dropped: that is not a failure to connect.
        assert_eq!(backoff.next_delay(HEALTHY_AFTER), 1);
        assert_eq!(
            backoff.next_delay(quick),
            2,
            "and the growth starts again from there"
        );

        // Just under the limit does not count as healthy.
        let mut backoff = Backoff::new();
        for _ in 0..3 {
            backoff.next_delay(quick);
        }
        assert_eq!(
            backoff.next_delay(HEALTHY_AFTER - Duration::from_millis(1)),
            8
        );
    }

    #[test]
    fn reads_lines_and_ends_when_a_command_finishes_successfully() {
        let (rx, _guard) = spawn(raw("printf 'a\\nb\\npartial'"), 100);
        let seen: Vec<String> = (0..6).map(|_| next(&rx)).collect();
        assert_eq!(
            seen,
            [
                "connecting",
                "connected",
                "line:a",
                "line:b",
                "line:partial",
                "ended:finished"
            ]
        );
    }

    #[test]
    fn a_failing_command_is_restarted_and_its_stderr_explains_why() {
        let (rx, _guard) = spawn(raw("echo out; echo oops >&2; exit 3"), 100);
        assert_eq!(next(&rx), "connecting");
        assert_eq!(next(&rx), "connected");
        assert_eq!(next(&rx), "line:out");
        assert_eq!(next(&rx), "retrying:exit 3: oops");
        // One second later it runs again, with a marker in the log first.
        let mut messages = Vec::new();
        for _ in 0..4 {
            messages.push(next(&rx));
        }
        assert!(
            messages
                .iter()
                .any(|m| m.starts_with("line:── connection lost (exit 3: oops)"))
        );
        assert!(
            messages.contains(&"line:out".to_string()),
            "second run: {messages:?}"
        );
    }

    #[test]
    fn with_merged_streams_the_last_line_explains_a_failure() {
        let spec = CommandSpec::raw_for_test(
            "echo started; echo 'Error: No such container: x' >&2; exit 1",
            true,
        );
        let (rx, _guard) = spawn(spec, 10);
        let events: Vec<String> = (0..5).map(|_| next(&rx)).collect();
        assert!(
            events
                .iter()
                .any(|e| e == "retrying:exit 1: Error: No such container: x"),
            "{events:?}"
        );
    }

    #[test]
    fn a_missing_program_ends_the_source_instead_of_retrying_forever() {
        // `sh` exists, but the spec below runs a program that doesn't: use a docker-less path.
        let spec = match SourceSpec::parse("docker:x").unwrap() {
            SourceSpec::Command(spec) => spec,
            SourceSpec::File(_) | SourceSpec::Merged | SourceSpec::Stdin => unreachable!(),
        };
        // Only meaningful where docker is not installed; there the source must end at once.
        if std::process::Command::new("docker")
            .arg("--version")
            .output()
            .is_err()
        {
            let (rx, _guard) = spawn(spec, 10);
            assert_eq!(next(&rx), "connecting");
            assert!(next(&rx).starts_with("ended:cannot start docker"));
        }
    }

    #[test]
    fn merged_streams_carry_stderr_lines_too() {
        let spec = CommandSpec::raw_for_test("echo to-out; echo to-err >&2", true);
        let (rx, _guard) = spawn(spec, 10);
        let mut lines: Vec<String> = (0..5)
            .map(|_| next(&rx))
            .filter(|m| m.starts_with("line:"))
            .collect();
        lines.sort();
        assert_eq!(lines, ["line:to-err", "line:to-out"]);
    }

    /// Whether process `pid` still exists.
    fn alive(pid: &str) -> bool {
        std::process::Command::new("kill")
            .args(["-0", pid])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    #[test]
    fn dropping_the_guard_kills_the_command_and_its_helpers() {
        let pid_file =
            std::env::temp_dir().join(format!("tilog-stream-pid-{}", std::process::id()));
        let helper_file =
            std::env::temp_dir().join(format!("tilog-stream-helper-{}", std::process::id()));
        // A shell that starts a helper in the background, records both pids, and waits.
        let script = format!(
            "sleep 60 & echo $! > {}; echo $$ > {}; echo started; wait",
            helper_file.display(),
            pid_file.display()
        );
        let (rx, guard) = spawn(raw(&script), 10);
        while next(&rx) != "line:started" {}

        let shell = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .to_string();
        let helper = std::fs::read_to_string(&helper_file)
            .unwrap()
            .trim()
            .to_string();
        assert!(alive(&shell) && alive(&helper));

        drop(guard);
        let deadline = Instant::now() + Duration::from_secs(3);
        while (alive(&shell) || alive(&helper)) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(!alive(&shell), "the shell must be gone");
        assert!(!alive(&helper), "so must the helper it started");
        let _ = std::fs::remove_file(&pid_file);
        let _ = std::fs::remove_file(&helper_file);
    }
}
