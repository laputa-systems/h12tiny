#!/usr/bin/env bash
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$HERE/tools/perf-gate.sh"
ROOT="$(cd "$HERE/../.." && pwd)"
COMMON="$(git -C "$ROOT" rev-parse --path-format=absolute --git-common-dir)"
MAIN_ROOT="$(cd "$COMMON/.." && pwd)"
CHALLENGE="$MAIN_ROOT/.work/twelve-dollar/challenge"
STATE="$ROOT/.work/twelve-dollar"
BOX_IN_CONTAINER=/work/benchmarks/twelve-dollar/box

IMAGE=h12t-twelve-box:local
BOX=h12t-twelve-box
BUILDER=h12t-twelve-builder
NET=h12t-twelve-net
SEED_VOL=h12t-twelve-seed
CARGO_VOL=h12t-twelve-cargo-home
TARGET_VOL=h12t-twelve-cargo-target
RUNS_VOL=h12t-twelve-runs
K6_IMAGE=grafana/k6@sha256:3e4186aea62e11a45560bfc51e19eeafe82fc07ce069c78aff44b7ff893d9a5e
K6_NAME=h12t-twelve-k6

usage() {
  cat >&2 <<'EOF'
usage: run.sh [box|macos] [--profile box|macos] [--mode nginx|direct] [options] <command>

commands: setup, box-check, test, smoke, ceiling, search, confirm
not implemented yet: attribute, report

smoke options: --vus N (50), --duration 30s, --ramp-up 10s, --readers N (2)
ceiling always probes both modes with 60 s ramp and 60 s hold

defaults: --profile box --mode nginx
EOF
}

die() {
  echo "$*" >&2
  exit 1
}

require_docker() {
  command -v docker >/dev/null 2>&1 || die "docker is required"
  docker info >/dev/null 2>&1 || die "docker daemon is not reachable"
}

require_challenge() {
  [ -f "$CHALLENGE/test/test.sh" ] || die "missing $CHALLENGE/test/test.sh; run benchmarks/twelve-dollar/setup.sh"
  [ -f "$CHALLENGE/bench/nginx.conf" ] || die "missing $CHALLENGE/bench/nginx.conf; run benchmarks/twelve-dollar/setup.sh"
  [ -f "$CHALLENGE/seed/feed.db" ] || die "missing $CHALLENGE/seed/feed.db; run benchmarks/twelve-dollar/setup.sh"
}

box_running() {
  [ "$(docker inspect --format '{{.State.Running}}' "$BOX" 2>/dev/null || true)" = true ]
}

ensure_volume() {
  docker volume inspect "$1" >/dev/null 2>&1 || docker volume create "$1" >/dev/null
}

ensure_network() {
  docker network inspect "$NET" >/dev/null 2>&1 || docker network create "$NET" >/dev/null
}

build_image() {
  toolchain="$(sed -n 's/^channel[[:space:]]*=[[:space:]]*"\(.*\)"$/\1/p' "$ROOT/rust-toolchain.toml")"
  [ -n "$toolchain" ] || die "cannot read the toolchain channel from $ROOT/rust-toolchain.toml"
  echo "== image $IMAGE (Rust $toolchain)"
  docker build \
    --build-arg "RUST_TOOLCHAIN=$toolchain" \
    --file "$HERE/linux/Dockerfile.box" \
    --tag "$IMAGE" \
    "$HERE/linux"
}

copy_seed() {
  echo "== seed volume $SEED_VOL"
  ensure_volume "$SEED_VOL"
  docker rm --force h12t-twelve-seed-copy >/dev/null 2>&1 || true
  docker run --rm --interactive \
    --name h12t-twelve-seed-copy \
    --volume "$SEED_VOL:/seed" \
    "$IMAGE" sh -c 'cat > /seed/feed.db' < "$CHALLENGE/seed/feed.db"
}

