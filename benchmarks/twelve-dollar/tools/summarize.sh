#!/usr/bin/env bash
set -euo pipefail

RUN_DIR="${1:?usage: summarize.sh <run-directory>}"

die() {
  echo "summarize: $*" >&2
  exit 1
}

for required in run.json k6-summary.json k6-run.json metrics.csv app.log k6.log; do
  [ -f "$RUN_DIR/$required" ] || die "missing $RUN_DIR/$required"
done

[ -f "$RUN_DIR/nginx-access.log" ] || : > "$RUN_DIR/nginx-access.log"

read_json() {
  jaq -r "$1 // empty" "$2" 2>/dev/null || true
}

k6_count="$(read_json '(.metrics.http_reqs.count // .metrics.http_reqs.values.count)' "$RUN_DIR/k6-summary.json")"
k6_rate="$(read_json '(.metrics.http_reqs.rate // .metrics.http_reqs.values.rate)' "$RUN_DIR/k6-summary.json")"
p50_raw="$(read_json '(.metrics.http_req_duration.med // .metrics.http_req_duration.values.med)' "$RUN_DIR/k6-summary.json")"
p95_raw="$(read_json '(.metrics.http_req_duration["p(95)"] // .metrics.http_req_duration.values["p(95)"])' "$RUN_DIR/k6-summary.json")"
p99_raw="$(read_json '(.metrics.http_req_duration["p(99)"] // .metrics.http_req_duration.values["p(99)"])' "$RUN_DIR/k6-summary.json")"
max_raw="$(read_json '(.metrics.http_req_duration.max // .metrics.http_req_duration.values.max)' "$RUN_DIR/k6-summary.json")"
fail_rate="$(read_json '(.metrics.http_req_failed.value // .metrics.http_req_failed.values.rate)' "$RUN_DIR/k6-summary.json")"
dropped="$(read_json '(.metrics.dropped_iterations.count // .metrics.dropped_iterations.values.count)' "$RUN_DIR/k6-summary.json")"
vus_max="$(read_json '(.metrics.vus.max // .metrics.vus.value // .metrics.vus.values.max)' "$RUN_DIR/k6-summary.json")"
k6_rc="$(read_json '.exit_code' "$RUN_DIR/k6-run.json")"

p50="${p50_raw:-0}"
p95="${p95_raw:-0}"
p99="${p99_raw:-0}"
max_ms="${max_raw:-0}"

k6_stdout_count="$(awk '/^[[:space:]]*http_reqs\.+:/ { sub(/^.*:[[:space:]]*/, ""); print $1; exit }' "$RUN_DIR/k6.log")"
nginx_lines="$(wc -l < "$RUN_DIR/nginx-access.log" | tr -d ' ')"
nginx_lines="${nginx_lines:-0}"

mode="$(read_json '.mode' "$RUN_DIR/run.json")"
vus="$(read_json '.vus' "$RUN_DIR/run.json")"
ramp="$(read_json '.ramp_up_s' "$RUN_DIR/run.json")"
hold="$(read_json '.hold_s' "$RUN_DIR/run.json")"
gate_before="$(jaq -r '.gate_before' "$RUN_DIR/run.json")"
gate_after="$(jaq -r '.gate_after' "$RUN_DIR/run.json")"
db_preexisting="$(jaq -r '.db_preexisting' "$RUN_DIR/run.json")"
challenge_dirty="$(jaq -r '.challenge.dirty' "$RUN_DIR/run.json")"

line="$(awk '/^h12tiny-twelve shutdown / { line = $0 } END { if (line != "") print line }' "$RUN_DIR/app.log")"
app_summary_present=false
[ -n "$line" ] && app_summary_present=true

field_value() {
  printf '%s\n' "$line" | tr ' ' '\n' | awk -F= -v key="$1" '$1 == key { print $2; exit }'
}

app_accepted="$(field_value accepted)"
app_rejected="$(field_value rejected)"
app_responses="$(field_value responses)"
app_accepted="${app_accepted:-0}"
app_rejected="${app_rejected:-0}"
app_responses="${app_responses:-0}"

status_app="{"
for code in 200 201 400 401 404 500; do
  value="$(field_value "status_$code")"
  status_app+="\"$code\":${value:-0},"
done
other="$(field_value status_other)"
status_app+="\"other\":${other:-0}}"

