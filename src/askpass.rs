//! Answering what `ssh` asks, inside tilog: a password, the passphrase of a key, a one-time
//! code, or "are you sure you want to continue connecting?".
//!
//! `ssh` can hand such questions to a helper program (`SSH_ASKPASS`). The helper here is tilog
//! itself, started by `ssh` with a socket to the running tilog in its environment. It sends the
//! question over the socket and prints the answer that comes back. The running tilog shows the
//! question in the tile of the source that is logging in, and the answer is typed there.
//!
//! ```text
//!   ssh ──asks──> tilog (helper) ──socket──> tilog (the app) ──shows it in the tile
//!   ssh <─answer─ tilog (helper) <─socket─── tilog (the app) <─you type it
//! ```
//!
//! The socket lives in a directory only the user can enter, and every request carries a random
//! token. A password is kept in memory only, so that a reconnect need not ask again.

use std::fmt;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::tail::TailMsg;

/// Set in the environment of `ssh` so that the helper knows where to ask.
pub const ENV_SOCKET: &str = "TILOG_ASKPASS_SOCKET";
const ENV_TOKEN: &str = "TILOG_ASKPASS_TOKEN";

/// How long a question waits for an answer before it counts as not answered. The server side of
/// a login gives up after about two minutes anyway.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(300);

/// What kind of answer `ssh` wants (`SSH_ASKPASS_PROMPT`, OpenSSH 8.4 and later).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    /// A password, a passphrase, a code: typed without being shown.
    Secret,
    /// A yes/no question (an unknown host key): typed in the open.
    Confirm,
    /// A message that needs no answer.
    Info,
}

impl Kind {
    fn code(self) -> &'static str {
        match self {
            Self::Secret => "secret",
            Self::Confirm => "confirm",
            Self::Info => "none",
        }
    }

    fn from_code(code: &str) -> Self {
        match code {
            "confirm" => Self::Confirm,
            "none" => Self::Info,
            _ => Self::Secret,
        }
    }
}

/// A question on its way to the user. The answer goes back through `answer`; dropping the
/// question without answering counts as cancelling it.
pub struct Ask {
    pub prompt: String,
    pub kind: Kind,
    reply: SyncSender<Option<String>>,
}

impl Ask {
    /// Sends the answer back to `ssh`, or `None` to cancel.
    pub fn answer(self, text: Option<String>) {
        let _ = self.reply.send(text);
    }

    /// A question and the channel its answer will come out of, for tests.
    #[cfg(test)]
    pub fn for_test(prompt: &str, kind: Kind) -> (Self, mpsc::Receiver<Option<String>>) {
        let (reply, answers) = mpsc::sync_channel(1);
        (
            Self {
                prompt: prompt.to_string(),
                kind,
                reply,
            },
            answers,
        )
    }
}

// The answer must never end up in a debug print, and it is not part of the question anyway.
impl fmt::Debug for Ask {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ask")
            .field("prompt", &self.prompt)
            .field("kind", &self.kind)
            .finish()
    }
}

/// The last password typed for a source, kept in memory so that a reconnect need not ask again.
pub type Remembered = Arc<Mutex<Option<String>>>;

/// Listens for the questions of one run of `ssh`.
pub struct Server {
    dir: PathBuf,
    token: String,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

impl Server {
    /// Starts listening. Questions arrive as `TailMsg::Ask` on `tx`. `remembered` answers the
    /// first password question of this run by itself, if a password was typed before.
    pub fn start(tx: SyncSender<TailMsg>, remembered: Remembered) -> io::Result<Self> {
        let dir = std::env::temp_dir().join(format!(
            "tilog-ask-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        // Only the user may enter: the socket inside is then theirs alone.
        fs::DirBuilder::new().mode(0o700).create(&dir)?;
        let listener = match UnixListener::bind(dir.join("ask.sock")) {
            Ok(listener) => listener,
            Err(e) => {
                let _ = fs::remove_dir_all(&dir);
                return Err(e);
            }
        };
        listener.set_nonblocking(true)?;

        let token = random_token();
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (token, stop) = (token.clone(), stop.clone());
            thread::spawn(move || serve(&listener, &token, &tx, &remembered, &stop))
        };
        Ok(Self {
            dir,
            token,
            stop,
            thread: Some(thread),
        })
    }

    /// The environment `ssh` is started with: the helper to call, and where it asks.
    pub fn env(&self) -> Vec<(String, String)> {
        let helper = std::env::current_exe().map(|p| p.to_string_lossy().into_owned());
        let mut env = vec![
            ("SSH_ASKPASS_REQUIRE".to_string(), "force".to_string()),
            (
                ENV_SOCKET.to_string(),
                self.dir.join("ask.sock").to_string_lossy().into_owned(),
            ),
            (ENV_TOKEN.to_string(), self.token.clone()),
            // Older versions of ssh only use the helper if they think there is a display.
            (
                "DISPLAY".to_string(),
                std::env::var("DISPLAY").unwrap_or_else(|_| "tilog:0".into()),
            ),
        ];
        if let Ok(helper) = helper {
            env.push(("SSH_ASKPASS".to_string(), helper));
        }
        env
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

/// A token nobody can guess: from the system's random source, with the time as a fallback.
fn random_token() -> String {
    let mut bytes = [0u8; 16];
    let filled = fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_ok();
    if !filled {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        bytes = nanos.to_le_bytes();
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The server thread: takes one question at a time (`ssh` asks one at a time).
fn serve(
    listener: &UnixListener,
    token: &str,
    tx: &SyncSender<TailMsg>,
    remembered: &Remembered,
    stop: &AtomicBool,
) {
    let mut remembered_used = false;
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let _ = handle(stream, token, tx, remembered, &mut remembered_used, stop);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(30))
            }
            Err(_) => return,
        }
    }
}

/// One question: read it, get it answered, write the answer.
fn handle(
    stream: UnixStream,
    token: &str,
    tx: &SyncSender<TailMsg>,
    remembered: &Remembered,
    remembered_used: &mut bool,
    stop: &AtomicBool,
) -> io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut line = String::new();
    BufReader::new(&stream)
        .take(16 * 1024)
        .read_line(&mut line)?;

