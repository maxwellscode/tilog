# Example logs

Sample logs of common applications, to try tilog without a server.

```
tilog examples/*.log
```

Most of them tell **one story**, so `:merge` shows it in order. Around 08:16:50 on 2026-10-03:

1. A nightly export in the shop app leaks database connections (`example_spring_boot.log`).
2. Postgres runs out of connection slots (`example_postgres.log`).
3. The app's connection pool times out and orders fail (`example_spring_boot.log`).
4. nginx answers 504, then 502 while the app is down (`example_nginx_access.log`, `example_nginx_error.log`).
5. systemd's watchdog restarts the service (`example_syslog.log`).
6. Everything comes back at about 08:17:21.

| File | Application | Format worth noticing |
|---|---|---|
| `example_nginx_access.log` | nginx access log | `[03/Oct/2026:08:14:00 +0000]`, status codes, `rt=` request time |
| `example_nginx_error.log` | nginx error log | `2026/10/03 08:14:00 [error] …` |
| `example_spring_boot.log` | Spring Boot (Logback) | Java stack traces with `Caused by:` and `... 58 common frames omitted` |
| `example_postgres.log` | PostgreSQL | multi-line SQL (tab-indented), `ERROR` / `DETAIL` / `STATEMENT` |
| `example_syslog.log` | Linux syslog (sshd, cron, systemd) | `Oct  3 08:14:01 host proc[pid]: …`, no year |
| `example_redis.log` | Redis | `1:M 03 Oct 2026 08:14:00.002 * …` |
| `example_tomcat.log` | Apache Tomcat | `03-Oct-2026 08:14:02.113 INFO [main] …` |
| `example_python_gunicorn.log` | gunicorn + Django | chained Python tracebacks |
| `example_nodejs_json.log` | Node.js (pino-style JSON lines) | one JSON object per line, long lines to scroll sideways |
| `example_pino_epoch.log` | Node.js with pino's defaults | numeric time in epoch milliseconds, level as a number |
| `example_kubernetes.log` | a Go service, `kubectl logs --timestamps` | nanosecond timestamps, logfmt, a Go panic |

Things to try:

- `:merge` → one timeline of all sources. `/remaining connection` jumps to the moment Postgres runs out; `n` / `N` step through the matches.
- In `example_spring_boot.log`: `&Caused by` → a tile with the whole stack trace, not just one line.
- In `example_postgres.log`: `&duplicate key` finds the error line, but `DETAIL` and `STATEMENT` are lines of their own. `&[5290]` (the backend id in brackets) shows all three together.
- `example_nginx_access.log`: `&-r " 50[24] "` → only the failed requests.
- `example_kubernetes.log` has a timestamp on every line, so a panic is many entries. `:group ^2026-.* level=` makes only the `level=` lines start an entry, and the panic joins the entry above it.
- `l` / `h` (or Shift + mouse wheel) scroll the long JSON lines sideways.
- `tilog -c 'tail -f examples/example_nginx_access.log'` follows a command instead of a file.

## Watching logs grow: `live.sh`

`examples/live.sh` appends new, correctly formatted lines (current UTC time) to the example logs, so you can try tilog's real-time features: following, filters that fill up, `:merge`, a stack trace arriving while you watch.

```
examples/live.sh 30                run for 30 seconds
examples/live.sh 60 --rate 10      about 10 lines per second (default 4)
examples/live.sh 30 --only nginx   only files whose name contains "nginx"
examples/live.sh 30 --restore      cut the files back to their old size at the end
examples/live.sh                   run until Ctrl+C
timeout 30s examples/live.sh       the same, with `timeout` (Linux, or `brew install coreutils`)
```

Start it in one terminal and `tilog examples/*.log` in another. Without `--restore` the example files keep the new lines (tilog's tests read them, and they stay valid either way); with it, the files end byte-for-byte as they were, and tilog shows the "file got shorter" reset when that happens.
