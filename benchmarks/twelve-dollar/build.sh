#!/usr/bin/env bash
set -euo pipefail

BENCH="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

cargo build --locked --release --manifest-path "$BENCH/Cargo.toml" -p h12tiny-app
