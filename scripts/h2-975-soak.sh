#!/usr/bin/env sh
# Non-gating corroboration for the h2#975 regression. The gated proof is
# `cargo test ... --test h2_975`.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

runs=${H2_975_RUNS:-12}
mkdir -p "$root/.work/tmp"
TMPDIR="$root/.work/tmp"
CARGO_TARGET_DIR="$root/.work/tmp/protocol-target"
export TMPDIR CARGO_TARGET_DIR

mode=${1:-scripted}
case "$mode" in
    scripted|--tls) ;;
    *) printf 'usage: sh scripts/h2-975-soak.sh [--tls]\n' >&2; exit 2 ;;
esac

completed=0
while [ "$completed" -lt "$runs" ]; do
    completed=$((completed + 1))
    if [ "$mode" = "--tls" ]; then
        printf 'tls run %s/%s: ' "$completed" "$runs"
        cargo test --locked --features client,server,http1,http2,tls \
            --test dogfood_tls -- \
            --exact concurrent_first_tls_h2_requests_share_one_handshake_and_session \
            >"$TMPDIR/h2-975-tls-$completed.log" 2>&1
    else
        printf 'scripted run %s/%s: ' "$completed" "$runs"
        cargo test --manifest-path crates/laputa-h2-futures-lite/Cargo.toml \
            -p h2-tests --test h2_975 \
            >"$TMPDIR/h2-975-$completed.log" 2>&1
    fi
    printf 'ok\n'
done

printf 'h2#975 %s soak: %s/%s runs, 0 stalls\n' "$mode" "$completed" "$runs"