create_box() {
  docker rm --force "$BOX" >/dev/null 2>&1 || true
  docker run --detach \
    --name "$BOX" \
    --hostname "$BOX" \
    --init \
    --network "$NET" \
    --cpus=1 \
    --memory=2g \
    --memory-swap=2g \
    --ulimit nofile=65535:65535 \
    --volume "$ROOT:/work" \
    --volume "$CHALLENGE/bench/nginx.conf:/etc/nginx/conf.d/default.conf:ro" \
    --volume "$CHALLENGE/test:/challenge/test:ro" \
    --volume "$SEED_VOL:/seed:ro" \
    --volume "$RUNS_VOL:/runs" \
    --volume "$CARGO_VOL:/cargo-home" \
    --volume "$TARGET_VOL:/cargo-target" \
    --env CARGO_HOME=/cargo-home \
    --env CARGO_TARGET_DIR=/cargo-target \
    --workdir /work \
    "$IMAGE" sleep infinity >/dev/null
}

cmd_setup() {
  require_docker
  require_challenge
  mkdir -p "$STATE/box"
  build_image
  ensure_network
  ensure_volume "$SEED_VOL"
  ensure_volume "$CARGO_VOL"
  ensure_volume "$TARGET_VOL"
  ensure_volume "$RUNS_VOL"
  copy_seed
  create_box
  docker exec "$BOX" test -s /seed/feed.db
  echo "== cgroup limits (stored in $STATE/box/cgroup.txt)"
  docker exec "$BOX" bash "$BOX_IN_CONTAINER/cgroup-check.sh" | tee "$STATE/box/cgroup.txt"
  echo "== box $BOX ready"
}

cmd_box_check() {
  require_docker
  box_running || die "box $BOX is not running; run: bash $HERE/run.sh setup"
  mkdir -p "$STATE/box"

  echo "== cgroup limits (stored in $STATE/box/cgroup.txt)"
  docker exec "$BOX" bash "$BOX_IN_CONTAINER/cgroup-check.sh" | tee "$STATE/box/cgroup.txt"

  echo "== app port 3000 is free"
  if [ -n "$(docker exec "$BOX" ss -H -ltn 'sport = :3000')" ]; then
    docker exec "$BOX" ss -H -ltnp 'sport = :3000' >&2
    die "port 3000 is already in use; a stale app is running"
  fi
  echo "port 3000 is free"

  echo "== nginx on port 80"
  docker exec "$BOX" bash "$BOX_IN_CONTAINER/nginx-check.sh" check

  echo "== seed volume matches $CHALLENGE/seed/feed.db"
  docker exec "$BOX" rm -rf /seed-verify
  docker exec "$BOX" mkdir -p /seed-verify
  docker cp "$CHALLENGE/seed/feed.db" "$BOX:/seed-verify/feed.db"
  docker exec "$BOX" bash "$BOX_IN_CONTAINER/seed-check.sh" /seed-verify/feed.db
  docker exec "$BOX" rm -rf /seed-verify

  echo "== box-check passed"
}

build_in_builder() {
  echo "== build in $BUILDER"
  docker rm --force "$BUILDER" >/dev/null 2>&1 || true
  docker run --rm \
    --name "$BUILDER" \
    --network "$NET" \
    --volume "$ROOT:/work" \
    --volume "$CARGO_VOL:/cargo-home" \
    --volume "$TARGET_VOL:/cargo-target" \
    --env CARGO_HOME=/cargo-home \
    --env CARGO_TARGET_DIR=/cargo-target \
    --workdir /work \
    "$IMAGE" bash /work/benchmarks/twelve-dollar/box/build.sh
}

duration_seconds() {
  local raw="$1" number suffix
  number="${raw%[sm]}"
  suffix="${raw#"$number"}"
  case "$number" in
    ""|*[!0-9]*) die "duration '$raw' must look like 30s or 2m" ;;
  esac
  case "$suffix" in
    s) printf '%s\n' "$number" ;;
    m) printf '%s\n' "$((number * 60))" ;;
    "") printf '%s\n' "$number" ;;
    *) die "duration '$raw' must look like 30s or 2m" ;;
  esac
}

