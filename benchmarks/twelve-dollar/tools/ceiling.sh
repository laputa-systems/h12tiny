#!/usr/bin/env bash
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/perf-gate.sh"
source "$HERE/k6-summary.sh"
BENCH="$(cd "$HERE/.." && pwd)"

BOX=h12t-twelve-box
NET=h12t-twelve-net
RUNS_VOL=h12t-twelve-runs
K6_IMAGE=grafana/k6@sha256:3e4186aea62e11a45560bfc51e19eeafe82fc07ce069c78aff44b7ff893d9a5e
K6_NAME=h12t-twelve-k6
K6_CPU_LIMIT_PERCENT=640
K6_MEMORY_LIMIT_PERCENT=80
GATE_RETRY_SECONDS=600
GATE_ATTEMPTS=3

out=""
challenge=""
ramp_s=60
hold_s=60

usage() {
  cat >&2 <<'EOF'
usage: ceiling.sh --out DIR --challenge DIR [options]

options: --box NAME (h12t-twelve-box), --net NAME (h12t-twelve-net),
         --runs-volume NAME (h12t-twelve-runs), --k6-image IMAGE,
         --bench DIR,
         --ramp-up SECONDS (60), --duration SECONDS (60)

The search is cap-first: one probe at the D8 cap per mode, then halve until
an unsaturated level. Every probe is bracketed by perf-gate-ready.
EOF
}

die() {
  echo "ceiling: $*" >&2
  exit 1
}

while [ $# -gt 0 ]; do
  case "$1" in
    --out)
      out="${2:?--out needs a directory}"
      shift
      ;;
    --out=*)
      out="${1#*=}"
      ;;
    --challenge)
      challenge="${2:?--challenge needs a directory}"
      shift
      ;;
    --challenge=*)
      challenge="${1#*=}"
      ;;
    --box)
      BOX="${2:?--box needs a name}"
      shift
      ;;
    --box=*)
      BOX="${1#*=}"
      ;;
    --net)
      NET="${2:?--net needs a name}"
      shift
      ;;
    --net=*)
      NET="${1#*=}"
      ;;
    --runs-volume)
      RUNS_VOL="${2:?--runs-volume needs a name}"
      shift
      ;;
    --runs-volume=*)
      RUNS_VOL="${1#*=}"
      ;;
    --k6-image)
      K6_IMAGE="${2:?--k6-image needs an image}"
      shift
      ;;
    --k6-image=*)
      K6_IMAGE="${1#*=}"
      ;;
    --bench)
      BENCH="${2:?--bench needs a directory}"
      shift
      ;;
    --bench=*)
      BENCH="${1#*=}"
      ;;
    --ramp-up)
      ramp_s="${2:?--ramp-up needs seconds}"
      shift
      ;;
    --ramp-up=*)
      ramp_s="${1#*=}"
      ;;
    --duration)
      hold_s="${2:?--duration needs seconds}"
      shift
      ;;
    --duration=*)
      hold_s="${1#*=}"
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage
      exit 2
      ;;
  esac
  shift
done

[ -n "$out" ] || { usage; exit 2; }
[ -n "$challenge" ] || { usage; exit 2; }
[ -f "$BENCH/box/app.sh" ] || die "missing $BENCH/box/app.sh"
[ -f "$BENCH/box/nginx-check.sh" ] || die "missing $BENCH/box/nginx-check.sh"
[ -f "$BENCH/start-static.sh" ] || die "missing $BENCH/start-static.sh"
[ -f "$challenge/bench/load.js" ] || die "missing $challenge/bench/load.js"
[ -f "$challenge/seed/tokens.json" ] || die "missing $challenge/seed/tokens.json"
command -v docker >/dev/null 2>&1 || die "docker is required"
docker info >/dev/null 2>&1 || die "docker daemon is not reachable"
[ "$(docker inspect --format '{{.State.Running}}' "$BOX" 2>/dev/null || true)" = true ] || die "box $BOX is not running"

box_ip="$(docker inspect --format '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$BOX")"
[ -n "$box_ip" ] || die "cannot read the IP address of $BOX"
challenge_revision="$(git -C "$challenge" rev-parse HEAD)"
challenge_dirty=false
if [ -n "$(git -C "$challenge" status --porcelain)" ]; then
  challenge_dirty=true
