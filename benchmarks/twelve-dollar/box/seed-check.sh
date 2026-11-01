#!/usr/bin/env bash
set -euo pipefail

reference="${1:?usage: seed-check.sh <reference-feed.db>}"
seed="${SEED:-/seed/feed.db}"

if [ ! -f "$seed" ]; then
  echo "missing seed database $seed" >&2
  exit 1
fi
if [ ! -f "$reference" ]; then
  echo "missing reference database $reference" >&2
  exit 1
fi

cmp "$seed" "$reference"

seed_bytes="$(wc -c < "$seed")"
reference_bytes="$(wc -c < "$reference")"
printf 'seed /seed/feed.db %s bytes, reference %s bytes, cmp identical\n' "$seed_bytes" "$reference_bytes"