    // `token <TAB> kind <TAB> prompt`, the prompt with its line breaks written as `\n`.
    let mut parts = line.trim_end_matches('\n').splitn(3, '\t');
    let (sent_token, kind, prompt) = (parts.next(), parts.next(), parts.next());
    let (Some(sent_token), Some(kind), Some(prompt)) = (sent_token, kind, prompt) else {
        return write_reply(&stream, None);
    };
    if sent_token != token {
        return write_reply(&stream, None);
    }
    let (kind, prompt) = (Kind::from_code(kind), prompt.replace("\\n", "\n"));

    // A message: show it, nothing to answer.
    if kind == Kind::Info {
        let _ = tx.send(TailMsg::Line {
            offset: 0,
            text: prompt,
        });
        return write_reply(&stream, Some(String::new()));
    }

    // The first password question of a run is answered with the last password typed, if any:
    // a dropped connection reconnects by itself. If that was wrong, `ssh` asks again, and then
    // the user is asked.
    if kind == Kind::Secret && !*remembered_used {
        *remembered_used = true;
        let known = remembered.lock().ok().and_then(|held| held.clone());
        if let Some(password) = known {
            return write_reply(&stream, Some(password));
        }
    }

    let (reply, answers) = mpsc::sync_channel(1);
    if tx
        .send(TailMsg::Ask(Ask {
            prompt,
            kind,
            reply,
        }))
        .is_err()
    {
        return write_reply(&stream, None); // the screen is gone
    }
    let deadline = Instant::now() + ANSWER_TIMEOUT;
    let answer = loop {
        match answers.recv_timeout(Duration::from_millis(100)) {
            Ok(answer) => break answer,
            Err(RecvTimeoutError::Disconnected) => break None, // the tile was closed
            Err(RecvTimeoutError::Timeout)
                if stop.load(Ordering::SeqCst) || Instant::now() > deadline =>
            {
                break None;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
    };
    if kind == Kind::Secret
        && let Some(password) = &answer
        && let Ok(mut held) = remembered.lock()
    {
        *held = Some(password.clone());
    }
    write_reply(&stream, answer)
}

/// `OK` and the answer, or `CANCEL`.
fn write_reply(mut stream: &UnixStream, answer: Option<String>) -> io::Result<()> {
    match answer {
        Some(text) => {
            stream.write_all(b"OK\n")?;
            stream.write_all(text.as_bytes())
        }
        None => stream.write_all(b"CANCEL\n"),
    }
}

/// Asks the running tilog at `socket`. `None` if the question was cancelled or refused.
pub fn ask(socket: &Path, token: &str, kind: Kind, prompt: &str) -> io::Result<Option<String>> {
    let mut stream = UnixStream::connect(socket)?;
    let prompt = prompt.replace('\n', "\\n").replace('\t', " ");
    stream.write_all(format!("{token}\t{}\t{prompt}\n", kind.code()).as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;

    let mut reply = Vec::new();
    stream.read_to_end(&mut reply)?;
    Ok(reply
        .strip_prefix(b"OK\n")
        .map(|answer| String::from_utf8_lossy(answer).into_owned()))
}

/// What tilog does when `ssh` starts it as its helper: ask, print the answer, exit. The
/// question is `argv[1]`. Returns the exit code.
pub fn run_helper() -> i32 {
    let prompt = std::env::args().nth(1).unwrap_or_default();
    let kind = Kind::from_code(&std::env::var("SSH_ASKPASS_PROMPT").unwrap_or_default());
    let (Some(socket), Some(token)) = (std::env::var_os(ENV_SOCKET), std::env::var(ENV_TOKEN).ok())
    else {
        return 2;
    };
    match ask(Path::new(&socket), &token, kind, &prompt) {
        Ok(Some(answer)) => {
            println!("{answer}");
            0
        }
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::Receiver;

    fn server() -> (Server, Receiver<TailMsg>, Remembered) {
        let (tx, rx) = mpsc::sync_channel(8);
        let remembered = Remembered::default();
        (
            Server::start(tx, remembered.clone()).unwrap(),
            rx,
            remembered,
        )
    }

    fn client(server: &Server, kind: Kind, prompt: &str) -> JoinHandle<io::Result<Option<String>>> {
        let env = server.env();
        let get = |name: &str| env.iter().find(|(k, _)| k == name).unwrap().1.clone();
        let (socket, token, prompt) = (get(ENV_SOCKET), get(ENV_TOKEN), prompt.to_string());
        thread::spawn(move || ask(Path::new(&socket), &token, kind, &prompt))
    }

    fn next_ask(rx: &Receiver<TailMsg>) -> Ask {
        match rx.recv_timeout(Duration::from_secs(3)).expect("a question") {
            TailMsg::Ask(ask) => ask,
            other => panic!("expected a question, got {other:?}"),
        }
    }

    #[test]
    fn a_question_reaches_the_screen_and_the_answer_reaches_ssh() {
        let (server, rx, remembered) = server();
        let helper = client(&server, Kind::Secret, "deploy@web1's password: ");
        let ask = next_ask(&rx);
        assert_eq!(
            (ask.prompt.as_str(), ask.kind),
            ("deploy@web1's password: ", Kind::Secret)
        );
        ask.answer(Some("hunter 2\twith tab".to_string()));
        assert_eq!(
            helper.join().unwrap().unwrap().as_deref(),
            Some("hunter 2\twith tab")
        );
        assert_eq!(
            remembered.lock().unwrap().as_deref(),
            Some("hunter 2\twith tab")
        );
    }

    #[test]
    fn a_multi_line_question_keeps_its_lines() {
        let (server, rx, _) = server();
        let text = "The authenticity of host 'web1' can't be established.\nContinue (yes/no)? ";
        let helper = client(&server, Kind::Confirm, text);
        let ask = next_ask(&rx);
        assert_eq!(ask.prompt, text);
        ask.answer(Some("yes".into()));
        assert_eq!(helper.join().unwrap().unwrap().as_deref(), Some("yes"));
    }

    #[test]
    fn cancelling_or_closing_the_tile_answers_nothing() {
        let (server, rx, remembered) = server();
        let helper = client(&server, Kind::Secret, "password: ");
        next_ask(&rx).answer(None);
        assert_eq!(helper.join().unwrap().unwrap(), None);

        let helper = client(&server, Kind::Secret, "password: ");
        drop(next_ask(&rx)); // the tile was closed
        assert_eq!(helper.join().unwrap().unwrap(), None);
        assert!(
            remembered.lock().unwrap().is_none(),
            "nothing was typed, nothing is kept"
        );
    }

    #[test]
    fn a_remembered_password_answers_once_then_the_user_is_asked_again() {
        let (server, rx, remembered) = server();
        *remembered.lock().unwrap() = Some("old".to_string());

        // The first question is answered from memory, without bothering the screen.
        let helper = client(&server, Kind::Secret, "password: ");
        assert_eq!(helper.join().unwrap().unwrap().as_deref(), Some("old"));
        assert!(rx.try_recv().is_err());

        // `ssh` asks again (it was wrong): now the user is asked, and the new one is kept.
        let helper = client(&server, Kind::Secret, "password: ");
        next_ask(&rx).answer(Some("new".into()));
        assert_eq!(helper.join().unwrap().unwrap().as_deref(), Some("new"));
        assert_eq!(remembered.lock().unwrap().as_deref(), Some("new"));
    }

    #[test]
    fn messages_are_shown_as_lines_and_a_wrong_token_is_refused() {
        let (server, rx, _) = server();
        let helper = client(&server, Kind::Info, "Authenticated to web1");
        assert_eq!(helper.join().unwrap().unwrap().as_deref(), Some(""));
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(3)),
            Ok(TailMsg::Line { text, .. }) if text == "Authenticated to web1"
        ));

        let env = server.env();
        let socket = &env.iter().find(|(k, _)| k == ENV_SOCKET).unwrap().1;
        let answer = ask(
            Path::new(socket),
            "not the token",
            Kind::Secret,
            "password: ",
        )
        .unwrap();
        assert_eq!(answer, None);
        assert!(rx.try_recv().is_err(), "nothing was shown");
    }

    #[test]
    fn the_socket_directory_is_private_and_goes_away_with_the_server() {
        use std::os::unix::fs::PermissionsExt;
        let (server, _rx, _) = server();
        let dir = server.dir.clone();
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        drop(server);
        assert!(!dir.exists());
    }
}
