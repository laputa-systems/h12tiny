#!/usr/bin/env bash
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCH="$(cd "$HERE/.." && pwd)"

mode=nginx
while [ $# -gt 0 ]; do
  case "$1" in
    --mode)
      mode="${2:?--mode needs nginx or direct}"
      shift
      ;;
    --mode=*)
      mode="${1#*=}"
      ;;
    *)
      echo "unknown argument: $1" >&2
      exit 2
      ;;
  esac
  shift
done

case "$mode" in
  nginx|direct) ;;
  *)
    echo "unknown mode: $mode (expected nginx or direct)" >&2
    exit 2
    ;;
esac

seed="${SEED:-/seed/feed.db}"
runs="${RUNS:-/runs}"
test_sh="${TEST_SH:-/challenge/test/test.sh}"
secret="${JWT_SECRET:-twelve-dollar-challenge}"

if [ ! -f "$seed" ]; then
  echo "missing seed database $seed" >&2
  exit 1
fi
if [ ! -f "$test_sh" ]; then
  echo "missing challenge test suite $test_sh" >&2
  exit 1
fi
if [ ! -f "$BENCH/start.sh" ]; then
  echo "missing $BENCH/start.sh (A2 supplies it)" >&2
  exit 1
fi

if [ "$mode" = nginx ]; then
  app_host=127.0.0.1
  app_port=3000
  bash "$HERE/nginx-check.sh" start
else
  app_host=0.0.0.0
  app_port=80
  bash "$HERE/nginx-check.sh" stop
fi

if [ -n "$(ss -H -ltn "sport = :$app_port")" ]; then
  echo "port $app_port is already in use:" >&2
  ss -H -ltnp "sport = :$app_port" >&2
  exit 1
fi

run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
run_dir="$runs/$run_id"
log="$run_dir/server.log"
pid=""

cleanup() {
  status=$?
  trap - EXIT INT TERM
  if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    pkill -TERM -P "$pid" >/dev/null 2>&1 || true
    kill -TERM "$pid" >/dev/null 2>&1 || true
    for _ in $(seq 50); do
      if ! kill -0 "$pid" 2>/dev/null; then
        break
      fi
      sleep 0.1
    done
    if kill -0 "$pid" 2>/dev/null; then
      pkill -KILL -P "$pid" >/dev/null 2>&1 || true
      kill -KILL "$pid" >/dev/null 2>&1 || true
    fi
    wait "$pid" >/dev/null 2>&1 || true
  fi
  if [ "$mode" = nginx ]; then
    bash "$HERE/nginx-check.sh" stop >/dev/null 2>&1 || true
  fi
  rm -rf "$run_dir"
  exit "$status"
}
trap cleanup EXIT INT TERM

mkdir -p "$run_dir"
cp "$seed" "$run_dir/feed.db"

export SQLITE_PATH="$run_dir/feed.db"
export JWT_SECRET="$secret"
export HOST="$app_host"
export PORT="$app_port"

bash "$BENCH/start.sh" >"$log" 2>&1 &
pid=$!

healthy=0
for _ in $(seq 60); do
  if curl -sf "http://127.0.0.1:$app_port/health" >/dev/null; then
    healthy=1
    break
  fi
  if ! kill -0 "$pid" 2>/dev/null; then
    break
  fi
  sleep 1
done

if [ "$healthy" != 1 ]; then
  echo "no healthy /health on 127.0.0.1:$app_port after 60 s" >&2
  tail -30 "$log" >&2 || true
  exit 1
fi

rc=0
bash "$test_sh" http://127.0.0.1 || rc=$?
if [ "$rc" != 0 ]; then
  echo "challenge test failed; last 30 server log lines:" >&2
  tail -30 "$log" >&2 || true
fi
exit "$rc"
