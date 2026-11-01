#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
mkdir -p "$root/.work/tmp"
test_dir="$(mktemp -d "$root/.work/tmp/summarize-latency.XXXXXXXX")"
trap 'rm -rf "$test_dir"' EXIT
printf '%s\n' '{"run_id":"test","mode":"direct","vus":1,"ramp_up_s":1,"hold_s":1,"gate_before":true,"gate_after":true,"db_preexisting":false,"challenge":{"dirty":false}}' > "$test_dir/run.json"
printf '%s\n' '{"metrics":{"http_reqs":{"count":1,"rate":1},"http_req_duration":{"med":0.31408,"p(95)":0.43134,"p(99)":0.9109,"max":6.41},"http_req_failed":{"value":0},"vus":{"max":1}}}' > "$test_dir/k6-summary.json"
printf '%s\n' '{"exit_code":0}' > "$test_dir/k6-run.json"
printf '%s\n' 'sample,elapsed_s,clk_tck,app_pid,app_cpu_ticks,nginx_cpu_ticks,app_rss_kib,app_hwm_kib,app_fds,established,db_bytes,wal_bytes,memory_peak_bytes,memory_current_bytes,pids_current,cpu_nr_periods,cpu_nr_throttled' '1,0,100,1,1,0,1,1,10,1,100,0,1,1,1,100,10' '2,1,100,1,2,0,1,1,20,1,100,0,1,1,1,200,16' > "$test_dir/metrics.csv"
printf '%s\n' 'listening reader_threads=1 executor_threads=1' 'h12tiny-twelve shutdown accepted=1 rejected=0 responses=1 status_200=1' > "$test_dir/app.log"
printf '%s\n' 'http_reqs....: 1' > "$test_dir/k6.log"
printf '%s\n' '{"CPUPerc":"1.00%","MemPerc":"1.00%"}' > "$test_dir/k6-stats.log"
bash "$root/benchmarks/twelve-dollar/tools/summarize.sh" "$test_dir" >/dev/null
jaq -e '.k6.p50_ms == 0.31408 and .k6.p95_ms == 0.43134 and .k6.p99_ms == 0.9109 and .k6.max_ms == 6.41 and .connections.peak_fds == 20 and .cpu.throttled_period_percent == 6' "$test_dir/summary.json" >/dev/null
bash "$root/benchmarks/twelve-dollar/tools/report.sh" "$test_dir" > /dev/null
rg -q '\| CPU periods throttled \| 6(\.0+)? % \|' "$test_dir/report.md"
rg -q '\| Peak app file descriptors \| 20 \|' "$test_dir/report.md"
jaq '.mode = "nginx"' "$test_dir/run.json" > "$test_dir/nginx.json"
mv "$test_dir/nginx.json" "$test_dir/run.json"
printf '%s\n' \
  '1.2.3.4 [01/Nov/2026:00:00:00 +0000] "GET /feed HTTP/1.1" 200 1 rt=0.001' \
  '1.2.3.4 [01/Nov/2026:00:00:01 +0000] "POST /posts/1/like HTTP/1.1" 499 0 rt=0.001' > "$test_dir/nginx-access.log"
bash "$root/benchmarks/twelve-dollar/tools/summarize.sh" "$test_dir" >/dev/null
jaq -e '.valid == true and .counts.nginx_access_lines == 2 and .counts.nginx_client_cancelled == 1 and .counts.nginx_completed_requests == 1 and .counts.k6_matches_nginx == true' "$test_dir/summary.json" >/dev/null
bash "$root/benchmarks/twelve-dollar/tools/report.sh" "$test_dir" > /dev/null
rg -q -- '- nginx 499 client cancellations: 1' "$test_dir/report.md"
printf '%s\n' \
  '1.2.3.4 [01/Nov/2026:00:00:00 +0000] "GET /feed HTTP/1.1" 200 1 rt=0.001' \
  '1.2.3.4 [01/Nov/2026:00:00:01 +0000] "POST /posts/1/like HTTP/1.1" 502 0 rt=0.001' > "$test_dir/nginx-access.log"
bash "$root/benchmarks/twelve-dollar/tools/summarize.sh" "$test_dir" >/dev/null 2>&1
jaq -e '.valid == false and .counts.nginx_client_cancelled == 0 and .counts.k6_matches_nginx == false' "$test_dir/summary.json" >/dev/null
jaq '.mode = "direct"' "$test_dir/run.json" > "$test_dir/direct.json"
mv "$test_dir/direct.json" "$test_dir/run.json"
: > "$test_dir/nginx-access.log"
jaq 'del(.metrics.http_req_duration["p(99)"])' "$test_dir/k6-summary.json" > "$test_dir/incomplete.json"
mv "$test_dir/incomplete.json" "$test_dir/k6-summary.json"
bash "$root/benchmarks/twelve-dollar/tools/summarize.sh" "$test_dir" >/dev/null 2>&1
jaq -e '.valid == false' "$test_dir/summary.json" >/dev/null
jaq '.metrics.http_req_duration["p(99)"] = 0.9109' "$test_dir/k6-summary.json" > "$test_dir/complete.json"
mv "$test_dir/complete.json" "$test_dir/k6-summary.json"
jaq '.metrics.http_reqs.count = 0' "$test_dir/k6-summary.json" > "$test_dir/empty.json"
mv "$test_dir/empty.json" "$test_dir/k6-summary.json"
printf '%s\n' 'http_reqs....: 0' > "$test_dir/k6.log"
bash "$root/benchmarks/twelve-dollar/tools/summarize.sh" "$test_dir" >/dev/null 2>&1
jaq -e '.valid == false' "$test_dir/summary.json" >/dev/null
jaq '.metrics.http_reqs.count = 1' "$test_dir/k6-summary.json" > "$test_dir/complete.json"
mv "$test_dir/complete.json" "$test_dir/k6-summary.json"
printf '%s\n' 'http_reqs....: 1' > "$test_dir/k6.log"
printf '%s\n' '{"exit_code":99}' > "$test_dir/k6-run.json"
bash "$root/benchmarks/twelve-dollar/tools/summarize.sh" "$test_dir" >/dev/null 2>&1
jaq -e '.valid == true and .k6.exit_code == 99' "$test_dir/summary.json" >/dev/null
: > "$test_dir/k6-stats.log"
bash "$root/benchmarks/twelve-dollar/tools/summarize.sh" "$test_dir" >/dev/null 2>&1
jaq -e '.valid == false' "$test_dir/summary.json" >/dev/null