status_nginx="{"
nginx_status_rows="$(awk '{
  for (i = 1; i <= NF; i++) {
    if ($i ~ /"$/ && $(i + 1) ~ /^[0-9][0-9][0-9]$/) {
      count[$(i + 1)]++
      break
    }
  }
}
END {
  for (code in count) print code, count[code]
}' "$RUN_DIR/nginx-access.log" | sort -n)"
while read -r code count; do
  [ -n "$code" ] || continue
  status_nginx+="\"$code\":$count,"
done <<< "$nginx_status_rows"
if [ "$status_nginx" = "{" ]; then
  status_nginx="{}"
else
  status_nginx="${status_nginx%,}}"
fi

reader_threads="$(sed -n 's/.* reader_threads=\([0-9][0-9]*\).*/\1/p' "$RUN_DIR/app.log" | head -1)"
executor_threads="$(sed -n 's/.* executor_threads=\([0-9][0-9]*\).*/\1/p' "$RUN_DIR/app.log" | head -1)"

metrics="$(awk -F, '
NR == 1 {
  for (i = 1; i <= NF; i++) col[$i] = i
  next
}
function j(v) { return v == "" ? "null" : v }
{
  n++
  if (n == 1) {
    clk = $col["clk_tck"]
    app_pid = $col["app_pid"]
    elapsed_first = $col["elapsed_s"]
    db_first = $col["db_bytes"]
    wal_first = $col["wal_bytes"]
    if (col["cpu_nr_periods"] && col["cpu_nr_throttled"]) {
      periods_first = $col["cpu_nr_periods"]
      throttled_first = $col["cpu_nr_throttled"]
    }
  }
  elapsed_last = $col["elapsed_s"]
  if ($col["app_cpu_ticks"] != "") {
    if (app_first == "") app_first = $col["app_cpu_ticks"]
    app_last = $col["app_cpu_ticks"]
  }
  if ($col["nginx_cpu_ticks"] != "") {
    if (nginx_first == "") nginx_first = $col["nginx_cpu_ticks"]
    nginx_last = $col["nginx_cpu_ticks"]
  }
  rss = $col["app_rss_kib"] + 0
  if (rss > peak_rss) peak_rss = rss
  hwm = $col["app_hwm_kib"] + 0
  if (hwm > peak_hwm) peak_hwm = hwm
  established = $col["established"] + 0
  if (established > peak_established) peak_established = established
  if (col["app_fds"] && $col["app_fds"] != "" && (peak_fds == "" || $col["app_fds"] + 0 > peak_fds)) peak_fds = $col["app_fds"] + 0
  if (col["cpu_nr_periods"] && col["cpu_nr_throttled"]) {
    periods_last = $col["cpu_nr_periods"]
    throttled_last = $col["cpu_nr_throttled"]
  }
  db_last = $col["db_bytes"]
  wal_last = $col["wal_bytes"]
  if ($col["memory_peak_bytes"] != "") cgroup_peak = $col["memory_peak_bytes"]
  if ($col["memory_current_bytes"] != "") cgroup_current = $col["memory_current_bytes"]
  if ($col["pids_current"] != "") pids_current = $col["pids_current"]
}
END {
  printf "{\"samples\":%d,\"clk_tck\":%s,\"app_pid\":\"%s\",\"elapsed_first_s\":%s,\"elapsed_last_s\":%s,\"app_cpu_ticks_first\":%s,\"app_cpu_ticks_last\":%s,\"nginx_cpu_ticks_first\":%s,\"nginx_cpu_ticks_last\":%s,\"peak_rss_kib\":%s,\"peak_hwm_kib\":%s,\"peak_established\":%s,\"peak_fds\":%s,\"periods_first\":%s,\"periods_last\":%s,\"throttled_first\":%s,\"throttled_last\":%s,\"db_bytes_first\":%s,\"db_bytes_last\":%s,\"wal_bytes_first\":%s,\"wal_bytes_last\":%s,\"cgroup_peak_bytes\":%s,\"cgroup_current_last_bytes\":%s,\"pids_current_last\":%s}\n", \
    n, j(clk), app_pid, j(elapsed_first), j(elapsed_last), j(app_first), j(app_last), j(nginx_first), j(nginx_last), j(peak_rss), j(peak_hwm), j(peak_established), j(peak_fds), j(periods_first), j(periods_last), j(throttled_first), j(throttled_last), j(db_first), j(db_last), j(wal_first), j(wal_last), j(cgroup_peak), j(cgroup_current), j(pids_current)
}' "$RUN_DIR/metrics.csv")"

