#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
mkdir -p "$root/.work/tmp"
test_dir="$(mktemp -d "$root/.work/tmp/perf-gate.XXXXXXXX")"
trap 'rm -rf "$test_dir"' EXIT
cat > "$test_dir/perf-gate-ready" <<'GATE'
#!/bin/sh
case "$MOCK_GATE" in
  quiet) exit 0 ;;
  hot) echo 'perf-gate-ready: hot process tree (>= 100% CPU)' >&2; echo "* pid=$$ ppid=1 fake external" >&2; exit 0 ;;
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
  PATH="$test_dir:$PATH" MOCK_GATE="$1" MOCK_HOT_PID="$$" PERF_GATE_IGNORE_PROCESS=perf-gate.sh \
    /bin/bash -c 'source "$1"; quiet_perf_gate' _ "$root/benchmarks/twelve-dollar/tools/perf-gate.sh"
}
run_gate quiet
run_gate hot-self >/dev/null 2>&1
if run_gate hot >/dev/null 2>&1; then exit 1; else [ "$?" -eq 75 ]; fi
if run_gate busy >/dev/null 2>&1; then exit 1; else [ "$?" -eq 75 ]; fi