cmd_test() {
  require_docker
  box_running || die "box $BOX is not running; run: bash $HERE/run.sh setup"
  if [ ! -f "$HERE/build.sh" ]; then
    die "missing $HERE/build.sh; A2 supplies the benchmark build script"
  fi
  if [ ! -f "$HERE/start.sh" ]; then
    die "missing $HERE/start.sh; A2 supplies the benchmark start script"
  fi

  build_in_builder

  echo "== test in $BOX (mode $mode)"
  docker exec "$BOX" bash "$BOX_IN_CONTAINER/test.sh" --mode "$mode"
}

cmd_smoke() {
  require_docker
  require_challenge
  box_running || die "box $BOX is not running; run: bash $HERE/run.sh setup"
  [ -f "$HERE/build.sh" ] || die "missing $HERE/build.sh"
  [ -f "$HERE/start.sh" ] || die "missing $HERE/start.sh"
  [ -f "$HERE/box/collect.sh" ] || die "missing $HERE/box/collect.sh"
  [ -f "$HERE/box/app.sh" ] || die "missing $HERE/box/app.sh"

  vus="$opt_vus"
  ramp_s="$(duration_seconds "$opt_ramp")"
  hold_s="$(duration_seconds "$opt_duration")"

  build_in_builder

  if [ -n "$(git -C "$CHALLENGE" status --porcelain)" ]; then
    die "challenge checkout is dirty: $CHALLENGE"
  fi

  box_ip="$(docker inspect --format '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$BOX")"
  [ -n "$box_ip" ] || die "cannot read the IP address of $BOX"

  if [ "$mode" = nginx ]; then
    app_host=127.0.0.1
    app_port=3000
  else
    app_host=0.0.0.0
    app_port=80
  fi
  target_port=80

  run_id="${TWELVE_DOLLAR_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)-${mode}-${vus}vu}"
  case "$run_id" in
    ''|*[!a-zA-Z0-9_-]*) die "invalid run id: $run_id" ;;
  esac
  result_dir="$STATE/results/$run_id"
  [ ! -e "$result_dir" ] || die "result directory already exists: $result_dir"
  docker exec "$BOX" test ! -e "/runs/$run_id" || die "run directory already exists in $BOX: /runs/$run_id"
  mkdir -p "$STATE/results" "$result_dir"

  harness="/runs/$run_id/harness"
  app_pid=""
  collector_pid=""
  stats_pid=""
  k6_pid=""
  k6_started=false
  smoke_discard=false
  smoke_cleanup() {
    if [ "$k6_started" = true ]; then
      docker rm --force "$K6_NAME" >/dev/null 2>&1 || true
    fi
    if [ -n "$k6_pid" ]; then
      kill -TERM "$k6_pid" 2>/dev/null || true
      wait "$k6_pid" 2>/dev/null || true
    fi
    if [ -n "$stats_pid" ]; then
      touch "$result_dir/k6-stats.done"
      wait "$stats_pid" 2>/dev/null || true
      rm -f "$result_dir/k6-stats.done"
    fi
    if [ -n "$collector_pid" ]; then
      docker exec "$BOX" kill -TERM "$collector_pid" >/dev/null 2>&1 || true
    fi
    if [ -n "$app_pid" ]; then
      docker exec "$BOX" env PIDFILE="/runs/$run_id/app.pid" bash "$harness/box/app.sh" stop >/dev/null 2>&1 || true
    fi
    docker exec "$BOX" bash "$harness/box/nginx-check.sh" stop >/dev/null 2>&1 || true
    docker exec "$BOX" rm -rf "/runs/$run_id" >/dev/null 2>&1 || true
    if [ "$smoke_discard" = true ]; then
      rm -rf "$result_dir"
    fi
  }
  trap smoke_cleanup EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  docker exec "$BOX" mkdir -p "/runs/$run_id"
  docker exec "$BOX" chmod 777 "/runs/$run_id"
  docker exec "$BOX" mkdir -p "$harness/box"
  docker cp "$HERE/box/collect.sh" "$BOX:$harness/box/collect.sh"
  docker cp "$HERE/box/app.sh" "$BOX:$harness/box/app.sh"
  docker cp "$HERE/box/nginx-check.sh" "$BOX:$harness/box/nginx-check.sh"
  docker cp "$HERE/start.sh" "$BOX:$harness/start.sh"
  docker exec "$BOX" chmod +x "$harness/box/collect.sh" "$harness/box/app.sh" "$harness/box/nginx-check.sh" "$harness/start.sh"

  echo "== run $run_id (mode $mode, vus $vus, ramp ${ramp_s}s, hold ${hold_s}s)"
  echo "== nginx log and fresh database"
  docker exec "$BOX" bash "$harness/box/nginx-check.sh" stop
  docker exec "$BOX" sh -c ': > /var/log/nginx/bench.access.log; : > /var/log/nginx/bench.error.log'
  if [ "$mode" = nginx ]; then
    docker exec "$BOX" nginx
    nginx_ready=false
    for _ in $(seq 50); do
      if [ -n "$(docker exec "$BOX" ss -H -ltn 'sport = :80')" ]; then
        nginx_ready=true
        break
      fi
      sleep 0.1
    done
    [ "$nginx_ready" = true ] || die "nginx did not listen on port 80"
    docker exec "$BOX" sh -c ': > /var/log/nginx/bench.access.log'
  fi

  docker exec "$BOX" test ! -e "/runs/$run_id/feed.db" || die "database path already exists before the run"
  docker exec "$BOX" cp /seed/feed.db "/runs/$run_id/feed.db"

  if docker exec "$BOX" pgrep -x h12tiny-twelve >/dev/null 2>&1; then
    die "a stale h12tiny-twelve process is running in $BOX"
  fi
  if [ -n "$(docker exec "$BOX" ss -H -ltn "sport = :$app_port")" ]; then
    docker exec "$BOX" ss -H -ltnp "sport = :$app_port" >&2
    die "port $app_port is already in use in $BOX"
  fi

  h12tiny_revision="$(git -C "$ROOT" rev-parse HEAD)"
  if [ -n "$(git -C "$ROOT" status --porcelain)" ]; then
    h12tiny_dirty=true
  else
    h12tiny_dirty=false
  fi
  challenge_revision="$(git -C "$CHALLENGE" rev-parse HEAD)"
  challenge_dirty=false
  kernel="$(docker exec "$BOX" uname -r)"
  arch="$(docker exec "$BOX" uname -m)"
  cpu_count="$(docker exec "$BOX" lscpu | awk -F': +' '/^CPU\(s\):/ { print $2; exit }')"
  cpu_model="$(docker exec "$BOX" lscpu | awk -F': +' '/^Model name:/ { print $2; exit }')"
  [ "$cpu_model" != "-" ] || cpu_model="not exposed by this kernel"
  cpu_max="$(docker exec "$BOX" cat /sys/fs/cgroup/cpu.max)"
  memory_max="$(docker exec "$BOX" cat /sys/fs/cgroup/memory.max)"
  memory_swap_max="$(docker exec "$BOX" cat /sys/fs/cgroup/memory.swap.max)"
  nginx_version="$(docker exec "$BOX" nginx -v 2>&1)"
  rustc_version="$(docker exec "$BOX" rustc -V)"
  rustc_verbose="$(docker exec "$BOX" rustc -Vv)"
  rustc_commit="$(printf '%s\n' "$rustc_verbose" | awk '/^commit-hash:/ { print $2; exit }')"
  rustc_host="$(printf '%s\n' "$rustc_verbose" | awk '/^host:/ { print $2; exit }')"
  sqlite_version="$(docker exec "$BOX" sqlite3 --version | awk '{ print $1 }')"
  started_utc="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

  jaq -n \
    --arg run_id "$run_id" \
    --arg started_utc "$started_utc" \
    --arg mode "$mode" \
    --argjson vus "$vus" \
    --argjson ramp_up_s "$ramp_s" \
    --argjson hold_s "$hold_s" \
    --arg box_ip "$box_ip" \
    --argjson app_port "$app_port" \
    --arg image "$IMAGE" \
    --arg k6_image "$K6_IMAGE" \
    --argjson h12tiny_dirty "$h12tiny_dirty" \
    --arg h12tiny_revision "$h12tiny_revision" \
    --argjson challenge_dirty "$challenge_dirty" \
    --arg challenge_revision "$challenge_revision" \
    --arg kernel "$kernel" \
    --arg arch "$arch" \
    --arg cpu_model "$cpu_model" \
    --argjson cpu_count "$cpu_count" \
    --arg cpu_max "$cpu_max" \
    --arg memory_max "$memory_max" \
    --arg memory_swap_max "$memory_swap_max" \
    --arg nginx_version "$nginx_version" \
    --arg rustc_version "$rustc_version" \
    --arg rustc_commit "$rustc_commit" \
    --arg rustc_host "$rustc_host" \
    --arg sqlite_version "$sqlite_version" \
    '{
      run_id: $run_id,
      started_utc: $started_utc,
      mode: $mode,
      vus: $vus,
      ramp_up_s: $ramp_up_s,
      hold_s: $hold_s,
      box: {container: "h12t-twelve-box", image: $image, ip: $box_ip, app_port: $app_port},
      k6: {image: $k6_image, cpus: 8, memory: "10g", network: "h12t-twelve-net"},
      h12tiny: {revision: $h12tiny_revision, dirty: $h12tiny_dirty},
      challenge: {revision: $challenge_revision, dirty: $challenge_dirty},
      environment: {
        kernel: $kernel,
        arch: $arch,
        cpu_model: $cpu_model,
        cpu_count: $cpu_count,
        cgroup: {cpu_max: $cpu_max, memory_max: $memory_max, memory_swap_max: $memory_swap_max},
        nginx_version: $nginx_version,
        rustc: {version: $rustc_version, commit_hash: $rustc_commit, host: $rustc_host},
        sqlite_version: $sqlite_version
      },
      build: {
        profile: "release",
        opt_level: 3,
        lto: "thin",
        panic: "abort",
        strip: false,
        debug: false,
        target_cpu: "default (no target-cpu=native)"
      },
      pragmas: {
        journal_mode: "wal",
        synchronous: "NORMAL",
        busy_timeout_ms: 5000,
        cache_size: "SQLite default",
        mmap: "SQLite default"
      },
      gate_before: true,
      gate_after: false,
      db_preexisting: false
    }' > "$result_dir/run.json"

  echo "== start app on $app_host:$app_port"
  docker exec -d "$BOX" env \
    SQLITE_PATH="/runs/$run_id/feed.db" \
    JWT_SECRET=twelve-dollar-challenge \
    TWELVE_DOLLAR_READERS="$opt_readers" \
    HOST="$app_host" \
    PORT="$app_port" \
    START_SH="$harness/start.sh" \
    LOG="/runs/$run_id/app.log" \
    PIDFILE="/runs/$run_id/app.pid" \
    bash "$harness/box/app.sh" start
  app_pid=""
  for _ in $(seq 50); do
    app_pid="$(docker exec "$BOX" cat "/runs/$run_id/app.pid" 2>/dev/null || true)"
    [ -n "$app_pid" ] && break
    sleep 0.1
  done
  [ -n "$app_pid" ] || die "app start did not write a pid"
  if ! docker exec "$BOX" env \
    HOST="$app_host" PORT="$app_port" \
    LOG="/runs/$run_id/app.log" PIDFILE="/runs/$run_id/app.pid" \
    bash "$harness/box/app.sh" wait; then
    die "app did not become healthy"
  fi

  if ! quiet_perf_gate 2> "$result_dir/gate-before.log"; then
    cat "$result_dir/gate-before.log" >&2
    smoke_discard=true
    echo "perf gate deferred: host busy" >&2
    exit 75
  fi

  echo "== collect metrics for the k6 window"
  docker exec -d "$BOX" bash "$harness/box/collect.sh" \
    --out "/runs/$run_id/metrics.csv" \
    --pid "$app_pid" \
    --port "$app_port" \
    --db "/runs/$run_id/feed.db" \
    --pidfile "/runs/$run_id/collector.pid"
  collector_ready=false
  for _ in $(seq 50); do
    if docker exec "$BOX" test -s "/runs/$run_id/collector.pid" \
      && [ "$(docker exec "$BOX" sh -c "wc -l < /runs/$run_id/metrics.csv" 2>/dev/null || echo 0)" -ge 2 ]; then
      collector_ready=true
      break
    fi
    sleep 0.1
  done
  [ "$collector_ready" = true ] || die "collector did not produce a first sample"
  collector_pid="$(docker exec "$BOX" cat "/runs/$run_id/collector.pid")"

  docker rm --force "$K6_NAME" >/dev/null 2>&1 || true
  : > "$result_dir/k6-stats.log"
  rm -f "$result_dir/k6-stats.done"
  (
    while [ ! -f "$result_dir/k6-stats.done" ]; do
      if docker inspect "$K6_NAME" >/dev/null 2>&1; then
        docker stats --no-stream --format '{{json .}}' "$K6_NAME" >> "$result_dir/k6-stats.log" 2>/dev/null || true
      fi
      sleep 1
    done
  ) &
  stats_pid=$!

  echo "== k6 $vus VUs, ramp ${ramp_s}s, hold ${hold_s}s against http://$box_ip:$target_port"
  k6_started_ms="$(( $(date +%s) * 1000 ))"
  set +e
  k6_started=true
  docker run --rm \
    --name "$K6_NAME" \
    --network "$NET" \
    --cpus=8 \
    --memory=10g \
    --ulimit nofile=65535:65535 \
    --volume "$CHALLENGE:/challenge:ro" \
    --volume "$RUNS_VOL:/runs" \
    --env BASE_URL="http://$box_ip:$target_port" \
    --env VUS="$vus" \
    --env DURATION="${hold_s}s" \
    --env RAMP_UP="${ramp_s}s" \
    --env TOKENS=/challenge/seed/tokens.json \
    "$K6_IMAGE" run \
    --summary-export "/runs/$run_id/k6-summary.json" \
    /challenge/bench/load.js > "$result_dir/k6.log" 2>&1 &
  k6_pid=$!
  wait "$k6_pid"
  k6_rc=$?
  k6_pid=""
  k6_started=false
  set -e
  k6_finished_ms="$(( $(date +%s) * 1000 ))"

  gate_after=false
  if quiet_perf_gate -1 2> "$result_dir/gate-after.log"; then
    gate_after=true
  else
    cat "$result_dir/gate-after.log" >&2
  fi

  touch "$result_dir/k6-stats.done"
  wait "$stats_pid" 2>/dev/null || true
  stats_pid=""
  rm -f "$result_dir/k6-stats.done"

  if [ -n "$collector_pid" ]; then
    docker exec "$BOX" kill -TERM "$collector_pid" >/dev/null 2>&1 || true
    for _ in $(seq 50); do
      if ! docker exec "$BOX" kill -0 "$collector_pid" >/dev/null 2>&1; then
        break
      fi
      sleep 0.1
    done
    docker exec "$BOX" kill -KILL "$collector_pid" >/dev/null 2>&1 || true
    collector_pid=""
  fi

  echo "== stop app with SIGTERM"
  app_stop_rc=0
  docker exec "$BOX" env PIDFILE="/runs/$run_id/app.pid" bash "$harness/box/app.sh" stop || app_stop_rc=$?
  if [ "$app_stop_rc" -ne 0 ]; then
    echo "smoke: app did not stop cleanly" >&2
  fi
  app_summary="$(docker exec "$BOX" env LOG="/runs/$run_id/app.log" bash "$harness/box/app.sh" summary || true)"
  printf '%s\n' "$app_summary" > "$result_dir/shutdown.txt"
  app_pid=""

  echo "== stop nginx"
  docker exec "$BOX" bash "$harness/box/nginx-check.sh" stop

  docker cp "$BOX:/runs/$run_id/metrics.csv" "$result_dir/metrics.csv"
  docker cp "$BOX:/runs/$run_id/app.log" "$result_dir/app.log"
  if docker exec "$BOX" test -f "/runs/$run_id/k6-summary.json"; then
    docker cp "$BOX:/runs/$run_id/k6-summary.json" "$result_dir/k6-summary.json"
  fi
  docker cp "$BOX:/var/log/nginx/bench.access.log" "$result_dir/nginx-access.log"
  docker cp "$BOX:/var/log/nginx/bench.error.log" "$result_dir/nginx-error.log"

  jaq -n \
    --argjson exit_code "$k6_rc" \
    --argjson started_epoch_ms "$k6_started_ms" \
    --argjson finished_epoch_ms "$k6_finished_ms" \
    --argjson gate_after "$gate_after" \
    '{exit_code: $exit_code, started_epoch_ms: $started_epoch_ms, finished_epoch_ms: $finished_epoch_ms, gate_after: $gate_after}' \
    > "$result_dir/k6-run.json"
  jaq --argjson gate_after "$gate_after" '.gate_after = $gate_after' "$result_dir/run.json" > "$result_dir/run.json.tmp"
  mv "$result_dir/run.json.tmp" "$result_dir/run.json"

  [ -f "$result_dir/k6-summary.json" ] || die "k6 did not write a summary export"
  [ -f "$result_dir/metrics.csv" ] || die "collector did not write metrics.csv"

  echo "== summarize and report"
  bash "$HERE/tools/summarize.sh" "$result_dir"
  bash "$HERE/tools/report.sh" "$result_dir"

  valid="$(jaq -r '.valid' "$result_dir/summary.json")"
  docker exec "$BOX" rm -rf "/runs/$run_id"
  trap - EXIT INT TERM

  if [ "$valid" != true ]; then
    echo "smoke: run is not valid under D9; see $result_dir/summary.json" >&2
    exit 1
  fi
  if [ "$k6_rc" -ne 0 ]; then
    echo "smoke: k6 thresholds failed; see $result_dir/summary.json" >&2
    exit 1
  fi
  echo "== results $result_dir"
}

