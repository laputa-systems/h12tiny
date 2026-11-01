#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
mkdir -p "$root/.work/tmp"
test_dir="$(mktemp -d "$root/.work/tmp/perf-gate.XXXXXXXX")"
trap 'rm -rf "$test_dir"' EXIT
cat > "$test_dir/perf-gate-ready" <<'GATE'
#!/bin/sh
printf '%s\n' "$MOCK_GATE" >> "$MOCK_LOG"
case "$MOCK_GATE" in
  quiet) exit 0 ;;
  hot) echo 'perf-gate-ready: hot process tree (>= 100% CPU)' >&2; echo "* pid=$$ ppid=1 fake external" >&2; exit 0 ;;
  hot-once)
    if [ -f "$MOCK_STATE" ]; then exit 0; fi
    : > "$MOCK_STATE"
    echo 'perf-gate-ready: hot process tree (>= 100% CPU)' >&2
    echo "* pid=$$ ppid=1 fake external" >&2
    exit 0
    ;;
  hot-self)
    echo 'perf-gate-ready: hot process tree (>= 100% CPU)' >&2
    echo "* pid=$MOCK_HOT_PID ppid=1 fake self" >&2
    exit 0
    ;;
  busy) exit 75 ;;
esac
GATE
chmod +x "$test_dir/perf-gate-ready"
run_gate() {
  PATH="$test_dir:$PATH" MOCK_GATE="$1" MOCK_LOG="$test_dir/log" /bin/bash -c 'source "$1"; quiet_perf_gate' _ "$root/benchmarks/twelve-dollar/tools/perf-gate.sh"
}
run_settled() {
  PATH="$test_dir:$PATH" MOCK_GATE="$1" MOCK_LOG="$test_dir/log" MOCK_STATE="$test_dir/state" MOCK_HOT_PID="$$" \
    PERF_GATE_IGNORE_PROCESS=perf-gate.sh PERF_GATE_SETTLE_WAIT=0 \
    /bin/bash -c 'source "$1"; settled_perf_gate -1' _ "$root/benchmarks/twelve-dollar/tools/perf-gate.sh"
}
run_gate quiet
if run_gate hot >/dev/null 2>&1; then exit 1; else [ "$?" -eq 75 ]; fi
if run_gate busy >/dev/null 2>&1; then exit 1; else [ "$?" -eq 75 ]; fi
run_settled hot-once >/dev/null 2>&1
run_settled hot-self >/dev/null 2>&1
: > "$test_dir/log"
if run_settled hot >/dev/null 2>&1; then exit 1; else [ "$?" -eq 75 ]; fi
[ "$(wc -l < "$test_dir/log")" -eq 6 ]
: > "$test_dir/log"
if run_settled busy >/dev/null 2>&1; then exit 1; else [ "$?" -eq 75 ]; fi
[ "$(wc -l < "$test_dir/log")" -eq 1 ]