samples="$(jaq -r '.samples' <<< "$metrics")"
clk_tck="$(jaq -r '.clk_tck // 0' <<< "$metrics")"
app_pid="$(jaq -r '.app_pid // ""' <<< "$metrics")"
elapsed_first="$(jaq -r '.elapsed_first_s // 0' <<< "$metrics")"
elapsed_last="$(jaq -r '.elapsed_last_s // 0' <<< "$metrics")"
app_ticks_first="$(jaq -r '.app_cpu_ticks_first // ""' <<< "$metrics")"
app_ticks_last="$(jaq -r '.app_cpu_ticks_last // ""' <<< "$metrics")"
nginx_ticks_first="$(jaq -r '.nginx_cpu_ticks_first // ""' <<< "$metrics")"
nginx_ticks_last="$(jaq -r '.nginx_cpu_ticks_last // ""' <<< "$metrics")"
peak_rss_kib="$(jaq -r '.peak_rss_kib // 0' <<< "$metrics")"
peak_hwm_kib="$(jaq -r '.peak_hwm_kib // 0' <<< "$metrics")"
peak_established="$(jaq -r '.peak_established // 0' <<< "$metrics")"
peak_fds="$(jaq -r '.peak_fds // "null"' <<< "$metrics")"
throttled_percent="$(jaq -r 'if .periods_first != null and .periods_last != null and .throttled_first != null and .throttled_last != null and .periods_last > .periods_first and .throttled_last >= .throttled_first then 100 * (.throttled_last - .throttled_first) / (.periods_last - .periods_first) else null end' <<< "$metrics")"
db_bytes_first="$(jaq -r '.db_bytes_first // 0' <<< "$metrics")"
db_bytes_last="$(jaq -r '.db_bytes_last // 0' <<< "$metrics")"
wal_bytes_first="$(jaq -r '.wal_bytes_first // 0' <<< "$metrics")"
wal_bytes_last="$(jaq -r '.wal_bytes_last // 0' <<< "$metrics")"
cgroup_peak_bytes="$(jaq -r '.cgroup_peak_bytes // 0' <<< "$metrics")"

window_seconds="$(awk -v first="$elapsed_first" -v last="$elapsed_last" 'BEGIN { value = last - first; if (value < 0) value = 0; printf "%.3f", value }')"

app_cpu_seconds="$(awk -v first="$app_ticks_first" -v last="$app_ticks_last" -v clk="$clk_tck" 'BEGIN { if (first == "" || last == "" || clk == 0) { print "0" } else { printf "%.6f", (last - first) / clk } }')"
nginx_cpu_seconds="$(awk -v first="$nginx_ticks_first" -v last="$nginx_ticks_last" -v clk="$clk_tck" 'BEGIN { if (first == "" || last == "" || clk == 0) { print "0" } else { printf "%.6f", (last - first) / clk } }')"
combined_cpu_seconds="$(awk -v app="$app_cpu_seconds" -v nginx="$nginx_cpu_seconds" 'BEGIN { printf "%.6f", app + nginx }')"

per_request() {
  awk -v cpu="$1" -v count="$2" 'BEGIN { if (count == 0) print "0"; else printf "%.9f", cpu / count }'
}

app_cpu_per_request="$(per_request "$app_cpu_seconds" "$k6_count")"
nginx_cpu_per_request="$(per_request "$nginx_cpu_seconds" "$k6_count")"
combined_cpu_per_request="$(per_request "$combined_cpu_seconds" "$k6_count")"
utilization_percent="$(awk -v cpu="$combined_cpu_seconds" -v seconds="$window_seconds" 'BEGIN { if (seconds == 0) print "0"; else printf "%.2f", 100 * cpu / seconds }')"

