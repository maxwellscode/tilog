#!/usr/bin/env bash
#
# Writes new, realistic log lines into the example logs while you watch, to try tilog's
# real-time features: following, filters, search, :merge, stack traces arriving live.
#
#   examples/live.sh 30                  run for 30 seconds (writes to a copy, see below)
#   examples/live.sh 60 --rate 10        ... about 10 lines per second (default 4)
#   examples/live.sh 30 --only nginx     only files whose name contains "nginx"
#   examples/live.sh 30 --restore        cut the files back to their old size when done
#   examples/live.sh 30 --dir /tmp/logs  write to this directory instead
#   examples/live.sh                     run until Ctrl+C
#   timeout 30s examples/live.sh         the same with `timeout` (Linux; `brew install coreutils`)
#
# Every line has the format of the example it is written to, with the current time in UTC.
# Entries that span several lines (stack traces, SQL) are written in one piece.
#
# The lines go to a copy of the example logs, in $TMPDIR/tilog-live (made on the first run), so
# the logs in the repository stay as they are: the tests read them. The copy keeps what was
# written; delete the directory to start over, or use --restore to cut the files back to their
# size at the start of each run. `--dir examples` writes into the repository's own logs.

set -u
export LC_ALL=C # month names and number formats must not depend on the user's locale

HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
DIR=${TMPDIR:-/tmp}/tilog-live
DIR=${DIR//\/\//\/} # TMPDIR may end in a slash
DURATION=0
RATE=4
ONLY=""
RESTORE=0
QUIET=0

usage() {
  sed -n '3,17p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
  echo "options:  --rate N   lines per second (1-50)      --only TEXT   file name filter"
  echo "          --dir DIR  write somewhere else         --restore     undo at the end"
  echo "          -q         no line-by-line output"
}

# ---- arguments ------------------------------------------------------------------------------

while [ $# -gt 0 ]; do
  case $1 in
    -h|--help) usage; exit 0 ;;
    --rate)    RATE=${2:?--rate needs a number}; shift ;;
    --only)    ONLY=${2:?--only needs a text}; shift ;;
    --dir)     DIR=${2:?--dir needs a directory}; shift ;;
    --restore) RESTORE=1 ;;
    -q)        QUIET=1 ;;
    -*)        echo "unknown option: $1 (try --help)" >&2; exit 2 ;;
    *)         DURATION=$1 ;;
  esac
  shift
done

case $DURATION in ''|*[!0-9]*) echo "the duration is a number of seconds, got: $DURATION" >&2; exit 2 ;; esac
case $RATE in ''|*[!0-9]*) echo "--rate is a number, got: $RATE" >&2; exit 2 ;; esac
[ "$RATE" -ge 1 ] && [ "$RATE" -le 50 ] || { echo "--rate must be between 1 and 50" >&2; exit 2; }

# ---- helpers --------------------------------------------------------------------------------

# pick A B C: one of the arguments, at random.
pick() { shift $((RANDOM % $#)); printf '%s' "$1"; }

# The time of the line being written, in every format the examples use, from ONE `date` call
# per line (refresh_time), and a millisecond that only ever grows within a second. A random
# millisecond would give two lines written in the same second times that go backwards, and a
# merged timeline would rightly put them in that order.
T_EPOCH=0; LAST_SEC=0; LAST_MS=0; MS=0
refresh_time() {
  IFS='|' read -r T_EPOCH T_ISO T_NGINX T_NGINX_ERROR T_PLAIN T_SYSLOG T_REDIS T_TOMCAT <<EOF_TIME
$(date -u +'%s|%Y-%m-%dT%H:%M:%S|%d/%b/%Y:%H:%M:%S|%Y/%m/%d %H:%M:%S|%Y-%m-%d %H:%M:%S|%b %e %H:%M:%S|%d %b %Y %H:%M:%S|%d-%b-%Y %H:%M:%S')
EOF_TIME
  if [ "$T_EPOCH" = "$LAST_SEC" ]; then
    MS=$((LAST_MS + 3 + RANDOM % 90)); [ "$MS" -le 999 ] || MS=999
  else
    MS=$((RANDOM % 150))
  fi
  LAST_SEC=$T_EPOCH; LAST_MS=$MS
}

# now FORMAT: the current UTC time (the formats the generators use were fetched by refresh_time).
now() {
  case $1 in
    '%s')                  printf '%s' "$T_EPOCH" ;;
    '%Y-%m-%dT%H:%M:%S')   printf '%s' "$T_ISO" ;;
    '%d/%b/%Y:%H:%M:%S')   printf '%s' "$T_NGINX" ;;
    '%Y/%m/%d %H:%M:%S')   printf '%s' "$T_NGINX_ERROR" ;;
    '%Y-%m-%d %H:%M:%S')   printf '%s' "$T_PLAIN" ;;
    '%b %e %H:%M:%S')      printf '%s' "$T_SYSLOG" ;;
    '%d %b %Y %H:%M:%S')   printf '%s' "$T_REDIS" ;;
    '%d-%b-%Y %H:%M:%S')   printf '%s' "$T_TOMCAT" ;;
    *)                     date -u +"$1" ;;
  esac
}

