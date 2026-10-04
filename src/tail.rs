use std::fs::{self, File, Metadata};
use std::io::{self, BufReader, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;
use std::time::Duration;

use crate::line::CappedLine;
use anyhow::{Context, Result};
use notify::{RecursiveMode, Watcher};

/// Safety net: even if the OS never tells us about a change, check the file this often.
const FALLBACK_POLL: Duration = Duration::from_secs(1);

/// After a log was replaced by a new file, how long the old one is still read before moving
/// on. A program keeps writing to the old file (it holds it open) until it reopens its log,
/// which usually takes a moment.
const ROTATION_GRACE: Duration = Duration::from_millis(500);

/// How soon to look again when the sink asked for a quick re-check (see `LineSink`).
const QUICK_POLL: Duration = Duration::from_millis(150);

/// How many messages a thread may have queued for the UI before it has to wait.
/// A bounded channel gives *backpressure*: reading a huge file can never flood memory faster
/// than the UI consumes it.
pub const CHANNEL_BOUND: usize = 1024;

/// What a following thread does with the lines it reads. The thread loop is the same for every
/// consumer; the sink decides what happens to each line. (Roughly a Java interface with a
/// default method.)
///
/// `Send + 'static` is required because the sink is moved into another thread: it must be safe
/// to transfer between threads and must not borrow from the caller's stack.
pub trait LineSink: Send + 'static {
    /// A complete line (without line ending) that starts at byte `offset` in the file.
    /// Return `false` to stop following, e.g. because the receiving UI is gone.
    fn line(&mut self, offset: u64, line: &[u8]) -> bool;

    /// The file was truncated, or replaced by a new one (log rotation): reading restarts from
    /// the beginning of what is now there.
    fn reset(&mut self) -> bool;

    /// The end of the file was reached: everything written so far has been delivered.
    fn caught_up(&mut self) -> bool {
        true
    }

    /// Return `true` while something is waiting on a short delay, e.g. a multi-line entry that
    /// is probably complete but can't be known to be until a moment passes without new lines.
    fn wants_quick_recheck(&self) -> bool {
        false
    }
}

/// Starts a background thread that follows `path` like `tail -f`, from the first byte on,
/// and feeds every line to `sink`.
pub fn follow_file(path: &str, sink: impl LineSink) -> Result<()> {
    follow_file_from(path, 0, sink)
}

/// Like `follow_file`, but starts reading at byte offset `start`, which must be the start of
/// a line. This is how a huge file is opened at its end without reading the rest.
pub fn follow_file_from(path: &str, start: u64, sink: impl LineSink) -> Result<()> {
    // Open the file *here*, on the caller's thread, so a bad path is reported
    // as a normal error instead of the thread silently dying.
    let file = File::open(path).with_context(|| format!("cannot open {path}"))?;

    // A channel only used as a doorbell: the OS file watcher rings it (sends `()`, a value
    // carrying no data) whenever something changes.
    let (wake_tx, wake_rx) = mpsc::channel::<()>();

    // The watcher calls this closure from *its own* thread, hence `move` for `wake_tx`.
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        // Reading the file itself triggers "access" events. Ignore them, or we would wake
        // ourselves up in an endless loop.
        if let Ok(event) = res
            && !event.kind.is_access()
        {
            let _ = wake_tx.send(());
        }
    })
    .context("cannot create file watcher")?;
    // Watch the *directory*. A watch on the file itself (inotify on Linux) follows that file
    // when it is renamed, and would never hear of the new file that log rotation puts in its
    // place. Events about other files in the directory only cause a harmless look at ours.
    let directory = Path::new(path)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if watcher
        .watch(directory, RecursiveMode::NonRecursive)
        .is_err()
    {
        watcher
            .watch(Path::new(path), RecursiveMode::NonRecursive)
            .with_context(|| format!("cannot watch {path}"))?;
    }

    // `move` transfers ownership of `file`, `sink`, `wake_rx` and `watcher` into the closure,
    // and so into the new thread. After this line, this function can no longer use them.
    // That is what makes it safe: only one thread owns each value.
    let mut sink = sink;
    let path = PathBuf::from(path);
    thread::spawn(move || {
        // A watcher stops watching when it is dropped. Binding it to a named variable keeps it
        // alive until the end of this closure. (`let _ = watcher;` would drop it at once.)
        let _watcher = watcher;

        // Ignoring the result is deliberate for now: if reading fails the thread just ends.
        let _ = follow(file, &path, start, &mut sink, &wake_rx);
    });

    Ok(())
}

