# Changelog

All notable changes are listed here. The format follows [Keep a Changelog](https://keepachangelog.com),
and versions follow [Semantic Versioning](https://semver.org).

## [Unreleased]

The first public version.

### Added

- Sources: files, standard input (`kubectl logs -f pod | tilog`), `ssh:host:/path`, `docker:name`,
  `kube:pod`, `cmd:...`, compressed `.gz` files, and docker or kubectl on another host over ssh
  (`ssh:host:docker:name`). Named sources in `~/.config/tilog/config.toml`.
- Tabs per source and an overview; a merged timeline of several sources by timestamp; saved sessions.
- A filter tile per source, live and file-backed, with whole entries for stack traces; `n` / `N`
  step through its matches and the main pane shows each one in its place in the file.
- Prompt history: Up and Down (or Ctrl+P and Ctrl+N) bring back earlier searches, filters and
  commands.
- The matches box of Tab shows the folder it lists (the sessions folder for `:load` and `:save`), and
  `:sessions` says where the sessions are saved.
- Exclude filters: `&!text` or `:filter -v text` keep the entries without the text.
- Colors and other terminal escape sequences in the logs are removed as lines are read.
- Search with highlight, `]` / `[` between error lines, `:goto` a line or a time, `:write` lines to
  a file, `:pause` and `:follow` for one tile or all.
- Reconnect with backoff for dropped commands, log rotation, a source limit of nine.
- SSH login questions (password, passphrase, code, host key) answered in the tile of the source.
- Level coloring for common formats (Java, logfmt, JSON, nginx, klog, ...), configurable in TOML;
  `NO_COLOR` is honored. Mouse selection and copy.
- A manual page (`man/tilog.1`), which the program carries: `tilog --print-man`.
- A small binary (about 3 MB) with bounded memory: lines are cut at 64 KiB, a source holds at most
  32 MiB of text, and the screen is only redrawn when something changed.
