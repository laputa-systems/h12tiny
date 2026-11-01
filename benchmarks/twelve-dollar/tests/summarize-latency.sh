#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
mkdir -p "$root/.work/tmp"
test_dir="$(mktemp -d "$root/.work/tmp/summarize-latency.XXXXXXXX")"
trap 'rm -rf "$test_dir"' EXIT
printf '%s\n' '{"run_id":"test","mode":"direct","vus":1,"ramp_up_s":1,"hold_s":1,"gate_before":true,"gate_after":true,"db_preexisting":false,"challenge":{"dirty":false}}' > "$test_dir/run.json"
printf '%s\n' '{"metrics":{"http_reqs":{"count":1,"rate":1},"http_req_duration":{"med":0.31408,"p(95)":0.43134,"p(99)":0.9109,"max":6.41},"http_req_failed":{"value":0},"vus":{"max":1}}}' > "$test_dir/k6-summary.json"
printf '%s\n' '{"exit_code":0}' > "$test_dir/k6-run.json"
printf '%s\n' 'sample,elapsed_s,clk_tck,app_pid,app_cpu_ticks,nginx_cpu_ticks,app_rss_kib,app_hwm_kib,established,db_bytes,wal_bytes,memory_peak_bytes,memory_current_bytes,pids_current' '1,0,100,1,1,0,1,1,1,100,0,1,1,1' '2,1,100,1,2,0,1,1,1,100,0,1,1,1' > "$test_dir/metrics.csv"
printf '%s\n' 'listening reader_threads=1 executor_threads=1' 'h12tiny-twelve shutdown accepted=1 rejected=0 responses=1 status_200=1' > "$test_dir/app.log"
printf '%s\n' 'http_reqs....: 1' > "$test_dir/k6.log"
printf '%s\n' '{"CPUPerc":"1.00%","MemPerc":"1.00%"}' > "$test_dir/k6-stats.log"
bash "$root/benchmarks/twelve-dollar/tools/summarize.sh" "$test_dir" >/dev/null
jaq -e '.k6.p50_ms == 0.31408 and .k6.p95_ms == 0.43134 and .k6.p99_ms == 0.9109 and .k6.max_ms == 6.41' "$test_dir/summary.json" >/dev/null
printf '%s\n' '{"exit_code":99}' > "$test_dir/k6-run.json"
bash "$root/benchmarks/twelve-dollar/tools/summarize.sh" "$test_dir" >/dev/null 2>&1
jaq -e '.valid == true and .k6.exit_code == 99' "$test_dir/summary.json" >/dev/null
: > "$test_dir/k6-stats.log"
bash "$root/benchmarks/twelve-dollar/tools/summarize.sh" "$test_dir" >/dev/null 2>&1
jaq -e '.valid == false' "$test_dir/summary.json" >/dev/null