/// Is `a` the same file as `b`? (Same device and inode. A path can come to point at another file
/// while an open handle still refers to the old one: that is what log rotation does.)
fn same_file(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// The thread's main loop. Returns only on an I/O error or when the sink says stop.
///
/// Follows `path` the way `tail -F` does: when the file is truncated it starts over, and when
/// it is replaced by a new one (renamed away, new file created) it reads the old one to its end
/// and moves on to the new one.
fn follow(
    file: File,
    path: &Path,
    start: u64,
    sink: &mut impl LineSink,
    wake_rx: &Receiver<()>,
) -> io::Result<()> {
    let mut reader = BufReader::new(file);
    reader.seek(SeekFrom::Start(start))?;
    let mut pos: u64 = start; // where we have read up to
    let mut line_start: u64 = start; // where the line currently being assembled began
    let mut line = CappedLine::new(); // the line currently being assembled
    // Set once the old file has been seen to be replaced and given its last moment.
    let mut grace_given = false;

    loop {
        // Adds bytes up to and including '\n' to the line. Returns 0 at end of file.
        // We read raw bytes rather than a `String`, because log files are not always valid UTF-8.
        let n = line.read_from(&mut reader)?;

        if n == 0 {
            if !sink.caught_up() {
                return Ok(());
            }

            // End of file: nothing new yet. Has the file been truncated (got shorter)?
            let open_file = reader.get_ref().metadata()?;
            if open_file.len() < pos {
                reader.seek(SeekFrom::Start(0))?;
                pos = 0;
                line_start = 0;
                line.clear();
                if !sink.reset() {
                    return Ok(());
                }
            }

            // Or does the path now lead to another file? (Rotated: the old one was renamed.)
            // If the path is missing for the moment, the new file isn't there yet: keep the old.
            match fs::metadata(path) {
                Ok(on_disk) if !same_file(&on_disk, &open_file) => {
                    if !grace_given {
                        // The program may still be writing to the old file. Wait a moment, then
                        // look at it once more before leaving it.
                        grace_given = true;
                        thread::sleep(ROTATION_GRACE);
                        while wake_rx.try_recv().is_ok() {}
                        continue;
                    }
                    if let Ok(replacement) = File::open(path) {
                        reader = BufReader::new(replacement);
                        pos = 0;
                        line_start = 0;
                        line.clear();
                        grace_given = false;
                        if !sink.reset() {
                            return Ok(());
                        }
                        continue;
                    }
                }
                _ => grace_given = false,
            }

            // Sleep until the watcher rings the doorbell, or the fallback timeout passes.
            // Unlike `thread::sleep`, this returns immediately when there is news.
            let timeout = if sink.wants_quick_recheck() {
                QUICK_POLL
            } else {
                FALLBACK_POLL
            };
            let _ = wake_rx.recv_timeout(timeout);
            // Several changes may have queued several wake-ups; one read pass handles them all.
            while wake_rx.try_recv().is_ok() {}
            continue;
        }

        pos += n as u64;
        grace_given = false; // the old file is still being written: it is not done yet

        // The writer may be in the middle of a line. Only emit complete lines.
        if line.is_complete() {
            let keep_going = sink.line(line_start, &line.text());
            line.clear();
            line_start = pos;
            if !keep_going {
                return Ok(());
            }
        }
    }
}

/// `line` without its trailing `\n` / `\r\n`. Returns a sub-slice: no copy.
pub fn trim_eol(line: &[u8]) -> &[u8] {
    let mut end = line.len();
    while end > 0 && matches!(line[end - 1], b'\n' | b'\r') {
        end -= 1;
    }
    &line[..end]
}

/// What the following thread of the main tile tells the UI.
pub enum TailMsg {
    /// A complete line, and the byte offset it starts at. (Lines of a command have no file
    /// offset; theirs is 0.)
    Line { offset: u64, text: String },
    /// The file was truncated or replaced (log rotation). The lines already received stay, but
    /// they no longer come from the file at the path: older lines can't be read from disk.
    Rotated,
    /// The state of a command source's process (files have none).
    Status(Status),
}

/// Where a command source (ssh, docker, kubectl, ...) is in its life.
#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Connecting,
    Connected,
    /// The command ended and will be started again in `in_secs` seconds.
    Retrying {
        in_secs: u64,
        why: String,
    },
    /// The command ended and will not be started again.
    Ended {
        why: String,
    },
}

