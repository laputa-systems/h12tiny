#!/usr/bin/env bash
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
BENCH="$(cd "$HERE/.." && pwd)"

if [ -z "${CARGO_TARGET_DIR:-}" ]; then
  if [ -d /cargo-target ]; then
    CARGO_TARGET_DIR=/cargo-target
  else
    CARGO_TARGET_DIR="$BENCH/target"
  fi
fi
export CARGO_TARGET_DIR

mkdir -p "$CARGO_TARGET_DIR"

if [ ! -f "$BENCH/build.sh" ]; then
  echo "missing $BENCH/build.sh (A2 supplies it)" >&2
  exit 1
fi

exec bash "$BENCH/build.sh"
