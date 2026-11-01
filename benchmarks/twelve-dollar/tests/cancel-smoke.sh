#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
mkdir -p "$root/.work/tmp"
test_dir="$(mktemp -d "$root/.work/tmp/cancel-smoke.XXXXXXXX")"
run_id="cancel-smoke-$$"
runner=""
cleanup() {
  if [ -n "$runner" ]; then kill -KILL "$runner" 2>/dev/null || true; wait "$runner" 2>/dev/null || true; fi
  if [ -f "$test_dir/k6.pid" ]; then kill -KILL "$(cat "$test_dir/k6.pid")" 2>/dev/null || true; fi
  rm -rf "$root/.work/twelve-dollar/results/$run_id" "$test_dir"
}
trap cleanup EXIT
mkdir -p "$test_dir/bin"
cat > "$test_dir/bin/perf-gate-ready" <<'GATE'
#!/usr/bin/env bash
exit 0
GATE
cat > "$test_dir/bin/docker" <<'DOCKER'
#!/usr/bin/env bash
set -euo pipefail
args=" $* "
printf '%s\n' "$*" >> "$TWELVE_TEST_DIR/docker-calls"
case "$args" in
  *' run '*' --name h12t-twelve-k6 '*)
    if [ "${TWELVE_TEST_COMPLETE:-false}" = true ]; then sleep 2; printf '%s\n' 'http_reqs....: 1'; exit 0; fi
    printf '%s\n' "$$" > "$TWELVE_TEST_DIR/k6.pid"; exec sleep 120 ;;
  *' stats '*) printf '%s\n' '{"CPUPerc":"1.00%","MemPerc":"1.00%"}' ;;
  *' cp h12t-twelve-box:'*) cp "$TWELVE_TEST_DIR/fixtures/$(basename "$2")" "$3" ;;
  *' inspect '*'.State.Running'*) printf 'true\n' ;;
  *' inspect '*'.NetworkSettings.Networks'*) printf '127.0.0.1\n' ;;
  *' exec '*' pgrep '*) exit 1 ;;
  *' exec '*' kill -0 '*) exit 1 ;;
  *' exec '*' ss '*) exit 0 ;;
  *' exec '*' cat /runs/'*'/app.pid '*) printf '111\n' ;;
  *' exec '*' cat /runs/'*'/collector.pid '*) printf '222\n' ;;
  *' exec '*' wc -l '*) printf '2\n' ;;
  *' exec '*' lscpu '*) printf 'CPU(s): 18\nModel name: test\n' ;;
  *' exec '*' cat /sys/fs/cgroup/cpu.max '*) printf '100000 100000\n' ;;
  *' exec '*' cat /sys/fs/cgroup/memory.max '*) printf '2147483648\n' ;;
  *' exec '*' cat /sys/fs/cgroup/memory.swap.max '*) printf '0\n' ;;
  *' exec '*' uname -r '*) printf 'test\n' ;;
  *' exec '*' uname -m '*) printf 'test\n' ;;
  *' exec '*' nginx -v '*) printf 'nginx test\n' ;;
  *' exec '*' rustc -Vv '*) printf 'commit-hash: test\nhost: test\n' ;;
  *' exec '*' rustc -V '*) printf 'rustc test\n' ;;
  *' exec '*' sqlite3 --version '*) printf '3.0\n' ;;
esac
exit 0
DOCKER
chmod +x "$test_dir/bin/docker" "$test_dir/bin/perf-gate-ready"
PATH="$test_dir/bin:$PATH" TWELVE_TEST_DIR="$test_dir" TWELVE_DOLLAR_RUN_ID="$run_id" \
  /bin/bash "$root/benchmarks/twelve-dollar/run.sh" box smoke --mode direct --vus 1 > "$test_dir/output" 2>&1 &
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
if kill -0 "$(cat "$test_dir/k6.pid")" 2>/dev/null; then echo 'generator survived cancellation' >&2; exit 1; fi
rg -q '^exec h12t-twelve-box kill -TERM 222$' "$test_dir/docker-calls"
rg -q '^exec h12t-twelve-box env PIDFILE=/runs/[^ ]+/app.pid bash /runs/[^ ]+/harness/box/app.sh stop$' "$test_dir/docker-calls"
rg -q "^exec h12t-twelve-box rm -rf /runs/$run_id$" "$test_dir/docker-calls"
mkdir -p "$test_dir/fixtures"
printf '%s\n' '{"metrics":{"http_reqs":{"count":1,"rate":1},"http_req_duration":{"med":0.3,"p(95)":0.4,"p(99)":0.9,"max":1},"http_req_failed":{"value":0},"vus":{"max":1}}}' > "$test_dir/fixtures/k6-summary.json"
printf '%s\n' 'sample,elapsed_s,clk_tck,app_pid,app_cpu_ticks,nginx_cpu_ticks,app_rss_kib,app_hwm_kib,app_fds,established,db_bytes,wal_bytes,memory_peak_bytes,memory_current_bytes,pids_current,cpu_nr_periods,cpu_nr_throttled' '1,0,100,1,1,0,1,1,10,1,100,0,1,1,1,100,10' '2,1,100,1,2,0,1,1,20,1,100,0,1,1,1,200,10' > "$test_dir/fixtures/metrics.csv"
printf '%s\n' 'listening reader_threads=1 executor_threads=1' 'h12tiny-twelve shutdown accepted=1 rejected=0 responses=1 status_200=1' > "$test_dir/fixtures/app.log"
: > "$test_dir/fixtures/bench.access.log"
: > "$test_dir/fixtures/bench.error.log"
rm -rf "$root/.work/twelve-dollar/results/$run_id"
run_id="${run_id}-complete"
PATH="$test_dir/bin:$PATH" TWELVE_TEST_DIR="$test_dir" TWELVE_TEST_COMPLETE=true TWELVE_DOLLAR_RUN_ID="$run_id" \
  /bin/bash "$root/benchmarks/twelve-dollar/run.sh" box smoke --mode direct --vus 1 > "$test_dir/output" 2>&1 || { cat "$test_dir/output" >&2; exit 1; }
jaq -e '.valid == true and .k6.http_reqs == 1 and .k6.exit_code == 0' "$root/.work/twelve-dollar/results/$run_id/summary.json" >/dev/null
