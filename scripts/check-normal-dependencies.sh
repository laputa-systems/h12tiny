#!/usr/bin/env sh
# Check only an enabled *normal* dependency graph. Cargo.lock is deliberately
# not inspected: dev and optional dependencies are allowed to exist there.
# With no arguments, audit the facade's `full` production configuration rather
# than its intentionally empty default feature set. Callers can pass another
# Cargo feature selection to inspect a narrower configuration.
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
manifest="$script_dir/../Cargo.toml"

if [ "$#" -eq 0 ]; then
    set -- --features full
fi

tree=$(cargo tree --manifest-path "$manifest" -e normal --prefix none "$@")

# Match Cargo package names rather than substrings so `tokio-metrics` cannot
# hide a direct `tokio` package and unrelated names do not trigger a match.
# `libc` is permitted directly and transitively. Optional serialization
# dependencies belong to explicit util/web features.
forbidden='\
tokio
tokio-util
native-tls
ring
aws-lc-rs
aws-lc-sys
openssl
openssl-sys
cc
cmake
bindgen
hyper-util
reqwest
tower
tower-layer
axum
async-trait
url
mime
cookie
socket2
webpki-roots
'

printf '%s\n' "$forbidden" | while IFS= read -r package; do
    [ -n "$package" ] || continue
    if printf '%s\n' "$tree" | awk -v package="$package" '$1 == package { found = 1 } END { exit !found }'; then
        printf 'forbidden normal dependency: %s\n' "$package" >&2
        exit 1
    fi
done

printf 'normal dependency graph contains no forbidden packages\n'
