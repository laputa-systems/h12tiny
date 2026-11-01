#!/usr/bin/env bash
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
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

usage() {
  cat >&2 <<'EOF'
usage: run.sh [box|macos] [--profile box|macos] [--mode nginx|direct] <command>

commands: setup, box-check, test
not implemented in H1: smoke, search, confirm, attribute, report, ceiling

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

cmd_test() {
  require_docker
  box_running || die "box $BOX is not running; run: bash $HERE/run.sh setup"
  if [ ! -f "$HERE/build.sh" ]; then
    die "missing $HERE/build.sh; A2 supplies the benchmark build script"
  fi
  if [ ! -f "$HERE/start.sh" ]; then
    die "missing $HERE/start.sh; A2 supplies the benchmark start script"
  fi

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

  echo "== test in $BOX (mode $mode)"
  docker exec "$BOX" bash "$BOX_IN_CONTAINER/test.sh" --mode "$mode"
}

profile=box
mode=nginx
command=""

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
  macos)
    echo "profile macos is not implemented in H1; it lands with M1" >&2
    exit 2
    ;;
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

case "$command" in
  setup) cmd_setup ;;
  box-check) cmd_box_check ;;
  test) cmd_test ;;
  smoke|search|confirm|attribute|report|ceiling)
    echo "command $command is not implemented in H1" >&2
    exit 2
    ;;
  "")
    usage
    exit 2
    ;;
esac
