#!/usr/bin/env bash
set -euo pipefail

BENCH="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TARGET="${CARGO_TARGET_DIR:-$BENCH/target}"

exec "$TARGET/release/h12tiny-twelve"