cmd_ceiling() {
  require_docker
  require_challenge
  box_running || die "box $BOX is not running; run: bash $HERE/run.sh setup"
  [ -f "$HERE/tools/ceiling.sh" ] || die "missing $HERE/tools/ceiling.sh"
  [ -f "$HERE/start-static.sh" ] || die "missing $HERE/start-static.sh"

  build_in_builder

  if [ -n "$(git -C "$CHALLENGE" status --porcelain)" ]; then
    die "challenge checkout is dirty: $CHALLENGE"
  fi

  run_id="ceiling-$(date -u +%Y%m%dT%H%M%SZ)"
  result_dir="$STATE/results/$run_id"
  [ ! -e "$result_dir" ] || die "result directory already exists: $result_dir"
  mkdir -p "$STATE/results"

  bash "$HERE/tools/ceiling.sh" \
    --out "$result_dir" \
    --challenge "$CHALLENGE" \
    --box "$BOX" \
    --net "$NET" \
    --runs-volume "$RUNS_VOL" \
    --k6-image "$K6_IMAGE" \
    --bench "$HERE" \
    --ramp-up 60 \
    --duration 60

  cp "$result_dir/ceiling.json" "$STATE/ceiling.json"
  echo "== results $result_dir"
}

cmd_macos_smoke() {
  require_docker
  require_challenge
  [ "$mode" = direct ] || die 'macOS smoke requires --mode direct'
  ensure_network
  local run_id result_dir
  run_id="$(date -u +%Y%m%dT%H%M%SZ)-macos-${opt_vus}vu"
  result_dir="$STATE/results/$run_id"
  bash "$HERE/macos/smoke.sh" "$HERE" "$CHALLENGE" "$result_dir" "$opt_vus" \
    "$(duration_seconds "$opt_ramp")" "$(duration_seconds "$opt_duration")" \
    "$opt_readers" "$K6_IMAGE" "$NET"
}