db_growth_bytes=$((db_bytes_last - db_bytes_first))
wal_growth_bytes=$((wal_bytes_last - wal_bytes_first))
db_growth_mib="$(awk -v bytes="$db_growth_bytes" 'BEGIN { printf "%.3f", bytes / 1048576 }')"
wal_growth_mib="$(awk -v bytes="$wal_growth_bytes" 'BEGIN { printf "%.3f", bytes / 1048576 }')"
peak_rss_mib="$(awk -v kib="$peak_rss_kib" 'BEGIN { printf "%.3f", kib / 1024 }')"
peak_hwm_mib="$(awk -v kib="$peak_hwm_kib" 'BEGIN { printf "%.3f", kib / 1024 }')"
cgroup_peak_mib="$(awk -v bytes="$cgroup_peak_bytes" 'BEGIN { printf "%.3f", bytes / 1048576 }')"

if [ -s "$RUN_DIR/k6-stats.log" ]; then
  k6_max_cpu="$(jaq -s 'map(.CPUPerc | sub("%$"; "") | tonumber) | if length == 0 then 0 else max end' "$RUN_DIR/k6-stats.log" 2>/dev/null || echo 0)"
  k6_max_mem="$(jaq -s 'map(.MemPerc | sub("%$"; "") | tonumber) | if length == 0 then 0 else max end' "$RUN_DIR/k6-stats.log" 2>/dev/null || echo 0)"
else
  k6_max_cpu=0
  k6_max_mem=0
fi
k6_max_cpu="${k6_max_cpu:-0}"
k6_max_mem="${k6_max_mem:-0}"

summary_matches_stdout=false
if [ -n "$k6_stdout_count" ] && [ "$k6_count" = "$k6_stdout_count" ]; then
  summary_matches_stdout=true
fi

k6_matches_nginx=null
if [ "$mode" = nginx ]; then
  k6_matches_nginx=false
  if [ "$k6_count" = "$nginx_lines" ]; then
    k6_matches_nginx=true
  fi
fi

k6_cpu_within="$(awk -v value="$k6_max_cpu" 'BEGIN { print (value <= 640) ? "true" : "false" }')"
k6_mem_within="$(awk -v value="$k6_max_mem" 'BEGIN { print (value <= 80) ? "true" : "false" }')"

reasons='[]'
add_reason() {
  reasons="$(jaq -n --argjson current "$reasons" --arg reason "$1" '$current + [$reason]')"
}

[ "$gate_before" = true ] || add_reason "perf gate before the run did not pass"
[ "$gate_after" = true ] || add_reason "perf gate after the run did not pass"
case "$k6_rc" in
  0|99) ;;
  *) add_reason "k6 exited with status ${k6_rc:-unknown}" ;;
esac
[ "${dropped:-0}" = 0 ] || add_reason "k6 dropped ${dropped} iterations"
[ -n "$vus_max" ] || add_reason "k6 summary did not report active VUs"
if [ -n "$vus_max" ] && [ "$vus_max" -lt "$vus" ]; then
  add_reason "k6 reached ${vus_max} VUs, target was ${vus}"
fi
[ "$app_rejected" = 0 ] || add_reason "app shutdown summary reported ${app_rejected} rejected connections"
[ "$challenge_dirty" = false ] || add_reason "challenge checkout was dirty"
[ "$db_preexisting" = false ] || add_reason "database path existed before the run"
[ "$k6_cpu_within" = true ] || add_reason "k6 container CPU reached ${k6_max_cpu}% of 800% allocation"
[ "$k6_mem_within" = true ] || add_reason "k6 container memory reached ${k6_max_mem}% of allocation"
if ! jaq -se 'length > 0 and all(.[]; (.CPUPerc | type == "string" and test("^[0-9.]+%$")) and (.MemPerc | type == "string" and test("^[0-9.]+%$")))' "$RUN_DIR/k6-stats.log" >/dev/null 2>&1; then
  add_reason "k6 resource samples are missing or malformed"
fi
[ "$summary_matches_stdout" = true ] || add_reason "k6 summary http_reqs ${k6_count} did not match stdout ${k6_stdout_count:-missing}"
if [ "$mode" = nginx ] && [ "$k6_matches_nginx" != true ]; then
  add_reason "k6 http_reqs ${k6_count} did not match nginx access log ${nginx_lines} lines"