fi
root="$(cd "$BENCH/../.." && pwd)"
h12tiny_revision="$(git -C "$root" rev-parse HEAD)"
h12tiny_dirty=false
if [ -n "$(git -C "$root" status --porcelain)" ]; then
  h12tiny_dirty=true
fi
started_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
run_id="$(basename "$out")"

mkdir -p "$out"
probe_seq=0
level_pid=""
cleanup() {
  if [ -n "$level_pid" ]; then kill -TERM "$level_pid" 2>/dev/null || true; wait "$level_pid" 2>/dev/null || true; fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

copy_harness() {
  local box_run="$1"
  docker exec "$BOX" mkdir -p "$box_run"
  docker exec "$BOX" chmod 777 "$box_run"
  docker cp "$BENCH/box/app.sh" "$BOX:$box_run/app.sh"
  docker cp "$BENCH/box/nginx-check.sh" "$BOX:$box_run/nginx-check.sh"
  docker cp "$BENCH/start-static.sh" "$BOX:$box_run/start-static.sh"
  docker exec "$BOX" chmod +x "$box_run/app.sh" "$box_run/nginx-check.sh" "$box_run/start-static.sh"
}

shutdown_line() {
  local box_run="$1"
  docker exec "$BOX" awk '/^h12tiny-static shutdown / { line = $0 } END { print line }' "$box_run/app.log" 2>/dev/null || true
}

field_value() {
  printf '%s' "$2" | tr ' ' '\n' | awk -F= -v key="$1" '$1 == key { print $2; exit }'
}

stats_max_cpu() {
  jaq -s 'map(.CPUPerc | sub("%$"; "") | tonumber) | if length == 0 then 0 else max end' "$1" 2>/dev/null || echo 0
}

stats_max_memory_percent() {
  jaq -s 'map(.MemPerc | sub("%$"; "") | tonumber) | if length == 0 then 0 else max end' "$1" 2>/dev/null || echo 0
}

stats_max_memory_mib() {
  jaq -r '.MemUsage' "$1" 2>/dev/null | awk '
    {
      split($1, parts, "/")
      value = parts[1]
      unit = value
      sub(/[0-9.]+/, "", unit)
      number = value + 0
      if (unit == "B") number = number / 1048576
      else if (unit == "KiB") number = number / 1024
      else if (unit == "GiB") number = number * 1024
      else if (unit == "TiB") number = number * 1048576
      if (number > max) max = number
    }
    END { if (max == "") print 0; else printf "%.3f", max }'
}

write_mode_state() {
  local mode="$1" cap="$2" status="$3" ceiling="$4" outcome="$5" last_valid="$6" lowest_saturated="$7" search_modified="$8"
  jaq -n \
    --arg mode "$mode" \
    --argjson cap "$cap" \
    --arg status "$status" \
    --argjson ceiling "${ceiling:-null}" \
    --arg outcome "$outcome" \
    --argjson last_valid "${last_valid:-null}" \
    --argjson lowest_saturated "${lowest_saturated:-null}" \
    --argjson search_modified "$search_modified" \
    '{
      mode: $mode,
      cap: $cap,
      status: $status,
      ceiling_vus: $ceiling,
      outcome: (if $outcome == "" then null else $outcome end),
      last_valid_vus: $last_valid,
      lowest_saturated_vus: $lowest_saturated,
      search_modified: $search_modified
    }' > "$out/.${mode}-state.json"
}