profile=box
mode=nginx
command=""
opt_vus=50
opt_duration=30s
duration_set=false
opt_ramp=10s
opt_readers=2

while [ $# -gt 0 ]; do
  case "$1" in
    box|macos)
      if [ -n "$command" ]; then
        usage
        exit 2
      fi
      profile="$1"
      ;;
    --profile)
      [ $# -ge 2 ] || { usage; exit 2; }
      profile="$2"
      shift
      ;;
    --profile=*)
      profile="${1#*=}"
      ;;
    --mode)
      [ $# -ge 2 ] || { usage; exit 2; }
      mode="$2"
      shift
      ;;
    --mode=*)
      mode="${1#*=}"
      ;;
    --vus)
      [ $# -ge 2 ] || { usage; exit 2; }
      opt_vus="$2"
      shift
      ;;
    --vus=*)
      opt_vus="${1#*=}"
      ;;
    --duration)
      [ $# -ge 2 ] || { usage; exit 2; }
      opt_duration="$2"
      duration_set=true
      shift
      ;;
    --duration=*)
      opt_duration="${1#*=}"
      duration_set=true
      ;;
    --ramp-up)
      [ $# -ge 2 ] || { usage; exit 2; }
      opt_ramp="$2"
      shift
      ;;
    --ramp-up=*)
      opt_ramp="${1#*=}"
      ;;
    --readers)
      [ $# -ge 2 ] || { usage; exit 2; }
      opt_readers="$2"
      shift
      ;;
    --readers=*)
      opt_readers="${1#*=}"
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    setup|box-check|test|smoke|search|confirm|attribute|report|ceiling)
      if [ -n "$command" ]; then
        usage
        exit 2
      fi
      command="$1"
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage
      exit 2
      ;;
  esac
  shift