# ms: the millisecond of the line being written (three digits).
ms() { printf '%03d' "$MS"; }

# chance PERCENT: true that often.
chance() { [ $((RANDOM % 100)) -lt "$1" ]; }

customer() { printf 'c-%d' $((1000 + RANDOM % 4000)); }
total() { printf '%d.%02d' $((RANDOM % 300)) $((RANDOM % 100)); }
nl=$'\n'

AGENTS=(
  'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Chrome/118.0 Safari/537.36'
  'Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 Mobile/15E148'
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Gecko/20100101 Firefox/118.0'
  'curl/8.4.0'
  'Googlebot/2.1 (+http://www.google.com/bot.html)'
  'kube-probe/1.28'
)

# Counters that grow over the run. The generators below set LINE (not print it), because a
# generator called inside $(...) would lose these.
ORDER=$((10600 + RANDOM % 50))
CONN=$((2000 + RANDOM % 500))
REPORT=$((60 + RANDOM % 20))

# ---- one generator per example file: each sets LINE, possibly over several lines -----------

gen_nginx_access() {
  local path method=GET status=200 size rt
  path=$(pick /api/products /api/cart /api/cart/items /api/orders /login '/api/search?q=shoes' \
    /static/app.js /static/app.css / /favicon.ico "/api/products/$((100 + RANDOM % 900))")
  case $path in /api/orders|/login|/api/cart/items) chance 70 && method=POST ;; esac
  size=$((300 + RANDOM % 9000))
  case $path in /static/*) size=$((20000 + RANDOM % 70000)) ;; esac
  printf -v rt '0.%03d' $((RANDOM % 90))
  if chance 4; then status=500 size=$((150 + RANDOM % 100)); rt="1.$((RANDOM % 9))$((RANDOM % 9))$((RANDOM % 9))"
  elif chance 5; then status=404 size=153
  elif chance 6; then status=301 size=169
  fi
  LINE="$(pick 10.0.1.4 10.0.1.17 10.0.1.23 10.0.2.88 10.0.2.130 203.0.113.9 198.51.100.77) - - [$(now '%d/%b/%Y:%H:%M:%S') +0000] \"$method $path HTTP/1.1\" $status $size \"-\" \"$(pick "${AGENTS[@]}")\" rt=$rt"
}

gen_nginx_error() {
  CONN=$((CONN + 1))
  local ts worker client request
  ts=$(now '%Y/%m/%d %H:%M:%S'); worker=$(pick 29 30)
  client="10.0.$((1 + RANDOM % 2)).$(pick 4 17 23 88 130)"
  request=$(pick '/api/products' '/api/cart' '/api/orders' '/api/search?q=shoes')
  if chance 45; then
    LINE="$ts [error] $worker#$worker: *$CONN open() \"/usr/share/nginx/html/favicon.ico\" failed (2: No such file or directory), client: $client, server: shop.example.com, request: \"GET /favicon.ico HTTP/1.1\", host: \"shop.example.com\""
  elif chance 40; then
    LINE="$ts [warn] $worker#$worker: *$CONN an upstream response is buffered to a temporary file /var/cache/nginx/proxy_temp/3/00/0000000003 while reading upstream, client: $client, server: shop.example.com, request: \"GET $request HTTP/1.1\", upstream: \"http://127.0.0.1:8080$request\", host: \"shop.example.com\""
  elif chance 60; then
    LINE="$ts [error] $worker#$worker: *$CONN upstream timed out (110: Connection timed out) while reading response header from upstream, client: $client, server: shop.example.com, request: \"GET $request HTTP/1.1\", upstream: \"http://127.0.0.1:8080$request\", host: \"shop.example.com\""
  else
    LINE="$ts [error] $worker#$worker: *$CONN connect() failed (111: Connection refused) while connecting to upstream, client: $client, server: shop.example.com, request: \"GET $request HTTP/1.1\", upstream: \"http://127.0.0.1:8080$request\", host: \"shop.example.com\""
  fi
}

# One Spring Boot (Logback) line: spring LEVEL THREAD LOGGER MESSAGE
spring_line() {
  local thread logger
  printf -v thread '[%15s]' "$3"
  printf -v logger '%-40s' "$4"
  printf '%s %5s 1 --- %s %s : %s' "$(now '%Y-%m-%dT%H:%M:%S').$(ms)Z" "$2" "$thread" "$logger" "$5"
}

gen_spring_boot() {
  local thread="nio-8080-exec-$((1 + RANDOM % 12))" order
  ORDER=$((ORDER + 1)); order=$ORDER
  if chance 70; then
    LINE=$(spring_line x INFO "$thread" c.e.shop.web.OrderController "Order created id=$order customer=$(customer) total=$(total)")
  elif chance 35; then
    LINE=$(spring_line x DEBUG "$thread" o.s.web.servlet.DispatcherServlet "GET \"/api/products\", parameters={}")
  elif chance 35; then
    LINE=$(spring_line x WARN "$thread" c.e.shop.service.PaymentService "Payment gateway slow response: $((1500 + RANDOM % 2500)) ms (threshold 1500 ms)")
  elif chance 50; then
    LINE=$(spring_line x INFO scheduling-1 c.e.shop.service.InventorySync "Synced $((1000 + RANDOM % 500)) stock levels in $((200 + RANDOM % 300)) ms")
  else
    # An error with a stack trace: one entry, five lines.
    LINE="$(spring_line x ERROR "$thread" c.e.shop.service.PaymentService "Payment failed for order id=$order")"
    LINE+="${nl}java.net.SocketTimeoutException: Read timed out"
    LINE+="${nl}"$'\t'"at com.example.shop.client.GatewayClient.post(GatewayClient.java:88) ~[classes!/:1.4.2]"
    LINE+="${nl}"$'\t'"at com.example.shop.service.PaymentService.charge(PaymentService.java:54) ~[classes!/:1.4.2]"
    LINE+="${nl}Caused by: java.net.SocketException: Connection reset"
    LINE+="${nl}"$'\t'"... $((20 + RANDOM % 40)) common frames omitted"
  fi
}

gen_postgres() {
  local pid=$((4900 + RANDOM % 500)) ts
  ts() { printf '%s.%s UTC' "$(now '%Y-%m-%d %H:%M:%S')" "$(ms)"; }
  if chance 50; then
    LINE="$(ts) [$pid] LOG:  duration: $((RANDOM % 900)).$((RANDOM % 900)) ms  statement: SELECT count(*) FROM orders WHERE created_at > now() - interval '1 day'"
  elif chance 40; then
    LINE="$(ts) [$pid] LOG:  connection received: host=10.0.3.5 port=$((50000 + RANDOM % 9000))"
    LINE+="${nl}$(ts) [$pid] LOG:  connection authorized: user=shop database=shop"
  elif chance 40; then
    # Multi-line SQL: the continuation lines are indented with a tab.
    LINE="$(ts) [$pid] LOG:  duration: $((1000 + RANDOM % 2500)).$((RANDOM % 900)) ms  execute <unnamed>: SELECT o.id, o.total, c.name"
    LINE+="${nl}"$'\t'"FROM orders o"
    LINE+="${nl}"$'\t'"JOIN customers c ON c.id = o.customer_id"
    LINE+="${nl}"$'\t'"WHERE o.status = \$1"
    LINE+="${nl}"$'\t'"ORDER BY o.created_at DESC"
    LINE+="${nl}$(ts) [$pid] DETAIL:  parameters: \$1 = 'PENDING'"
  elif chance 55; then
    LINE="$(ts) [$pid] ERROR:  duplicate key value violates unique constraint \"orders_pkey\""
    LINE+="${nl}$(ts) [$pid] DETAIL:  Key (id)=($((10000 + RANDOM % 900))) already exists."
    LINE+="${nl}$(ts) [$pid] STATEMENT:  INSERT INTO orders (id, customer_id, total) VALUES (\$1, \$2, \$3)"
  elif chance 50; then
    LINE="$(ts) [27] LOG:  checkpoint starting: time"
    LINE+="${nl}$(ts) [27] LOG:  checkpoint complete: wrote $((50 + RANDOM % 150)) buffers (0.$((RANDOM % 9))%); 0 WAL file(s) added, 0 removed, 0 recycled; write=6.$((RANDOM % 9)) s, sync=0.00$((RANDOM % 9)) s, total=6.$((RANDOM % 9)) s"
  else
    LINE="$(ts) [$pid] FATAL:  remaining connection slots are reserved for non-replication superuser connections"
  fi
}

gen_syslog() {
  local ts pid=$((8000 + RANDOM % 900))
  ts=$(now '%b %e %H:%M:%S')
  if chance 30; then
    LINE="$ts shop-1 CRON[$pid]: (root) CMD (/usr/local/bin/rotate-logs.sh)"
  elif chance 35; then
    LINE="$ts shop-1 sshd[$pid]: Accepted publickey for deploy from 10.0.9.$((1 + RANDOM % 9)) port $((40000 + RANDOM % 20000)) ssh2: ED25519 SHA256:q8Zs0Vt1x2n7B0Yq2kQ0m1x7r3w5P0b8c6d4e2f1a3g"
    LINE+="${nl}$ts shop-1 sshd[$pid]: pam_unix(sshd:session): session opened for user deploy(uid=1001) by (uid=0)"
  elif chance 40; then
    LINE="$ts shop-1 sshd[$pid]: Failed password for invalid user admin from 203.0.113.$((1 + RANDOM % 200)) port $((40000 + RANDOM % 20000)) ssh2"
  elif chance 40; then
    LINE="$ts shop-1 sudo:   deploy : TTY=pts/0 ; PWD=/home/deploy ; USER=root ; COMMAND=/usr/bin/journalctl -u shop -n 100"
    LINE+="${nl}$ts shop-1 sudo: pam_unix(sudo:session): session opened for user root(uid=0) by deploy(uid=1001)"
  elif chance 50; then
    LINE="$ts shop-1 kernel: [$((412000 + RANDOM % 900)).$((RANDOM % 900000))] TCP: request_sock_TCP: Possible SYN flooding on port 8080. Sending cookies.  Check SNMP counters."
  else
    LINE="$ts shop-1 systemd[1]: Started Session $((200 + RANDOM % 100)) of User deploy."
  fi
}

gen_redis() {
  local ts child=$((60 + RANDOM % 80))
  ts() { printf '%s.%s' "$(now '%d %b %Y %H:%M:%S')" "$(ms)"; }
  if chance 45; then
    LINE="1:M $(ts) * Accepted 10.0.3.5:$((50000 + RANDOM % 9000))"
  elif chance 45; then
    # A background save: four lines from two processes (the server and its child).
    LINE="1:M $(ts) * $((100 + RANDOM % 9000)) changes in 300 seconds. Saving..."
    LINE+="${nl}1:M $(ts) * Background saving started by pid $child"
    LINE+="${nl}$child:C $(ts) * DB saved on disk"
    LINE+="${nl}1:M $(ts) * Background saving terminated with success"
  elif chance 50; then
    LINE="1:M $(ts) # Connection with client id=$((300 + RANDOM % 200)) addr=10.0.3.5:$((50000 + RANDOM % 9000)) lost: Connection reset by peer"
  else
    LINE="1:M $(ts) * $((10 + RANDOM % 900)) changes in 60 seconds. Saving..."
  fi
}

gen_tomcat() {
  local ts thread="http-nio-8080-exec-$((1 + RANDOM % 10))" order
  ts() { printf '%s.%s' "$(now '%d-%b-%Y %H:%M:%S')" "$(ms)"; }
  ORDER=$((ORDER + 1)); order=$ORDER
  if chance 65; then
    LINE="$(ts) INFO [$thread] com.example.shop.web.OrderController.create Order created id=$order customer=$(customer) total=$(total)"
  elif chance 40; then
    LINE="$(ts) WARNING [$thread] com.example.shop.service.PaymentService.charge Payment gateway slow response: $((1500 + RANDOM % 2500)) ms (threshold 1500 ms)"
  elif chance 50; then
    LINE="$(ts) INFO [Catalina-utility-$((1 + RANDOM % 2))] org.apache.catalina.core.StandardContext.reload Reloading Context with name [/shop] has started"
  else
    # SEVERE with a trace, the way java.util.logging prints it (tab-indented).
    LINE="$(ts) SEVERE [$thread] com.example.shop.service.PaymentService.charge Payment failed for order $order"
    LINE+="${nl}"$'\t'"java.net.SocketTimeoutException: Read timed out"
    LINE+="${nl}"$'\t\t'"at java.base/sun.nio.ch.NioSocketImpl.timedRead(NioSocketImpl.java:278)"
    LINE+="${nl}"$'\t\t'"at com.example.shop.client.GatewayClient.post(GatewayClient.java:88)"
    LINE+="${nl}"$'\t\t'"at com.example.shop.service.PaymentService.charge(PaymentService.java:54)"
  fi
}

gen_python_gunicorn() {
  local ts pid=$(pick 7 8 112)
  ts() { printf '[%s +0000] [%s]' "$(now '%Y-%m-%d %H:%M:%S')" "$pid"; }
  REPORT=$((REPORT + 1))
  if chance 60; then
    LINE="$(ts) [INFO] reports.tasks: generated report $REPORT in $((RANDOM % 3)).$((RANDOM % 90 + 10))s"
  elif chance 45; then
    LINE="$(ts) [WARNING] django.request: Not Found: /api/reports/$((1000 + RANDOM % 9000))"
  else
    LINE="$(ts) [ERROR] Exception on /api/reports/$REPORT [GET]"
    LINE+="${nl}Traceback (most recent call last):"
    LINE+="${nl}  File \"/app/reports/views.py\", line 57, in report_detail"
    LINE+="${nl}    value = int(request.GET[\"limit\"])"
    LINE+="${nl}ValueError: invalid literal for int() with base 10: 'ten'"
  fi
}

gen_nodejs_json() {
  local req
  printf -v req 'r-%04x' $((RANDOM % 65536))
  if chance 70; then
    LINE="{\"time\":\"$(now '%Y-%m-%dT%H:%M:%S').$(ms)Z\",\"level\":\"info\",\"pid\":1,\"hostname\":\"web-7\",\"reqId\":\"$req\",\"method\":\"$(pick GET GET POST)\",\"url\":\"$(pick /api/products /api/cart /api/search?q=shoes)\",\"statusCode\":200,\"responseTime\":$((5 + RANDOM % 90)),\"msg\":\"request completed\"}"
  elif chance 50; then
    LINE="{\"time\":\"$(now '%Y-%m-%dT%H:%M:%S').$(ms)Z\",\"level\":\"warn\",\"pid\":1,\"hostname\":\"web-7\",\"reqId\":\"$req\",\"msg\":\"slow upstream\",\"upstream\":\"payments\",\"elapsedMs\":$((1500 + RANDOM % 2500)),\"thresholdMs\":1500}"
  else
    LINE="{\"time\":\"$(now '%Y-%m-%dT%H:%M:%S').$(ms)Z\",\"level\":\"error\",\"pid\":1,\"hostname\":\"web-7\",\"reqId\":\"$req\",\"err\":{\"type\":\"Error\",\"message\":\"connect ETIMEDOUT 10.0.3.5:8080\",\"stack\":\"Error: connect ETIMEDOUT 10.0.3.5:8080\\n    at TCPConnectWrap.afterConnect [as oncomplete] (node:net:1555:16)\"},\"msg\":\"upstream request failed\"}"
  fi
}

gen_pino_epoch() {
  local req time
  printf -v req 'p-%04x' $((RANDOM % 65536))
  time="$(now '%s')$(ms)" # epoch milliseconds, as pino writes it
  if chance 70; then
    LINE="{\"level\":30,\"time\":$time,\"pid\":1,\"hostname\":\"web-8\",\"reqId\":\"$req\",\"req\":{\"method\":\"$(pick GET GET POST)\",\"url\":\"$(pick /api/products /api/cart /api/orders)\"},\"res\":{\"statusCode\":200},\"responseTime\":$((5 + RANDOM % 90)),\"msg\":\"request completed\"}"
  elif chance 50; then
    LINE="{\"level\":40,\"time\":$time,\"pid\":1,\"hostname\":\"web-8\",\"upstream\":\"payments\",\"elapsedMs\":$((1500 + RANDOM % 2500)),\"msg\":\"slow upstream\"}"
  else
    LINE="{\"level\":50,\"time\":$time,\"pid\":1,\"hostname\":\"web-8\",\"reqId\":\"$req\",\"err\":{\"type\":\"Error\",\"message\":\"connect ETIMEDOUT 10.0.3.5:8080\",\"stack\":\"Error: connect ETIMEDOUT 10.0.3.5:8080\\n    at TCPConnectWrap.afterConnect [as oncomplete] (node:net:1555:16)\"},\"msg\":\"upstream request failed\"}"
  fi
}

gen_kubernetes() {
  local ts
  # `kubectl logs --timestamps`: nanoseconds, and a timestamp on every line.
  ts() { printf '%s.%09dZ' "$(now '%Y-%m-%dT%H:%M:%S')" $(((RANDOM * 32768 + RANDOM) % 1000000000)); }
  ORDER=$((ORDER + 1))
  if chance 65; then
    LINE="$(ts) level=info msg=\"order processed\" order_id=$ORDER duration_ms=$((30 + RANDOM % 40))"
  elif chance 35; then
    LINE="$(ts) level=debug msg=\"heartbeat\" unacked=$((RANDOM % 4))"
  elif chance 40; then
    LINE="$(ts) level=warn msg=\"slow consumer\" queue=orders lag_ms=$((1000 + RANDOM % 2000))"
  elif chance 85; then
    LINE="$(ts) level=error msg=\"failed to write order\" order_id=$ORDER err=\"context deadline exceeded\""
    LINE+="${nl}$(ts) level=warn msg=\"retrying\" order_id=$ORDER attempt=1 backoff=1s"
  else
    # A Go panic: a timestamp on every line of it.
    LINE="$(ts) panic: runtime error: invalid memory address or nil pointer dereference"
    LINE+="${nl}$(ts) [signal SIGSEGV: segmentation violation code=0x1 addr=0x0 pc=0x4a2f1c]"
    LINE+="${nl}$(ts) goroutine 42 [running]:"
    LINE+="${nl}$(ts) main.(*Worker).handle(0xc0001a2000, {0xc000124000, 0x5c})"
    LINE+="${nl}$(ts) "$'\t'"/src/cmd/orders-worker/worker.go:118 +0x1c"
  fi
}

# The generator for a file, or nothing if this script doesn't know the file.
generator_for() {
  case $(basename "$1") in
    example_nginx_access.log)    echo gen_nginx_access ;;
    example_nginx_error.log)     echo gen_nginx_error ;;
    example_spring_boot.log)     echo gen_spring_boot ;;
    example_postgres.log)        echo gen_postgres ;;
    example_syslog.log)          echo gen_syslog ;;
    example_redis.log)           echo gen_redis ;;
    example_tomcat.log)          echo gen_tomcat ;;
    example_python_gunicorn.log) echo gen_python_gunicorn ;;
    example_nodejs_json.log)     echo gen_nodejs_json ;;
    example_pino_epoch.log)      echo gen_pino_epoch ;;
    example_kubernetes.log)      echo gen_kubernetes ;;
  esac
}

# ---- the files ------------------------------------------------------------------------------

# With --dir, a directory that has no logs yet gets copies of the examples, so that a place
# that can be written to is one command away (the examples themselves stay as they are).
if [ "$DIR" != "$HERE" ] && ! ls "$DIR"/*.log >/dev/null 2>&1; then
  mkdir -p "$DIR" && cp "$HERE"/example_*.log "$DIR"/ || {
    echo "cannot copy the example logs to $DIR" >&2
    exit 1
  }
  echo "copied the example logs to $DIR"
fi

FILES=(); GENERATORS=(); SIZES=(); WRITTEN=()
for file in "$DIR"/*.log; do
  [ -f "$file" ] || continue
  generator=$(generator_for "$file")
  [ -n "$generator" ] || continue
  case $(basename "$file") in *"$ONLY"*) ;; *) continue ;; esac
  FILES+=("$file"); GENERATORS+=("$generator")
  SIZES+=("$(wc -c < "$file" | tr -d ' ')"); WRITTEN+=(0)
done

if [ ${#FILES[@]} -eq 0 ]; then
  echo "no example logs to write to in $DIR${ONLY:+ matching \"$ONLY\"}" >&2
  exit 1
fi

# Say so once, before starting, if a file cannot be written to: a read-only mount (a Lima VM
# mounts the home directory that way), a locked file. Not once per line.
for file in "${FILES[@]}"; do
  if [ ! -w "$file" ]; then
    echo "cannot write to $file" >&2
    echo "(a read-only file system? In a Lima VM the home directory is read-only.)" >&2
    echo "Write copies somewhere else instead:  $0 ${DURATION:-0} --dir /tmp/tilog-logs" >&2
    exit 1
  fi
done

finish() {
  trap - INT TERM EXIT
  echo
  echo "wrote lines in $SECONDS s:"
  local i
  for i in "${!FILES[@]}"; do
    printf '  %-32s +%d lines\n' "$(basename "${FILES[$i]}")" "${WRITTEN[$i]}"
  done
  if [ "$RESTORE" -eq 1 ]; then
    for i in "${!FILES[@]}"; do
      # Writing no input at an offset cuts the file there: the same file, shorter.
      dd if=/dev/null of="${FILES[$i]}" bs=1 seek="${SIZES[$i]}" 2>/dev/null
    done
    echo "files cut back to their old size"
  else
    echo "the example logs have grown (run with --restore to undo)"
  fi
  exit 0
}
trap finish INT TERM EXIT

if [ "$DURATION" -gt 0 ]; then
  echo "writing to ${#FILES[@]} example logs in $DIR, about $RATE lines/s, for ${DURATION}s (Ctrl+C stops early)"
  echo "watch them with:  tilog $DIR/*.log"
else
  echo "writing to ${#FILES[@]} example logs in $DIR, about $RATE lines/s, until Ctrl+C"
  echo "watch them with:  tilog $DIR/*.log"
fi

# ---- the loop -------------------------------------------------------------------------------

END=$((SECONDS + DURATION))
while [ "$DURATION" -eq 0 ] || [ "$SECONDS" -lt "$END" ]; do
  i=$((RANDOM % ${#FILES[@]}))
  LINE=""
  refresh_time
  "${GENERATORS[$i]}"
  # One write for the whole entry: a stack trace is never seen half written.
  printf '%s\n' "$LINE" >> "${FILES[$i]}"

  count=$(printf '%s\n' "$LINE" | wc -l | tr -d ' ')
  WRITTEN[$i]=$((WRITTEN[i] + count))
  [ "$QUIET" -eq 1 ] || printf '%3ds  %-28s +%d\n' "$SECONDS" "$(basename "${FILES[$i]}")" "$count"

  # Wait a while: on average 1/RATE seconds, between 40% and 160% of that.
  wait_ms=$((1000 / RATE * (40 + RANDOM % 120) / 100))
  sleep "$(printf '%d.%03d' $((wait_ms / 1000)) $((wait_ms % 1000)))"
done