write_summary() {
  local final_status="$1"
  local files=()
  local file
  while IFS= read -r file; do
    files+=("$file")
  done < <(find "$out" -maxdepth 2 -name probe.json | sort)
  local nginx_state='{"mode":"nginx","status":"not run","ceiling_vus":null,"outcome":null,"last_valid_vus":null,"lowest_saturated_vus":null,"search_modified":false}'
  local direct_state='{"mode":"direct","status":"not run","ceiling_vus":null,"outcome":null,"last_valid_vus":null,"lowest_saturated_vus":null,"search_modified":false}'
  if [ -f "$out/.nginx-state.json" ]; then
    nginx_state="$(cat "$out/.nginx-state.json")"
  fi
  if [ -f "$out/.direct-state.json" ]; then
    direct_state="$(cat "$out/.direct-state.json")"
  fi
  local common=(
    --arg run_id "$run_id"
    --arg status "$final_status"
    --arg started_utc "$started_utc"
    --arg finished_utc "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
    --arg box "$BOX"
    --arg box_ip "$box_ip"
    --arg k6_image "$K6_IMAGE"
    --argjson k6_cpu_limit_percent "$K6_CPU_LIMIT_PERCENT"
    --argjson k6_memory_limit_percent "$K6_MEMORY_LIMIT_PERCENT"
    --arg challenge_revision "$challenge_revision"
    --argjson challenge_dirty "$challenge_dirty"
    --arg h12tiny_revision "$h12tiny_revision"
    --argjson h12tiny_dirty "$h12tiny_dirty"
    --argjson ramp_up_s "$ramp_s"
    --argjson hold_s "$hold_s"
    --argjson nginx_state "$nginx_state"
    --argjson direct_state "$direct_state"
  )
  if [ "${#files[@]}" -eq 0 ]; then
    jaq -n "${common[@]}" \
      '{
        run_id: $run_id,
        status: $status,
        started_utc: $started_utc,
        finished_utc: $finished_utc,
        box: {container: $box, ip: $box_ip},
        ramp_up_s: $ramp_up_s,
        hold_s: $hold_s,
        k6: {
          image: $k6_image,
          cpus: 8,
          memory: "10g",
          saturation_cpu_percent: $k6_cpu_limit_percent,
          saturation_memory_percent: $k6_memory_limit_percent,
          no_thresholds: true
        },
        challenge: {revision: $challenge_revision, dirty: $challenge_dirty},
        h12tiny: {revision: $h12tiny_revision, dirty: $h12tiny_dirty},
        probes: [],
        modes: {
          nginx: ($nginx_state + {probes: []}),
          direct: ($direct_state + {probes: []})
        }
      }' > "$out/ceiling.json"
    return 0
  fi
  cat "${files[@]}" | jaq -s "${common[@]}" \
    '{
      run_id: $run_id,
      status: $status,
      started_utc: $started_utc,
      finished_utc: $finished_utc,
      box: {container: $box, ip: $box_ip},
      ramp_up_s: $ramp_up_s,
      hold_s: $hold_s,
      k6: {
        image: $k6_image,
        cpus: 8,
        memory: "10g",
        saturation_cpu_percent: $k6_cpu_limit_percent,
        saturation_memory_percent: $k6_memory_limit_percent,
        no_thresholds: true
      },
      challenge: {revision: $challenge_revision, dirty: $challenge_dirty},
      h12tiny: {revision: $h12tiny_revision, dirty: $h12tiny_dirty},
      probes: .,
      modes: {
        nginx: ($nginx_state + {probes: [.[] | select(.mode == "nginx")]}),
        direct: ($direct_state + {probes: [.[] | select(.mode == "direct")]})
      }
    }' > "$out/ceiling.json"
}

defer_and_stop() {
  echo "perf gate deferred: host busy" >&2
  write_summary deferred
  echo "== partial record $out/ceiling.json"
  exit 75
}

wait_for_gate() {
  local gate_log="$1" attempt
  for attempt in $(seq 1 "$GATE_ATTEMPTS"); do
    printf 'gate attempt %s\n' "$attempt" >> "$gate_log"
    if quiet_perf_gate 2>> "$gate_log"; then
      printf 'gate passed\n' >> "$gate_log"
      return 0
    fi
    printf 'gate busy\n' >> "$gate_log"
    cat "$gate_log" >&2
    if [ "$attempt" -lt "$GATE_ATTEMPTS" ]; then
      echo "ceiling: perf gate busy, retrying in ${GATE_RETRY_SECONDS}s (attempt $attempt of $GATE_ATTEMPTS)" >&2
      sleep "$GATE_RETRY_SECONDS" &
      gate_sleep_pid=$!
      wait "$gate_sleep_pid"
      gate_sleep_pid=""
    fi
  done
  return 1
}

