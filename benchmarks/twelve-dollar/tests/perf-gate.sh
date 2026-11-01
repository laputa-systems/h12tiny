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
  hot) echo 'perf-gate-ready: hot process tree (>= 100% CPU)' >&2; exit 0 ;;
  busy) exit 75 ;;
esac
GATE
chmod +x "$test_dir/perf-gate-ready"
run_gate() {
  PATH="$test_dir:$PATH" MOCK_GATE="$1" /bin/bash -c 'source "$1"; quiet_perf_gate' _ "$root/benchmarks/twelve-dollar/tools/perf-gate.sh"
}
run_gate quiet
if run_gate hot >/dev/null 2>&1; then exit 1; else [ "$?" -eq 75 ]; fi
if run_gate busy >/dev/null 2>&1; then exit 1; else [ "$?" -eq 75 ]; fi