fi
[ "$app_summary_present" = true ] || add_reason "app shutdown summary line missing"
[ "$samples" -gt 0 ] || add_reason "collector produced no samples"
[ -n "$reader_threads" ] || add_reason "app startup line missing reader thread count"
[ -n "$executor_threads" ] || add_reason "app startup line missing executor thread count"
[ -n "$k6_count" ] || add_reason "k6 summary export missing http_reqs"
if ! jaq -e '
  .metrics as $m |
  [($m.http_req_duration.med // $m.http_req_duration.values.med),
   ($m.http_req_duration["p(95)"] // $m.http_req_duration.values["p(95)"]),
   ($m.http_req_duration["p(99)"] // $m.http_req_duration.values["p(99)"]),
   ($m.http_req_duration.max // $m.http_req_duration.values.max),
   ($m.http_req_failed.value // $m.http_req_failed.values.rate),
   ($m.http_reqs.count // $m.http_reqs.values.count),
   ($m.http_reqs.rate // $m.http_reqs.values.rate)] |
  all(.[]; type == "number" and . >= 0) and
  (($m.http_reqs.count // $m.http_reqs.values.count) > 0) and
  (($m.http_req_failed.value // $m.http_req_failed.values.rate) <= 1)
' "$RUN_DIR/k6-summary.json" >/dev/null 2>&1; then
  add_reason "k6 summary has missing or malformed request metrics"
fi

valid=true
[ "$(jaq 'length' <<< "$reasons")" = 0 ] || valid=false

jaq -n \
  --arg run_id "$(jaq -r '.run_id' "$RUN_DIR/run.json")" \
  --arg mode "$mode" \
  --argjson vus "$vus" \
  --argjson ramp_up_s "$ramp" \
  --argjson hold_s "$hold" \
  --argjson http_reqs "${k6_count:-0}" \
  --argjson http_reqs_stdout "${k6_stdout_count:-null}" \
  --argjson throughput_req_s "${k6_rate:-0}" \
  --argjson p50_ms "${p50:-0}" \
  --argjson p95_ms "${p95:-0}" \
  --argjson p99_ms "${p99:-0}" \
  --argjson max_ms "${max_ms:-0}" \
  --argjson failure_rate_percent "$(awk -v rate="${fail_rate:-0}" 'BEGIN { printf "%.6f", rate * 100 }')" \
  --argjson dropped_iterations "${dropped:-0}" \
  --argjson vus_max "${vus_max:-0}" \
  --argjson k6_exit_code "${k6_rc:-null}" \
  --argjson clk_tck "$clk_tck" \
  --argjson samples "$samples" \
  --argjson app_cpu_seconds "$app_cpu_seconds" \
  --argjson nginx_cpu_seconds "$nginx_cpu_seconds" \
  --argjson combined_cpu_seconds "$combined_cpu_seconds" \
  --argjson app_cpu_seconds_per_request "$app_cpu_per_request" \
  --argjson nginx_cpu_seconds_per_request "$nginx_cpu_per_request" \
  --argjson combined_cpu_seconds_per_request "$combined_cpu_per_request" \
  --argjson utilization_percent "$utilization_percent" \
  --argjson window_seconds "$window_seconds" \
  --argjson peak_rss_mib "$peak_rss_mib" \
  --argjson peak_hwm_mib "$peak_hwm_mib" \
  --argjson cgroup_peak_mib "$cgroup_peak_mib" \
  --argjson peak_established "$peak_established" \
  --argjson peak_fds "$peak_fds" \
  --argjson throttled_percent "$throttled_percent" \
  --argjson db_growth_mib "$db_growth_mib" \
  --argjson wal_growth_mib "$wal_growth_mib" \
  --argjson status_app "$status_app" \
  --argjson status_nginx "$status_nginx" \
  --argjson reader_threads "${reader_threads:-null}" \
  --argjson executor_threads "${executor_threads:-null}" \
  --argjson k6_max_cpu_percent "$k6_max_cpu" \
  --argjson k6_max_memory_percent "$k6_max_mem" \
  --argjson k6_cpu_within_allocation "$k6_cpu_within" \
  --argjson k6_memory_within_allocation "$k6_mem_within" \
  --argjson nginx_lines "$nginx_lines" \
  --argjson summary_matches_stdout "$summary_matches_stdout" \
  --argjson k6_matches_nginx "$k6_matches_nginx" \
  --argjson app_summary_present "$app_summary_present" \
  --argjson app_accepted "$app_accepted" \
  --argjson app_rejected "$app_rejected" \
  --argjson app_responses "$app_responses" \
  --argjson app_pid "${app_pid:-null}" \
  --argjson valid "$valid" \
  --argjson invalid_reasons "$reasons" \
  '{
    run_id: $run_id,
    mode: $mode,
    vus: $vus,
    ramp_up_s: $ramp_up_s,
    hold_s: $hold_s,
    k6: {
      http_reqs: $http_reqs,
      http_reqs_stdout: $http_reqs_stdout,
      throughput_req_s: $throughput_req_s,
      p50_ms: $p50_ms,
      p95_ms: $p95_ms,
      p99_ms: $p99_ms,
      max_ms: $max_ms,
      failure_rate_percent: $failure_rate_percent,
      dropped_iterations: $dropped_iterations,
      vus_max: $vus_max,
      exit_code: $k6_exit_code
    },
    cpu: {
      clk_tck: $clk_tck,
      app_cpu_seconds: $app_cpu_seconds,
      nginx_cpu_seconds: $nginx_cpu_seconds,
      combined_cpu_seconds: $combined_cpu_seconds,
      app_cpu_seconds_per_request: $app_cpu_seconds_per_request,
      nginx_cpu_seconds_per_request: $nginx_cpu_seconds_per_request,
      combined_cpu_seconds_per_request: $combined_cpu_seconds_per_request,
      utilization_percent: $utilization_percent,
      window_seconds: $window_seconds,
      throttled_period_percent: $throttled_percent
    },
    memory: {
      peak_rss_mib: $peak_rss_mib,
      peak_rss_hwm_mib: $peak_hwm_mib,
      cgroup_peak_mib: $cgroup_peak_mib
    },
    connections: {
      peak_established: $peak_established,
      peak_fds: $peak_fds
    },
    db: {
      db_growth_mib: $db_growth_mib,
      wal_growth_mib: $wal_growth_mib
    },
    status_app: $status_app,
    status_nginx: $status_nginx,
    threads: {
      reader: $reader_threads,
      executor: $executor_threads
    },
    counts: {
      k6_summary_http_reqs: $http_reqs,
      k6_stdout_http_reqs: $http_reqs_stdout,
      nginx_access_lines: $nginx_lines,
      app_accepted: $app_accepted,
      app_rejected: $app_rejected,
      app_responses: $app_responses,
      app_summary_present: $app_summary_present,
      summary_matches_stdout: $summary_matches_stdout,
      k6_matches_nginx: $k6_matches_nginx
    },
    collector: {
      samples: $samples,
      app_pid: $app_pid
    },
    k6_container: {
      max_cpu_percent: $k6_max_cpu_percent,
      max_memory_percent: $k6_max_memory_percent,
      cpu_within_allocation: $k6_cpu_within_allocation,
      memory_within_allocation: $k6_memory_within_allocation
    },
    valid: $valid,
    invalid_reasons: $invalid_reasons
  }' > "$RUN_DIR/summary.json"

jaq -n \
  --slurpfile run "$RUN_DIR/run.json" \
  --slurpfile summary "$RUN_DIR/summary.json" \
  --argjson k6_max_cpu_percent "$k6_max_cpu" \
  --argjson k6_max_memory_percent "$k6_max_mem" \
  --argjson k6_cpu_within_allocation "$k6_cpu_within" \
  --argjson k6_memory_within_allocation "$k6_mem_within" \
  --argjson reader_threads "${reader_threads:-null}" \
  --argjson executor_threads "${executor_threads:-null}" \
  --argjson valid "$valid" \
  --argjson invalid_reasons "$reasons" \
  '$run[0] + {
    threads: {reader: $reader_threads, executor: $executor_threads},
    k6_container: {
      max_cpu_percent: $k6_max_cpu_percent,
      max_memory_percent: $k6_max_memory_percent,
      cpu_within_allocation: $k6_cpu_within_allocation,
      memory_within_allocation: $k6_memory_within_allocation
    },
    validity: {valid: $valid, reasons: $invalid_reasons},
    measurements: $summary[0]
  }' > "$RUN_DIR/meta.json"

echo "summarize: wrote $RUN_DIR/meta.json and $RUN_DIR/summary.json (valid=$valid)"
if [ "$valid" != true ]; then
  printf 'summarize: invalid run: %s\n' "$(jaq -r '.invalid_reasons[]' "$RUN_DIR/summary.json")" >&2
fi
