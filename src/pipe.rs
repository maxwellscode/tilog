//! Reading a log from standard input: `kubectl logs -f pod | tilog`.
//!
//! A pipe has no file behind it: nothing older can be read again, and it does not come back
//! when it ends. So it is a source like a command, minus the restarting: lines arrive until the
//! writer closes the pipe, and then the source says so.

use std::io::{self, BufRead, BufReader, IsTerminal};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread;

use anyhow::{Result, bail};

use crate::tail::{Status, TailMsg, trim_eol};

/// How many lines may wait for the UI before the reader slows down (and, through the pipe, the
/// program that writes to it). The same limit the other sources use.
const CHANNEL_BOUND: usize = 4096;

/// There is only one standard input, so it can be a source only once.
static TAKEN: AtomicBool = AtomicBool::new(false);

/// Starts reading standard input. Lines and the end of the input arrive on the channel.
pub fn spawn() -> Result<Receiver<TailMsg>> {
    if io::stdin().is_terminal() {
        bail!("stdin is a terminal: pipe a log into tilog, e.g. `kubectl logs -f pod | tilog`");
    }
    if TAKEN.swap(true, Ordering::SeqCst) {
        bail!("stdin can be read once, and it was used already");
    }
    let (tx, rx) = mpsc::sync_channel(CHANNEL_BOUND);
    thread::spawn(move || read_lines(BufReader::with_capacity(64 * 1024, io::stdin()), &tx));
    Ok(rx)
}

/// Sends every line of `input`, then says the input ended. A last line without a newline is a
/// line too: a pipe has nothing more to wait for, unlike a file that is still being written.
fn read_lines(mut input: impl BufRead, tx: &SyncSender<TailMsg>) {
    let (mut offset, mut buf) = (0u64, Vec::new());
    let why = loop {
        buf.clear();
        match input.read_until(b'\n', &mut buf) {
            Ok(0) => break "input closed".to_string(),
            Ok(read) => {
                let text = String::from_utf8_lossy(trim_eol(&buf)).into_owned();
                if tx.send(TailMsg::Line { offset, text }).is_err() {
                    return; // the UI is gone
                }
                offset += read as u64;
            }
            Err(e) => break format!("cannot read stdin: {e}"),
        }
    };
    let _ = tx.send(TailMsg::Status(Status::Ended { why }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn collect(bytes: &[u8]) -> Vec<TailMsg> {
        let (tx, rx) = mpsc::sync_channel(100);
        read_lines(Cursor::new(bytes.to_vec()), &tx);
        drop(tx);
        rx.iter().collect()
    }

    #[test]
    fn every_line_arrives_then_the_end_is_reported() {
        let messages = collect(b"one\r\ntwo\nlast without newline");
        let lines: Vec<(u64, &str)> = messages
            .iter()
            .filter_map(|m| match m {
                TailMsg::Line { offset, text } => Some((*offset, text.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(lines, [(0, "one"), (5, "two"), (9, "last without newline")]);
        assert!(matches!(
            messages.last(),
            Some(TailMsg::Status(Status::Ended { why })) if why == "input closed"
        ));
    }

    #[test]
    fn invalid_utf8_is_replaced_and_empty_input_just_ends() {
        let messages = collect(b"ok \xff\xfe bad\n");
        assert!(
            matches!(&messages[0], TailMsg::Line { text, .. } if text == "ok \u{fffd}\u{fffd} bad")
        );
        assert_eq!(collect(b"").len(), 1, "only the end");
    }
}
