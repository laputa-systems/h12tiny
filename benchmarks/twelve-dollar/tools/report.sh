#!/usr/bin/env bash
set -euo pipefail

RUN_DIR="${1:?usage: report.sh <run-directory>}"

die() {
  echo "report: $*" >&2
  exit 1
}

for required in meta.json summary.json; do
  [ -f "$RUN_DIR/$required" ] || die "missing $RUN_DIR/$required; run summarize.sh first"
done

meta="$RUN_DIR/meta.json"
summary="$RUN_DIR/summary.json"

read_meta() {
  jaq -r "$1" "$meta" 2>/dev/null || true
}

read_summary() {
  jaq -r "$1" "$summary" 2>/dev/null || true
}

fmt() {
  awk -v value="$1" -v format="$2" 'BEGIN { printf format, value }'
}

status_string() {
  jaq -r 'if length == 0 then "none" else (to_entries | map("\(.key)=\(.value)") | join(", ")) end' <<< "$1"
}

run_id="$(read_meta '.run_id')"
mode="$(read_summary '.mode')"
vus="$(read_summary '.vus')"
ramp="$(read_summary '.ramp_up_s')"
hold="$(read_summary '.hold_s')"
http_reqs="$(read_summary '.k6.http_reqs')"
http_reqs_stdout="$(read_summary '.k6.http_reqs_stdout')"
nginx_lines="$(read_summary '.counts.nginx_access_lines')"
summary_matches_stdout="$(read_summary '.counts.summary_matches_stdout')"
k6_matches_nginx="$(read_summary '.counts.k6_matches_nginx')"
app_responses="$(read_summary '.counts.app_responses')"
app_accepted="$(read_summary '.counts.app_accepted')"
app_rejected="$(read_summary '.counts.app_rejected')"
status_app="$(jaq -c '.status_app' "$summary")"
status_nginx="$(jaq -c '.status_nginx' "$summary")"
throughput="$(read_summary '.k6.throughput_req_s')"
p50="$(read_summary '.k6.p50_ms')"
p95="$(read_summary '.k6.p95_ms')"
p99="$(read_summary '.k6.p99_ms')"
failure_rate="$(read_summary '.k6.failure_rate_percent')"
app_cpu_per_request="$(read_summary '.cpu.app_cpu_seconds_per_request')"
nginx_cpu_per_request="$(read_summary '.cpu.nginx_cpu_seconds_per_request')"
combined_cpu_per_request="$(read_summary '.cpu.combined_cpu_seconds_per_request')"
utilization="$(read_summary '.cpu.utilization_percent')"
window_seconds="$(read_summary '.cpu.window_seconds')"
peak_rss="$(read_summary '.memory.peak_rss_hwm_mib')"
peak_rss_sampled="$(read_summary '.memory.peak_rss_mib')"
cgroup_peak="$(read_summary '.memory.cgroup_peak_mib')"
peak_connections="$(read_summary '.connections.peak_established')"
peak_fds="$(read_summary '.connections.peak_fds // "unavailable"')"
throttled_percent="$(read_summary '.cpu.throttled_period_percent // "unavailable"')"
db_growth="$(read_summary '.db.db_growth_mib')"
wal_growth="$(read_summary '.db.wal_growth_mib')"
reader_threads="$(read_summary '.threads.reader')"
executor_threads="$(read_summary '.threads.executor')"
valid="$(read_summary '.valid')"

kernel="$(read_meta '.environment.kernel')"
arch="$(read_meta '.environment.arch')"
cpu_model="$(read_meta '.environment.cpu_model')"
cpu_count="$(read_meta '.environment.cpu_count')"
cpu_max="$(read_meta '.environment.cgroup.cpu_max')"
memory_max="$(read_meta '.environment.cgroup.memory_max')"
memory_swap_max="$(read_meta '.environment.cgroup.memory_swap_max')"
nginx_version="$(read_meta '.environment.nginx_version')"
rustc_version="$(read_meta '.environment.rustc.version')"
rustc_commit="$(read_meta '.environment.rustc.commit_hash')"
sqlite_version="$(read_meta '.environment.sqlite_version')"
h12tiny_revision="$(read_meta '.h12tiny.revision')"
h12tiny_dirty="$(read_meta '.h12tiny.dirty')"
challenge_revision="$(read_meta '.challenge.revision')"
challenge_dirty="$(read_meta '.challenge.dirty')"
build_profile="$(read_meta '.build.profile')"
build_opt="$(read_meta '.build.opt_level')"
build_lto="$(read_meta '.build.lto')"
build_panic="$(read_meta '.build.panic')"
build_target="$(read_meta '.build.target_cpu')"
pragmas="$(jaq -r '.pragmas | "journal_mode=\(.journal_mode), synchronous=\(.synchronous), busy_timeout=\(.busy_timeout_ms) ms"' "$meta")"

if [ "$mode" = nginx ]; then
  nginx_status_value="$(status_string "$status_nginx")"
  nginx_count_value="$nginx_lines"
else
  nginx_status_value="not applicable (nginx stopped, direct mode)"
  nginx_count_value="not applicable (nginx stopped, direct mode)"
fi

