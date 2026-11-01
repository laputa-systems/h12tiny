#!/usr/bin/env bash
set -euo pipefail
[ "$(uname -s)" = Darwin ] || exit 0
root="$(cd "$(dirname "$0")/../../.." && pwd)"
mkdir -p "$root/.work/tmp"
test_dir="$(mktemp -d "$root/.work/tmp/cancel-macos.XXXXXXXX")"
runner=""
cleanup() {
  if [ -n "$runner" ]; then kill -KILL "$runner" 2>/dev/null || true; wait "$runner" 2>/dev/null || true; fi
  for f in k6.pid app.pid; do
    if [ -f "$test_dir/$f" ]; then kill -KILL "$(cat "$test_dir/$f")" 2>/dev/null || true; fi
  done
  rm -rf "$test_dir"
}
trap cleanup EXIT
mkdir -p "$test_dir/bin" "$test_dir/bench/tools" "$test_dir/challenge/seed"
printf seed > "$test_dir/challenge/seed/feed.db"
git -C "$test_dir/challenge" init -q
git -C "$test_dir/challenge" add seed/feed.db
git -C "$test_dir/challenge" -c user.name=test -c user.email=test@example.invalid commit -qm fixture
cp "$root/benchmarks/twelve-dollar/tools/perf-gate.sh" "$test_dir/bench/tools/perf-gate.sh"
cat > "$test_dir/bench/build.sh" <<'BUILD'
#!/bin/sh
exit 0
BUILD
cat > "$test_dir/bench/start.sh" <<'START'
#!/bin/sh
printf '%s\n' "$$" > "$TWELVE_TEST_DIR/app.pid"
exec sleep 120
START
cat > "$test_dir/bin/docker" <<'DOCKER'
#!/bin/sh
case " $* " in
  *' run '*' --name h12t-twelve-k6 '*) printf '%s\n' "$$" > "$TWELVE_TEST_DIR/k6.pid"; exec sleep 120 ;;
esac
exit 0
DOCKER
printf '#!/bin/sh\nexit 0\n' > "$test_dir/bin/curl"
printf '#!/bin/sh\nexit 1\n' > "$test_dir/bin/lsof"
printf '#!/bin/sh\nexit 0\n' > "$test_dir/bin/perf-gate-ready"
chmod +x "$test_dir/bin/"* "$test_dir/bench/"*.sh
PATH="$test_dir/bin:$PATH" TWELVE_TEST_DIR="$test_dir" /bin/bash "$root/benchmarks/twelve-dollar/macos/smoke.sh" \
  "$test_dir/bench" "$test_dir/challenge" "$test_dir/result" 1 1 1 1 image net > "$test_dir/output" 2>&1 &
runner=$!
for _ in $(seq 1 100); do
  [ -s "$test_dir/k6.pid" ] && break
  kill -0 "$runner" 2>/dev/null || { cat "$test_dir/output" >&2; exit 1; }
  sleep 0.1
done
[ -s "$test_dir/k6.pid" ] || { cat "$test_dir/output" >&2; exit 1; }
kill -TERM "$runner"
for _ in $(seq 1 50); do
  if ! kill -0 "$runner" 2>/dev/null; then break; fi
  sleep 0.1
done
if kill -0 "$runner" 2>/dev/null; then echo 'runner did not stop after SIGTERM' >&2; exit 1; fi
wait "$runner" 2>/dev/null || true
runner=""
for f in k6.pid app.pid; do
  if kill -0 "$(cat "$test_dir/$f")" 2>/dev/null; then echo "$f survived cancellation" >&2; exit 1; fi
done
[ ! -e "$test_dir/result/feed.db" ]