run_probe() {
  local mode="$1" vus="$2" name="$3"
  local host host_port
  if [ "$mode" = nginx ]; then
    host=127.0.0.1
    host_port=3000
  else
    host=0.0.0.0
    host_port=80
  fi

  local dir box_run run_started_utc app_pid stats_pid="" k6_pid="" gate_sleep_pid="" k6_started=false
  dir="$out/$name"
  box_run="/runs/ceiling-$name"
  [ ! -e "$dir" ] || die "probe directory already exists: $dir"
  mkdir -p "$dir"

  cleanup_probe() {
    if [ -n "$gate_sleep_pid" ]; then kill -TERM "$gate_sleep_pid" 2>/dev/null || true; wait "$gate_sleep_pid" 2>/dev/null || true; fi
    if [ "$k6_started" = true ]; then docker rm --force "$K6_NAME" >/dev/null 2>&1 || true; fi
    if [ -n "$k6_pid" ]; then kill -TERM "$k6_pid" 2>/dev/null || true; wait "$k6_pid" 2>/dev/null || true; fi
    if [ -n "${stats_pid:-}" ]; then
      touch "$dir/k6-stats.done" >/dev/null 2>&1 || true
      wait "$stats_pid" 2>/dev/null || true
    fi
    docker exec "$BOX" env PIDFILE="$box_run/app.pid" bash "$box_run/app.sh" stop >/dev/null 2>&1 || true
    docker exec "$BOX" bash "$box_run/nginx-check.sh" stop >/dev/null 2>&1 || true
    docker exec "$BOX" rm -rf "$box_run" >/dev/null 2>&1 || true
  }
  trap cleanup_probe EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM

  copy_harness "$box_run" >&2
  docker exec "$BOX" sh -c ': > /var/log/nginx/bench.access.log; : > /var/log/nginx/bench.error.log'
  if [ "$mode" = nginx ]; then
    docker exec "$BOX" bash "$box_run/nginx-check.sh" start >&2
  else
    docker exec "$BOX" bash "$box_run/nginx-check.sh" stop
  fi

  if [ -n "$(docker exec "$BOX" ss -H -ltn "sport = :$host_port")" ]; then
    docker exec "$BOX" ss -H -ltnp "sport = :$host_port" >&2
    die "port $host_port is already in use in $BOX"
  fi
  local stale
  stale="$(docker exec "$BOX" pgrep -x h12tiny-static 2>/dev/null || true)"
  [ -z "$stale" ] || die "a stale h12tiny-static process is running in $BOX"

  run_started_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "== probe $name ($mode, $vus VUs, ramp ${ramp_s}s, hold ${hold_s}s)" >&2
  docker exec -d "$BOX" env \
    HOST="$host" \
    PORT="$host_port" \
    START_SH="$box_run/start-static.sh" \
    LOG="$box_run/app.log" \
    PIDFILE="$box_run/app.pid" \
    bash "$box_run/app.sh" start
  app_pid=""
  for _ in $(seq 50); do
    app_pid="$(docker exec "$BOX" cat "$box_run/app.pid" 2>/dev/null || true)"
    if [ -n "$app_pid" ]; then
      break
    fi
    sleep 0.1
  done
  [ -n "$app_pid" ] || die "static app start did not write a pid"
  if ! docker exec "$BOX" env HOST="$host" PORT="$host_port" LOG="$box_run/app.log" PIDFILE="$box_run/app.pid" bash "$box_run/app.sh" wait; then
    die "static app did not become healthy"
  fi

  if ! wait_for_gate "$dir/gate-before.log"; then
    exit 75
  fi
  local gate_before=true

  docker rm --force "$K6_NAME" >/dev/null 2>&1 || true
  : > "$dir/k6-stats.log"
  rm -f "$dir/k6-stats.done"
  (
    while [ ! -f "$dir/k6-stats.done" ]; do
      if docker inspect "$K6_NAME" >/dev/null 2>&1; then
        docker stats --no-stream --format '{{json .}}' "$K6_NAME" >> "$dir/k6-stats.log" 2>/dev/null || true
      fi
      sleep 1
    done
  ) &
  stats_pid=$!

  local k6_started_ms k6_finished_ms k6_rc
  k6_started_ms="$(( $(date +%s) * 1000 ))"
  k6_started=true
  docker run --rm \
    --name "$K6_NAME" \
    --network "$NET" \
    --cpus=8 \
    --memory=10g \
    --ulimit nofile=65535:65535 \
    --volume "$challenge:/challenge:ro" \
    --volume "$RUNS_VOL:/runs" \
    --env BASE_URL="http://$box_ip:80" \
    --env VUS="$vus" \
    --env DURATION="${hold_s}s" \
    --env RAMP_UP="${ramp_s}s" \
    --env TOKENS=/challenge/seed/tokens.json \
    "$K6_IMAGE" run \
    --no-thresholds \
    --summary-export "/runs/ceiling-$name/k6-summary.json" \
    /challenge/bench/load.js > "$dir/k6.log" 2>&1 &
  k6_pid=$!
  if wait "$k6_pid"; then k6_rc=0; else k6_rc=$?; fi
  k6_pid=""
  k6_started=false
  k6_finished_ms="$(( $(date +%s) * 1000 ))"

  local gate_after=false
  if quiet_perf_gate -1 2> "$dir/gate-after.log"; then
    gate_after=true
  else
    cat "$dir/gate-after.log" >&2
  fi

  touch "$dir/k6-stats.done"
  wait "$stats_pid" 2>/dev/null || true
  rm -f "$dir/k6-stats.done"
  stats_pid=""

  docker exec "$BOX" env PIDFILE="$box_run/app.pid" bash "$box_run/app.sh" stop >/dev/null 2>&1 || true
  local summary_line app_log
  summary_line="$(shutdown_line "$box_run")"
  app_log="$(docker exec "$BOX" cat "$box_run/app.log" 2>/dev/null || true)"
  printf '%s\n' "$summary_line" > "$dir/shutdown.txt"
  printf '%s\n' "$app_log" > "$dir/app.log"
  docker exec "$BOX" bash "$box_run/nginx-check.sh" stop >/dev/null 2>&1 || true

  if docker exec "$BOX" test -f "/runs/ceiling-$name/k6-summary.json"; then
    docker cp "$BOX:/runs/ceiling-$name/k6-summary.json" "$dir/k6-summary.json"
  fi
  if [ "$mode" = nginx ]; then
    docker cp "$BOX:/var/log/nginx/bench.access.log" "$dir/nginx-access.log" 2>/dev/null || true
    docker cp "$BOX:/var/log/nginx/bench.error.log" "$dir/nginx-error.log" 2>/dev/null || true
  fi
  docker exec "$BOX" rm -rf "$box_run" >/dev/null 2>&1 || true

  local accepted rejected responses has_summary=false
  accepted="$(field_value accepted "$summary_line")"
  rejected="$(field_value rejected "$summary_line")"
  responses="$(field_value responses "$summary_line")"
  accepted="${accepted:-0}"
  rejected="${rejected:-0}"
  responses="${responses:-0}"
  if [ -f "$dir/k6-summary.json" ]; then
    has_summary=true
  fi

  local http_reqs=0 http_reqs_stdout=0 throughput=0 p50=0 p95=0 p99=0 failure_rate=0 dropped=0 vus_max=0
  if [ "$has_summary" = true ]; then
    http_reqs="$(jaq -r '.metrics.http_reqs.count // 0' "$dir/k6-summary.json" 2>/dev/null || echo 0)"
    throughput="$(jaq -r '.metrics.http_reqs.rate // 0' "$dir/k6-summary.json" 2>/dev/null || echo 0)"
    p50="$(jaq -r '.metrics.http_req_duration.med // 0' "$dir/k6-summary.json" 2>/dev/null || echo 0)"
    p95="$(jaq -r '.metrics.http_req_duration["p(95)"] // 0' "$dir/k6-summary.json" 2>/dev/null || echo 0)"
    p99="$(jaq -r '.metrics.http_req_duration["p(99)"] // 0' "$dir/k6-summary.json" 2>/dev/null || echo 0)"
    failure_rate="$(jaq -r '.metrics.http_req_failed.value // 0' "$dir/k6-summary.json" 2>/dev/null || echo 0)"
    dropped="$(jaq -r '.metrics.dropped_iterations.count // 0' "$dir/k6-summary.json" 2>/dev/null || echo 0)"
    vus_max="$(jaq -r '.metrics.vus_max.max // 0' "$dir/k6-summary.json" 2>/dev/null || echo 0)"
  fi
  http_reqs_stdout="$(awk '/^[[:space:]]*http_reqs\.+:/ { sub(/^.*:[[:space:]]*/, ""); print $1; exit }' "$dir/k6.log")"
  http_reqs_stdout="${http_reqs_stdout:-0}"

  local k6_cpu k6_mem k6_mem_mib
  k6_cpu="$(stats_max_cpu "$dir/k6-stats.log")"
  k6_mem="$(stats_max_memory_percent "$dir/k6-stats.log")"
  k6_mem_mib="$(stats_max_memory_mib "$dir/k6-stats.log")"
  k6_cpu="${k6_cpu:-0}"
  k6_mem="${k6_mem:-0}"
  k6_mem_mib="${k6_mem_mib:-0}"

  local reasons='[]'
  add_reason() {
    reasons="$(jaq -n --argjson current "$reasons" --arg reason "$1" '$current + [$reason]')"
  }
  local cpu_saturated mem_saturated dropped_saturated vus_saturated
  cpu_saturated="$(awk -v value="$k6_cpu" -v limit="$K6_CPU_LIMIT_PERCENT" 'BEGIN { print (value >= limit) ? "true" : "false" }')"
  mem_saturated="$(awk -v value="$k6_mem" -v limit="$K6_MEMORY_LIMIT_PERCENT" 'BEGIN { print (value >= limit) ? "true" : "false" }')"
  dropped_saturated=false
  if [ "$dropped" -gt 0 ] 2>/dev/null; then
    dropped_saturated=true
  fi
  vus_saturated=false
  if [ "$has_summary" = true ] && [ "$vus_max" -lt "$vus" ] 2>/dev/null; then
    vus_saturated=true
  fi
  if [ "$cpu_saturated" = true ]; then
    add_reason "k6 CPU ${k6_cpu}% reached ${K6_CPU_LIMIT_PERCENT}% of the 800% allocation"
  fi
  if [ "$mem_saturated" = true ]; then
    add_reason "k6 memory ${k6_mem}% reached ${K6_MEMORY_LIMIT_PERCENT}% of the 10 GiB allocation"
  fi
  if [ "$dropped_saturated" = true ]; then
    add_reason "k6 dropped ${dropped} iterations"
  fi
  if [ "$vus_saturated" = true ]; then
    add_reason "k6 reached ${vus_max} VUs, target was ${vus}"
  fi
  local saturated=false
  if [ "$(jaq 'length' <<< "$reasons")" -gt 0 ]; then
    saturated=true
  fi

  local invalid='[]'
  add_invalid() {
    invalid="$(jaq -n --argjson current "$invalid" --arg reason "$1" '$current + [$reason]')"
  }
  if ! jaq -se 'length > 0 and all(.[]; (.CPUPerc | type == "string" and test("^[0-9.]+%$")) and (.MemPerc | type == "string" and test("^[0-9.]+%$")) and (.MemUsage | type == "string" and test("^[0-9.]+(B|KiB|MiB|GiB|TiB) /")))' "$dir/k6-stats.log" >/dev/null 2>&1; then
    add_invalid "k6 resource samples are missing or malformed"
  fi
  if [ "$k6_rc" -ne 0 ]; then
    add_invalid "k6 exited with status $k6_rc"
  fi
  if [ "$gate_before" != true ]; then
    add_invalid "perf gate before the probe did not pass"
  fi
  if [ "$gate_after" != true ]; then
    add_invalid "perf gate after the probe did not pass"
  fi
  if [ "$has_summary" != true ]; then
    add_invalid "k6 did not write a summary export"
  elif ! valid_k6_generator_metrics "$dir/k6-summary.json"; then
    add_invalid "k6 summary has missing or malformed request or generator metrics"
  fi
  if [ "$http_reqs" != "$http_reqs_stdout" ]; then
    add_invalid "k6 summary and stdout request counts differ"
  fi
  if [ -z "$summary_line" ]; then
    add_invalid "static app shutdown summary is missing"
  fi
  if [ "$rejected" -gt 0 ]; then
    add_invalid "the static app rejected $rejected connections"
  fi
  if [ "$challenge_dirty" != false ]; then
    add_invalid "challenge checkout was dirty"
  fi
  local valid=true
  if [ "$(jaq 'length' <<< "$invalid")" -gt 0 ]; then
    valid=false
  fi

  local window_s
  window_s="$(awk -v start="$k6_started_ms" -v finish="$k6_finished_ms" 'BEGIN { printf "%.3f", (finish - start) / 1000 }')"

  jaq -n \
    --arg mode "$mode" \
    --argjson vus "$vus" \
    --argjson ramp_up_s "$ramp_s" \
    --argjson hold_s "$hold_s" \
    --arg started_utc "$run_started_utc" \
    --arg finished_utc "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --argjson window_s "$window_s" \
    --argjson exit_code "$k6_rc" \
    --argjson http_reqs "$http_reqs" \
    --argjson http_reqs_stdout "$http_reqs_stdout" \
    --argjson throughput_req_s "$throughput" \
    --argjson p50_ms "$p50" \
    --argjson p95_ms "$p95" \
    --argjson p99_ms "$p99" \
    --argjson failure_rate "$failure_rate" \
    --argjson dropped_iterations "$dropped" \
    --argjson vus_max "$vus_max" \
    --argjson k6_cpu_percent "$k6_cpu" \
    --argjson k6_memory_percent "$k6_mem" \
    --argjson k6_memory_mib "$k6_mem_mib" \
    --argjson gate_before "$gate_before" \
    --argjson gate_after "$gate_after" \
    --argjson saturated "$saturated" \
    --argjson saturation_reasons "$reasons" \
    --argjson valid "$valid" \
    --argjson invalid_reasons "$invalid" \
    --argjson app_accepted "$accepted" \
    --argjson app_rejected "$rejected" \
    --argjson app_responses "$responses" \
    --argjson app_pid "${app_pid:-null}" \
    '{
      mode: $mode,
      vus: $vus,
      ramp_up_s: $ramp_up_s,
      hold_s: $hold_s,
      started_utc: $started_utc,
      finished_utc: $finished_utc,
      window_s: $window_s,
      k6: {
        exit_code: $exit_code,
        http_reqs: $http_reqs,
        http_reqs_stdout: $http_reqs_stdout,
        throughput_req_s: $throughput_req_s,
        p50_ms: $p50_ms,
        p95_ms: $p95_ms,
        p99_ms: $p99_ms,
        failure_rate: $failure_rate,
        dropped_iterations: $dropped_iterations,
        vus_max: $vus_max,
        max_cpu_percent: $k6_cpu_percent,
        max_memory_percent: $k6_memory_percent,
        max_memory_mib: $k6_memory_mib
      },
      gate: {before: $gate_before, after: $gate_after},
      saturated: $saturated,
      saturation_reasons: $saturation_reasons,
      valid: $valid,
      invalid_reasons: $invalid_reasons,
      app: {
        pid: $app_pid,
        accepted: $app_accepted,
        rejected: $app_rejected,
        responses: $app_responses
      }
    }' > "$dir/probe.json"

  trap - EXIT
  echo "   k6 exit=$k6_rc reqs=$http_reqs vus_max=$vus_max dropped=$dropped cpu=${k6_cpu}% mem=${k6_mem}% (${k6_mem_mib} MiB) gate=$gate_before/$gate_after saturated=$saturated valid=$valid" >&2
  printf '%s\n' "$dir"
}