done

case "$profile" in
  box) ;;
  macos) [ "$duration_set" = true ] || opt_duration=60s ;;
  *)
    echo "unknown profile: $profile" >&2
    usage
    exit 2
    ;;
esac

case "$mode" in
  nginx|direct) ;;
  *)
    echo "unknown mode: $mode" >&2
    usage
    exit 2
    ;;
esac

if [ "$profile" = macos ] && [ "$command" != smoke ]; then
  die 'macOS profile supports smoke only'
fi

case "$opt_vus" in
  ""|*[!0-9]*)
    echo "--vus must be a positive integer, got '$opt_vus'" >&2
    exit 2
    ;;
esac
if [ "$opt_vus" -le 0 ]; then
  echo "--vus must be a positive integer, got '$opt_vus'" >&2
  exit 2
fi

case "$opt_readers" in
  1|2|3|4) ;;
  *) echo "--readers must be between 1 and 4, got '$opt_readers'" >&2; exit 2 ;;
esac

case "$command" in
  setup) cmd_setup ;;
  box-check) cmd_box_check ;;
  test) cmd_test ;;
  smoke) if [ "$profile" = macos ]; then cmd_macos_smoke; else cmd_smoke; fi ;;
  ceiling) cmd_ceiling ;;
  search|confirm) bash "$HERE/tools/search.sh" "$command" "$mode" "$HERE" "$STATE" "$opt_readers" ;;
  attribute|report)
    echo "command $command is not implemented yet" >&2
    exit 2
    ;;
  "")
    usage
    exit 2
    ;;
esac
