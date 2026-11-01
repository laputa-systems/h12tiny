#!/usr/bin/env bash
set -euo pipefail

bench="$1"
challenge="$2"
result="$3"
vus="$4"
ramp="$5"
hold="$6"
readers="$7"
image="$8"
network="$9"

[ "$(uname -s)" = Darwin ] || { echo 'macOS profile requires macOS' >&2; exit 1; }
[ ! -e "$result" ] || { echo "result already exists: $result" >&2; exit 1; }
[ -z "$(git -C "$challenge" status --porcelain)" ] || { echo 'challenge checkout is dirty' >&2; exit 1; }
[ -z "$(lsof -nP -iTCP:3000 -sTCP:LISTEN -t)" ] || { echo 'port 3000 is already in use' >&2; exit 1; }
dirty=false
if [ -n "$(git -C "$bench/../.." status --porcelain)" ]; then dirty=true; fi
mkdir -p "$result"
cp "$challenge/seed/feed.db" "$result/feed.db"
bash "$bench/build.sh"
app_pid=""
time_pid=""
stats_pid=""
cleanup() {
  if [ -n "$stats_pid" ]; then touch "$result/stats.done"; wait "$stats_pid" 2>/dev/null || true; fi
  if [ -n "$app_pid" ]; then kill -TERM "$app_pid" 2>/dev/null || true; fi
  if [ -n "$time_pid" ]; then wait "$time_pid" 2>/dev/null || true; fi
  docker rm -f h12t-twelve-k6 >/dev/null 2>&1 || true
  rm -f "$result/feed.db" "$result/feed.db-wal" "$result/feed.db-shm" "$result/stats.done"
}
trap cleanup EXIT
(
  ulimit -n 65535
  export SQLITE_PATH="$result/feed.db" JWT_SECRET=twelve-dollar-challenge HOST=0.0.0.0 PORT=3000 TWELVE_DOLLAR_READERS="$readers"
  exec /usr/bin/time -l -o "$result/time.log" bash "$bench/start.sh"
) > "$result/app.log" 2>&1 &
time_pid=$!
for _ in $(seq 1 60); do
  app_pid="$(pgrep -P "$time_pid" | head -1 || true)"
  if [ -n "$app_pid" ] && curl -fsS http://127.0.0.1:3000/health >/dev/null 2>&1; then break; fi
  kill -0 "$time_pid" 2>/dev/null || { echo 'server exited before becoming healthy' >&2; exit 1; }
  sleep 1
done
[ -n "$app_pid" ] && curl -fsS http://127.0.0.1:3000/health >/dev/null || { echo 'server did not become healthy' >&2; exit 1; }
if ! perf-gate-ready; then echo 'perf gate deferred: host busy' >&2; exit 75; fi
: > "$result/k6-stats.log"
(
  while [ ! -e "$result/stats.done" ]; do
    if docker inspect h12t-twelve-k6 >/dev/null 2>&1; then
      docker stats --no-stream --format '{{json .}}' h12t-twelve-k6 >> "$result/k6-stats.log" 2>/dev/null || true
    fi
    sleep 1
  done
) &
stats_pid=$!
set +e
docker run --rm --name h12t-twelve-k6 --network "$network" --cpus=8 --memory=10g --ulimit nofile=65535:65535 \
  --volume "$challenge:/challenge:ro" --volume "$result:/result" \
  --env BASE_URL=http://host.docker.internal:3000 --env VUS="$vus" --env RAMP_UP="${ramp}s" \
  --env DURATION="${hold}s" --env TOKENS=/challenge/seed/tokens.json \
  "$image" run --summary-export /result/k6-summary.json /challenge/bench/load.js > "$result/k6.log" 2>&1
k6_rc=$?
set -e
gate_after=false
if perf-gate-ready -1; then gate_after=true; fi
touch "$result/stats.done"
wait "$stats_pid" || true
stats_pid=""
kill -TERM "$app_pid"
app_pid=""
wait "$time_pid"
time_pid=""
if [ "$gate_after" != true ]; then echo 'perf gate deferred: host busy' >&2; exit 75; fi
[ "$k6_rc" -eq 0 ] || { echo "k6 exited $k6_rc" >&2; exit 1; }
[ -s "$result/k6-summary.json" ] || { echo 'missing k6 summary' >&2; exit 1; }
[ -s "$result/k6-stats.log" ] || { echo 'missing k6 resource samples' >&2; exit 1; }
jaq -se 'all(.[]; (.CPUPerc | sub("%$"; "") | tonumber) < 640 and (.MemPerc | sub("%$"; "") | tonumber) < 80)' "$result/k6-stats.log" >/dev/null || { echo 'k6 resource limit reached or samples invalid' >&2; exit 1; }
jaq -e --argjson vus "$vus" '(.metrics.vus_max.max >= $vus) and ((.metrics.dropped_iterations.count // 0) == 0) and (.metrics.http_req_failed.value == 0)' "$result/k6-summary.json" >/dev/null || { echo 'k6 run is invalid or had failed requests' >&2; exit 1; }
awk '/^h12tiny-twelve shutdown / { found=1; if ($0 !~ /rejected=0([[:space:]]|$)/) exit 1 } END { if (!found) exit 1 }' "$result/app.log" || { echo 'missing shutdown summary or rejected connections' >&2; exit 1; }
[ -z "$(git -C "$challenge" status --porcelain)" ] || { echo 'challenge checkout became dirty' >&2; exit 1; }
cpu_seconds="$(awk '/ real / && / user / && / sys/ { print $3 + $5; found=1 } END { if (!found) exit 1 }' "$result/time.log")"
peak_rss="$(awk '/maximum resident set size/ { print $1; found=1 } END { if (!found) exit 1 }' "$result/time.log")"
jaq -n --arg profile macos --arg mode direct --argjson vus "$vus" --argjson ramp "$ramp" --argjson hold "$hold" --argjson readers "$readers" --arg revision "$(git -C "$bench/../.." rev-parse HEAD)" --argjson dirty "$dirty" --arg challenge "$(git -C "$challenge" rev-parse HEAD)" '{profile:$profile,mode:$mode,vus:$vus,ramp_up_s:$ramp,hold_s:$hold,reader_threads:$readers,h12tiny:{revision:$revision,dirty:$dirty},challenge:{revision:$challenge,dirty:false},gate_before:true,gate_after:true,valid:true}' > "$result/run.json"
jaq --argjson cpu "$cpu_seconds" --argjson rss "$peak_rss" '{valid:true,cpu_seconds:$cpu,peak_rss_bytes:$rss,k6:{http_reqs:.metrics.http_reqs.count,throughput_req_s:.metrics.http_reqs.rate,p50_ms:.metrics.http_req_duration.med,p95_ms:.metrics.http_req_duration["p(95)"],p99_ms:.metrics.http_req_duration["p(99)"],failure_rate:.metrics.http_req_failed.value}} | .cpu_seconds_per_request = (.cpu_seconds / .k6.http_reqs)' "$result/k6-summary.json" > "$result/summary.json"
echo "== macOS smoke results $result"