probe_level() {
  local mode="$1" vus="$2" name="$3"
  local attempt dir valid gate_after rc probe_pid=""
  cleanup_level() {
    if [ -n "$probe_pid" ]; then kill -TERM "$probe_pid" 2>/dev/null || true; wait "$probe_pid" 2>/dev/null || true; fi
  }
  trap cleanup_level EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  for attempt in $(seq 1 "$GATE_ATTEMPTS"); do
    dir="$out/$name-a$attempt"
    run_probe "$mode" "$vus" "$name-a$attempt" >/dev/null &
    probe_pid=$!
    if wait "$probe_pid"; then rc=0; else rc=$?; fi
    probe_pid=""
    if [ "$rc" -eq 75 ]; then
      return 75
    fi
    if [ "$rc" -ne 0 ]; then
      return "$rc"
    fi
    valid="$(jaq -r '.valid' "$dir/probe.json")"
    if [ "$valid" = true ]; then
      printf '%s\n' "$dir"
      return 0
    fi
    gate_after="$(jaq -r '.gate.after' "$dir/probe.json")"
    echo "ceiling: probe $dir is not valid: $(jaq -r '.invalid_reasons | join("; ")' "$dir/probe.json")" >&2
    if [ "$gate_after" = true ]; then
      return 2
    fi
    if [ "$attempt" -lt "$GATE_ATTEMPTS" ]; then
      echo "ceiling: probe overlapped load; retrying $mode at $vus VUs in ${GATE_RETRY_SECONDS}s" >&2
      sleep "$GATE_RETRY_SECONDS" &
      probe_pid=$!
      wait "$probe_pid"
      probe_pid=""
    fi
  done
  return 75
}