report="$RUN_DIR/report.md"
{
  printf '# Twelve Dollar smoke run `%s`\n\n' "$run_id"
  if [ "$valid" = true ]; then
    printf 'Valid under D9: **yes**.\n\n'
  else
    printf 'Valid under D9: **no**.\n\n'
    while IFS= read -r reason; do
      [ -n "$reason" ] || continue
      printf '%s\n' "- $reason"
    done < <(jaq -r '.invalid_reasons[]' "$summary")
    printf '\n'
  fi

  printf '| Metric | Value |\n'
  printf '| --- | --- |\n'
  printf '| VUs | %s |\n' "$vus"
  printf '| Mode | %s |\n' "$mode"
  printf '| Ramp / hold durations | %s s / %s s |\n' "$ramp" "$hold"
  printf '| k6 http_reqs | %s |\n' "$http_reqs"
  printf '| Throughput | %s req/s |\n' "$(fmt "$throughput" '%.3f')"
  printf '| Latency p50 | %s ms |\n' "$(fmt "$p50" '%.3f')"
  printf '| Latency p95 | %s ms |\n' "$(fmt "$p95" '%.3f')"
  printf '| Latency p99 | %s ms |\n' "$(fmt "$p99" '%.3f')"
  printf '| Failure rate | %s %% |\n' "$(fmt "$failure_rate" '%.4f')"
  printf '| App CPU seconds/request | %s s/request |\n' "$(fmt "$app_cpu_per_request" '%.9f')"
  printf '| Nginx CPU seconds/request | %s s/request |\n' "$(fmt "$nginx_cpu_per_request" '%.9f')"
  printf '| Combined CPU seconds/request | %s s/request |\n' "$(fmt "$combined_cpu_per_request" '%.9f')"
  printf '| CPU utilization of the 1-CPU box (app + nginx) | %s %% over %s s |\n' "$(fmt "$utilization" '%.2f')" "$(fmt "$window_seconds" '%.3f')"
  printf '| CPU periods throttled | %s %% |\n' "$throttled_percent"
  printf '| Peak RSS | %s MiB |\n' "$(fmt "$peak_rss" '%.3f')"
  printf '| Peak sampled VmRSS | %s MiB |\n' "$(fmt "$peak_rss_sampled" '%.3f')"
  printf '| cgroup peak memory | %s MiB |\n' "$(fmt "$cgroup_peak" '%.3f')"
  printf '| Peak established connections on the app port | %s |\n' "$peak_connections"
  printf '| Peak app file descriptors | %s |\n' "$peak_fds"
  printf '| DB growth | %s MiB |\n' "$(fmt "$db_growth" '%.3f')"
  printf '| WAL growth | %s MiB |\n' "$(fmt "$wal_growth" '%.3f')"
  printf '| Requests by status (app shutdown summary) | %s |\n' "$(status_string "$status_app")"
  printf '| Requests by status (nginx access log) | %s |\n' "$nginx_status_value"
  printf '| Reader / executor threads | %s / %s |\n' "$reader_threads" "$executor_threads"
  printf '| nginx version | %s |\n' "$nginx_version"
  printf '| Rust version | %s (%s) |\n' "$rustc_version" "$rustc_commit"
  printf '| SQLite version | %s |\n' "$sqlite_version"
  printf '| Kernel / CPU | %s / %s, %s CPUs, %s |\n' "$kernel" "$arch" "$cpu_count" "$cpu_model"
  printf '| cgroup limits | cpu.max=%s, memory.max=%s, memory.swap.max=%s |\n' "$cpu_max" "$memory_max" "$memory_swap_max"
  printf '| h12tiny revision | %s (dirty=%s) |\n' "$h12tiny_revision" "$h12tiny_dirty"
  printf '| Challenge revision | %s (dirty=%s) |\n' "$challenge_revision" "$challenge_dirty"
  printf '| Build | profile=%s, opt-level=%s, lto=%s, panic=%s, target-cpu=%s |\n' "$build_profile" "$build_opt" "$build_lto" "$build_panic" "$build_target"
  printf '| SQLite pragmas | %s |\n\n' "$pragmas"

  printf '## Request count equality\n\n'
  printf -- '- k6 saved summary `http_reqs`: %s\n' "$http_reqs"
  printf -- '- k6 stdout `http_reqs`: %s\n' "${http_reqs_stdout:-missing}"
  printf -- '- nginx access-log lines: %s\n' "$nginx_count_value"
  printf -- '- k6 summary equals k6 stdout: %s\n' "$summary_matches_stdout"
  if [ "$mode" = nginx ]; then
    printf -- '- k6 `http_reqs` equals nginx access-log lines: %s\n' "$k6_matches_nginx"
  else
    printf -- '- k6 `http_reqs` equals nginx access-log lines: not applicable (direct mode)\n'
  fi
  printf -- '- app shutdown summary responses: %s (accepted connections %s, rejected %s)\n\n' "$app_responses" "$app_accepted" "$app_rejected"

  printf '## Raw inputs\n\n'
  printf -- '- `meta.json`, `summary.json`, `run.json`, `k6-run.json`\n'
  printf -- '- `k6-summary.json` (k6 `--summary-export`)\n'
  printf -- '- `k6.log` (k6 stdout), `k6-stats.log` (docker stats samples)\n'
  printf -- '- `metrics.csv` (collector), `app.log`, `nginx-access.log`, `nginx-error.log`\n'
} > "$report"

cat "$report"
