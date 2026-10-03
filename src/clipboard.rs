//! Copying text to the system clipboard.
//!
//! On the local machine the platform's own tool does it (`pbcopy`, `wl-copy`, `xclip`, `xsel`).
//! Over ssh those would fill the clipboard of the *remote* machine, so there, and anywhere no
//! tool is found, the terminal is asked to do it with an OSC 52 escape sequence, which most
//! modern terminals (iTerm2, kitty, WezTerm, Alacritty, tmux with `set-clipboard on`) honor.

use std::env;
use std::io::{self, Write};
use std::process::{Command, Stdio};

use anyhow::{Result, bail};

/// How the text got to the clipboard, for the message shown to the user.
pub fn copy(text: &str) -> Result<&'static str> {
    if text.is_empty() {
        bail!("nothing selected");
    }
    let over_ssh = env::var_os("SSH_CONNECTION").is_some() || env::var_os("SSH_TTY").is_some();
    if !over_ssh {
        for (program, args) in local_tools() {
            if pipe_to(program, args, text).is_ok() {
                return Ok(program);
            }
        }
    }
    // The terminal's own clipboard. There is no answer to wait for: if the terminal ignores it,
    // nothing happens, and this is as far as we can tell.
    let mut stdout = io::stdout();
    stdout.write_all(osc52(text).as_bytes())?;
    stdout.flush()?;
    Ok("the terminal")
}

/// The clipboard programs to try on this system, best first.
fn local_tools() -> Vec<(&'static str, &'static [&'static str])> {
    if cfg!(target_os = "macos") {
        return vec![("pbcopy", &[])];
    }
    let mut tools: Vec<(&'static str, &'static [&'static str])> = Vec::new();
    if env::var_os("WAYLAND_DISPLAY").is_some() {
        tools.push(("wl-copy", &[]));
    }
    tools.push(("xclip", &["-selection", "clipboard"]));
    tools.push(("xsel", &["--clipboard", "--input"]));
    tools
}

/// Runs `program` with `text` on its stdin.
fn pipe_to(program: &str, args: &[&str], text: &str) -> Result<()> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes())?;
    } // closing stdin tells the program the text is complete
    if !child.wait()?.success() {
        bail!("{program} failed");
    }
    Ok(())
}

/// The escape sequence that asks a terminal to put `text` on the clipboard.
fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

/// Standard base64 with padding. Small enough to write here instead of adding a dependency.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        // Up to three bytes make one 24-bit group, written as four 6-bit characters.
        let group = chunk.iter().enumerate().fold(0u32, |acc, (i, &byte)| {
            acc | u32::from(byte) << (16 - 8 * i)
        });
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(
                    ALPHABET[(group >> (18 - 6 * i) & 0x3f) as usize],
                ));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_the_rfc_4648_examples() {
        for (plain, encoded) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(plain.as_bytes()), encoded, "for {plain:?}");
        }
        assert_eq!(base64("ä€".as_bytes()), "w6Tigqw=");
    }

    #[test]
    fn the_osc52_sequence_has_the_right_shape() {
        assert_eq!(osc52("hi"), "\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn nothing_selected_is_an_error() {
        assert!(copy("").is_err());
    }

    #[test]
    fn a_missing_tool_is_an_error_not_a_panic() {
        assert!(pipe_to("tilog-no-such-clipboard-tool", &[], "x").is_err());
    }
}
