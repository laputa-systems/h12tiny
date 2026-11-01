#!/usr/bin/env bash
set -euo pipefail

action="${1:-}"
shift || true

case "$action" in
  start)
    : "${START_SH:?START_SH must name the app start script}"
    : "${LOG:?LOG must name the app log}"
    : "${PIDFILE:?PIDFILE must name the pid file}"
    mkdir -p "$(dirname "$LOG")" "$(dirname "$PIDFILE")"
    : > "$LOG"
    printf '%s\n' "$$" > "$PIDFILE"
    exec bash "$START_SH" >> "$LOG" 2>&1
    ;;
  wait)
    : "${HOST:?HOST must name the app bind address}"
    : "${PORT:?PORT must name the app port}"
    : "${LOG:?LOG must name the app log}"
    probe="$HOST"
    if [ "$probe" = "0.0.0.0" ] || [ "$probe" = "::" ]; then
      probe=127.0.0.1
    fi
    healthy=0
    for _ in $(seq 60); do
      if curl -sf "http://$probe:$PORT/health" >/dev/null 2>&1; then
        healthy=1
        break
      fi
      if [ -s "${PIDFILE:-}" ]; then
        pid="$(cat "$PIDFILE")"
        if ! kill -0 "$pid" 2>/dev/null; then
          break
        fi
      fi
      sleep 1
    done
    if [ "$healthy" != 1 ]; then
      echo "no healthy /health on $probe:$PORT after 60 s" >&2
      tail -30 "$LOG" >&2 || true
      exit 1
    fi
    ;;
  stop)
    : "${PIDFILE:?PIDFILE must name the pid file}"
    pid="$(cat "$PIDFILE")"
    if ! kill -0 "$pid" 2>/dev/null; then
      exit 0
    fi
    kill -TERM "$pid" 2>/dev/null || true
    for _ in $(seq 400); do
      if ! kill -0 "$pid" 2>/dev/null; then
        exit 0
      fi
      sleep 0.1
    done
    echo "app $pid did not exit within 40 s of SIGTERM" >&2
    kill -KILL "$pid" 2>/dev/null || true
    exit 1
    ;;
  summary)
    : "${LOG:?LOG must name the app log}"
    awk '/^h12tiny-twelve shutdown / { line = $0 } END { if (line != "") print line }' "$LOG"
    ;;
  *)
    echo "usage: app.sh start|wait|stop|summary" >&2
    exit 2
    ;;
esac