/// The main tile's sink: forwards each line, with its offset, as a message.
struct MessageSink {
    tx: SyncSender<TailMsg>,
}

impl LineSink for MessageSink {
    fn line(&mut self, offset: u64, line: &[u8]) -> bool {
        let text = String::from_utf8_lossy(line).into_owned();
        // `send` fails only if the receiver was dropped, i.e. the UI has quit.
        self.tx.send(TailMsg::Line { offset, text }).is_ok()
    }

    fn reset(&mut self) -> bool {
        self.tx.send(TailMsg::Rotated).is_ok()
    }
}

/// Follows `path` from byte offset `start` (a line start) and returns the receiving end of a
/// channel carrying its lines.
///
/// `pub` makes the function visible outside this module. Everything else is private by default.
pub fn spawn(path: &str, start: u64) -> Result<Receiver<TailMsg>> {
    // A channel has two ends: `tx` (transmitter) sends, `rx` (receiver) receives.
    let (tx, rx) = mpsc::sync_channel(CHANNEL_BOUND);
    follow_file_from(path, start, MessageSink { tx })?;
    Ok(rx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::OpenOptions;
    use std::io::Write;

    const WAIT: Duration = Duration::from_secs(2);

    /// The next line, with its offset. Fails the test on a reset or a timeout.
    fn next_line(rx: &Receiver<TailMsg>) -> (u64, String) {
        match rx.recv_timeout(WAIT).expect("no message in time") {
            TailMsg::Line { offset, text } => (offset, text),
            other => panic!(
                "unexpected message: {}",
                match other {
                    TailMsg::Rotated => "rotated",
                    _ => "status",
                }
            ),
        }
    }

    #[test]
    fn follows_appended_lines_and_waits_for_complete_ones() {
        let path = std::env::temp_dir().join(format!("tilog-tail-test-{}.log", std::process::id()));
        std::fs::write(&path, "a\nb\n").unwrap();

        let rx = spawn(path.to_str().unwrap(), 0).unwrap();
        assert_eq!(next_line(&rx), (0, "a".to_string()));
        assert_eq!(next_line(&rx), (2, "b".to_string()));

        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"c\npart").unwrap();
        assert_eq!(next_line(&rx), (4, "c".to_string()));
        // "part" has no newline yet, so nothing may arrive.
        assert!(rx.recv_timeout(Duration::from_millis(500)).is_err());

        f.write_all(b"ial\n").unwrap();
        assert_eq!(next_line(&rx), (6, "partial".to_string()));

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn can_start_in_the_middle_of_a_file() {
        let path = std::env::temp_dir().join(format!("tilog-tail-from-{}.log", std::process::id()));
        std::fs::write(&path, "skipped\nfirst\nsecond\n").unwrap();

        let rx = spawn(path.to_str().unwrap(), 8).unwrap(); // right after "skipped\n"
        assert_eq!(next_line(&rx), (8, "first".to_string()));
        assert_eq!(next_line(&rx), (14, "second".to_string()));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_truncated_file_is_reported_and_read_from_the_start() {
        let path =
            std::env::temp_dir().join(format!("tilog-tail-trunc-{}.log", std::process::id()));
        std::fs::write(&path, "one\ntwo\nthree\n").unwrap();
        let rx = spawn(path.to_str().unwrap(), 0).unwrap();
        for _ in 0..3 {
            next_line(&rx);
        }

        std::fs::write(&path, "new\n").unwrap(); // shorter than before
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(3)).unwrap(),
            TailMsg::Rotated
        ));
        assert_eq!(next_line(&rx), (0, "new".to_string()));
        std::fs::remove_file(&path).unwrap();
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("tilog-rotate-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The next message, whatever it is, as `line:text` or `rotated`.
    fn next_event(rx: &Receiver<TailMsg>) -> String {
        match rx
            .recv_timeout(Duration::from_secs(4))
            .expect("no message in time")
        {
            TailMsg::Line { text, .. } => format!("line:{text}"),
            TailMsg::Rotated => "rotated".to_string(),
            TailMsg::Status(_) => "status".to_string(),
        }
    }

    #[test]
    fn a_log_replaced_by_rotation_is_followed_to_its_successor() {
        let dir = temp_dir("rename");
        let path = dir.join("app.log");
        std::fs::write(&path, "a\nb\n").unwrap();
        let rx = spawn(path.to_str().unwrap(), 0).unwrap();
        assert_eq!(
            (next_event(&rx), next_event(&rx)),
            ("line:a".into(), "line:b".into())
        );

        // What logrotate does: rename the log, create a new one in its place.
        let rotated = dir.join("app.log.1");
        std::fs::rename(&path, &rotated).unwrap();
        std::fs::write(&path, "c\nd\n").unwrap();
        // The program still has the old file open and writes to it for a moment.
        let mut old = OpenOptions::new().append(true).open(&rotated).unwrap();
        old.write_all(b"late\n").unwrap();

        // The line written to the old file is not lost, then the new file starts over.
        let seen: Vec<String> = (0..4).map(|_| next_event(&rx)).collect();
        assert_eq!(seen, ["line:late", "rotated", "line:c", "line:d"]);

        // And the new file is followed from now on.
        OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"e\n")
            .unwrap();
        assert_eq!(next_event(&rx), "line:e");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rotation_where_the_new_file_appears_a_little_later() {
        let dir = temp_dir("gap");
        let path = dir.join("app.log");
        std::fs::write(&path, "a\n").unwrap();
        let rx = spawn(path.to_str().unwrap(), 0).unwrap();
        assert_eq!(next_event(&rx), "line:a");

        // Between the rename and the new file there is no file at the path at all.
        std::fs::rename(&path, dir.join("app.log.1")).unwrap();
        thread::sleep(Duration::from_millis(1500));
        std::fs::write(&path, "fresh\n").unwrap();
        assert_eq!(
            (next_event(&rx), next_event(&rx)),
            ("rotated".into(), "line:fresh".into())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_log_that_is_deleted_and_never_returns_is_simply_left_alone() {
        let dir = temp_dir("deleted");
        let path = dir.join("app.log");
        std::fs::write(&path, "a\n").unwrap();
        let rx = spawn(path.to_str().unwrap(), 0).unwrap();
        assert_eq!(next_event(&rx), "line:a");
        std::fs::remove_file(&path).unwrap();
        // Nothing arrives, and nothing breaks: the thread keeps waiting for the file.
        assert!(rx.recv_timeout(Duration::from_millis(1500)).is_err());
        std::fs::write(&path, "back\n").unwrap();
        assert_eq!(
            (next_event(&rx), next_event(&rx)),
            ("rotated".into(), "line:back".into())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn trims_line_endings() {
        assert_eq!(trim_eol(b"abc\r\n"), b"abc");
        assert_eq!(trim_eol(b"abc\n"), b"abc");
        assert_eq!(trim_eol(b"\n"), b"");
        assert_eq!(trim_eol(b"abc"), b"abc");
    }
}