run_mode() {
  local mode="$1" cap="$2"
  local levels=()
  if [ "$mode" = nginx ]; then
    levels=(14000 7000 3500 2500)
  else
    levels=(25000 12500 6250 2500)
  fi
  local lowest_saturated="" last_valid="" ceiling="" outcome="" status=complete search_modified=false
  local vus dir rc probe_saturated name selected
  for vus in "${levels[@]}"; do
    probe_seq=$((probe_seq + 1))
    name="$(printf '%02d-%s-%05dvu' "$probe_seq" "$mode" "$vus")"
    selected="$out/.$name-selected"
    probe_level "$mode" "$vus" "$name" > "$selected" &
    level_pid=$!
    if wait "$level_pid"; then rc=0; else rc=$?; fi
    level_pid=""
    dir="$(cat "$selected")"
    rm -f "$selected"
    if [ "$rc" -ne 0 ]; then
      if [ "$rc" -eq 75 ]; then
        status=deferred
      else
        status=invalid
      fi
      break
    fi
    probe_saturated="$(jaq -r '.saturated' "$dir/probe.json")"
    if [ "$probe_saturated" = true ]; then
      lowest_saturated="$vus"
      echo "ceiling: $mode at $vus VUs is generator-saturated" >&2
      continue
    fi
    last_valid="$vus"
    if [ -z "$lowest_saturated" ]; then
      ceiling="$cap"
      outcome=cap
    else
      ceiling=$(( lowest_saturated * 80 / 100 ))
      outcome=saturated
    fi
    break
  done
  if [ -z "$ceiling" ] && [ -n "$lowest_saturated" ]; then
    ceiling=$(( lowest_saturated * 80 / 100 ))
    outcome=saturated
    if [ "$lowest_saturated" -eq 2500 ]; then
      search_modified=true
    fi
  fi
  if [ "$status" != complete ]; then
    ceiling=""
    outcome=""
  fi
  write_mode_state "$mode" "$cap" "$status" "$ceiling" "$outcome" "$last_valid" "$lowest_saturated" "$search_modified"
  echo "== $mode ceiling: ${ceiling:-not established} (outcome ${outcome:-none}, last valid ${last_valid:-none}, status $status)"
  if [ "$status" = deferred ]; then
    defer_and_stop
  fi
}

run_mode nginx 14000
run_mode direct 25000

final_status=complete
for mode in nginx direct; do
  status="$(jaq -r '.status' "$out/.$mode-state.json")"
  if [ "$status" != complete ]; then
    final_status=incomplete
  fi
done
write_summary "$final_status"

echo "== ceiling record $out/ceiling.json"
jaq -r '.modes | to_entries[] | .value as $mode | ($mode.probes | map(select(.vus == $mode.last_valid_vus and .valid == true)) | last // ($mode.probes | map(select(.valid == true)) | last) // ($mode.probes | last)) as $probe | "\(.key): ceiling=\($mode.ceiling_vus // "not established") outcome=\($mode.outcome // "none") last_valid=\($mode.last_valid_vus // "none") k6_cpu=\($probe.k6.max_cpu_percent // "none")% k6_mem=\($probe.k6.max_memory_percent // "none")% (\($probe.k6.max_memory_mib // "none") MiB)"' "$out/ceiling.json"

if [ "$final_status" != complete ]; then
  exit 1
fi
