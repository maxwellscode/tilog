# Contributing to tilog

tilog is **`tig` for logs**: a small, fast, read-only terminal viewer for live logs during a
deployment and for checking production errors afterwards. Changes are welcome when they fit that;
please open an issue first for anything bigger than a fix.

## What belongs in tilog

- It feels like `less`, `head` and `tail`: a new key should look like it could have been in `less`.
- It works with no configuration. Config is an optional extra, never a requirement.
- It only reads: it never changes a log or a server, and installs nothing on remote hosts.
- It stays small and fast, and memory stays bounded whatever the input.

What it is **not**: log storage, indexing, alerting, dashboards, format parsing or SQL (use
[lnav](https://lnav.org) for that), agents or accounts, a plugin system, native Windows (WSL
works for now).

## Building and testing

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all
```

All of them must pass; CI runs the same on Linux and macOS. The minimum Rust version is 1.88.

The tests read the logs in `examples/`, so keep them as they are. To watch tilog with logs that
grow, run `examples/live.sh` (it writes to a copy, see [`examples/README.md`](examples/README.md)).

A change that affects the screen or the keyboard should be tried in a real terminal.
`tmux capture-pane -p -e` shows what is drawn and `tmux send-keys` types into it; use a tmux
server of your own (`tmux -L name`) so that yours is not touched. `docs/demo/make_demo.py` does
exactly that and makes the GIF in the README (it needs Pillow and a monospace font).

The manual page, `man/tilog.1`, is written by hand and built into the program (`tilog --print-man`).
A test checks that it lists every command and option, so a new one fails the build until the page
says something about it. Check the page with `mandoc -T lint -W warning man/tilog.1`.

A change that touches reading, buffers or drawing should keep the numbers in the README's
*Small and fast* section true: re-measure with a release build.

## Dev container

`.devcontainer/` has a Linux environment with Rust, tmux and ssh for VS Code and GitHub
Codespaces. Open the folder in a container and run the commands above.

## Code

- The code is organised by topic. Each module starts with a `//!` comment saying what it is for;
  `src/tile/` is one window, `src/app/` is the application (keys, drawing, commands).
- Keep files focused: roughly 500 lines of production code at most. Tests live next to the code.
- Say *why* in comments, not what. Prefer a small test that shows the bug over a long explanation.

## Commits

One logical change per commit, with a short imperative subject (`perf: redraw only when
something changed`) and a body that says what changed and why. Each commit should build and pass
the tests on its own.
