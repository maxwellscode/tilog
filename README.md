# tilog

A tiling TUI for real-time log monitoring, with per-tile filters.

Watch several live log streams side by side, and open a filter on any of them with one key.
A source is a file or any command whose output is lines, so SSH, Docker, Kubernetes and
`journalctl` work the same way as a local file.

## Usage

```sh
tilog app.log nginx.log                      # local files
tilog ssh:deploy@web1:/var/log/app.log       # remote file (reconnects when the link drops)
tilog docker:api kube:prod/web-0 cmd:'journalctl -fu app'
tilog -s deploy                              # a saved session
```

Keys follow `less`: `j k Space b g G` scroll, `/` searches, `&` filters, `:` runs a command
(`:add`, `:merge`, `:save`, ...), `?` shows all keys, `1`-`9` open a source, `q` goes back.
In a filter pane, `n` / `N` step through its matches and the main pane shows each one in
its place in the file.

Colors and named sources live in `~/.config/tilog/config.toml`
(`tilog --print-config` prints the defaults). Example logs and a live writer for trying
it out are in [`examples/`](examples/).

## Platforms

Linux and macOS. On Windows, use WSL: the Linux build runs there unchanged. A native Windows
build is not supported yet (process handling and file identity use Unix APIs).

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --all --check
```

The code is organised by topic; each module starts with a `//!` comment saying what it is for.
`src/tile/` is one window, `src/app/` is the application (keys, drawing, commands).

## License

MIT, see [LICENSE](LICENSE).
