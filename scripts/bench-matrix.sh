#!/usr/bin/env sh
# Repeated HTTP/1.1 and HTTP/2 matrix for the deterministic benchmark endpoints.
# Start examples/interop-server first, then run:
#
#   scripts/bench-matrix.sh
#
# Raw oha and h2load output is kept under .work/bench/<timestamp>/ so a run can
# be summarized or compared without rerunning the load generators.
set -eu

usage() {
    cat <<'EOF'
usage: scripts/bench-matrix.sh

Environment controls:
  H12TINY_BENCH_URL          base URL (default: http://127.0.0.1:3000)
  H12TINY_BENCH_TLS          1 passes --insecure to oha (default: 0)
  H12TINY_BENCH_REQUESTS     requests per run (default: 20000)
  H12TINY_BENCH_PATHS        endpoint paths (default: "/0 /1k /64k /1m /headers")
  H12TINY_BENCH_CONNECTIONS  connection counts (default: "1 16 128")
  H12TINY_BENCH_STREAMS      H2 streams per connection (default: 16)
  H12TINY_BENCH_DIR          output directory (default: .work/bench/<timestamp>)
EOF
}

if [ "${1:-}" = "--help" ] || [ "${1:-}" = "-h" ]; then
    usage
    exit 0
fi
if [ "$#" -ne 0 ]; then
    usage >&2
    exit 2
fi

die() {
    echo "bench-matrix: $*" >&2
    exit 2
}

require_tool() {
    command -v "$1" >/dev/null 2>&1 || die "required tool not found: $1"
}

require_tool oha
require_tool h2load

base=${H12TINY_BENCH_URL:-http://127.0.0.1:3000}
tls=${H12TINY_BENCH_TLS:-0}
requests=${H12TINY_BENCH_REQUESTS:-20000}
paths=${H12TINY_BENCH_PATHS:-"/0 /1k /64k /1m /headers"}
connections=${H12TINY_BENCH_CONNECTIONS:-"1 16 128"}
streams=${H12TINY_BENCH_STREAMS:-16}
out=${H12TINY_BENCH_DIR:-.work/bench/$(date +%Y%m%d-%H%M%S)}

case "$tls" in
    0|1) ;;
    *) die "H12TINY_BENCH_TLS must be 0 or 1" ;;
esac
case "$requests" in
    ''|*[!0-9]*|0) die "H12TINY_BENCH_REQUESTS must be a positive integer" ;;
esac
case "$streams" in
    ''|*[!0-9]*|0) die "H12TINY_BENCH_STREAMS must be a positive integer" ;;
esac

mkdir -p "$out"
base=${base%/}

echo "matrix output: $out"
echo "url=$base requests=$requests connections=\"$connections\" paths=\"$paths\""

for path in $paths; do
    case "$path" in
        /*) ;;
        *) die "paths must start with '/': $path" ;;
    esac
    url="$base$path"
    label=$(printf '%s' "$path" | tr '/' '_')
    for count in $connections; do
        case "$count" in
            ''|*[!0-9]*|0) die "connection counts must be positive integers" ;;
        esac
        h1="$out/h1${label}-c${count}.txt"
        if [ "$tls" = 1 ]; then
            oha --insecure --no-tui --http-version 1.1 -n "$requests" -c "$count" \
                "$url" >"$h1" 2>&1
        else
            oha --no-tui --http-version 1.1 -n "$requests" -c "$count" \
                "$url" >"$h1" 2>&1
        fi
        h1_rate=$(awk '/Requests\/sec:/ { print $2 }' "$h1" | tail -1)
        printf 'h1 path=%-8s c=%-4s requests_per_second=%s\n' "$path" "$count" "${h1_rate:-?}"

        h2="$out/h2${label}-c${count}m${streams}.txt"
        h2load -n "$requests" -c "$count" -m "$streams" "$url" >"$h2" 2>&1
        h2_rate=$(awk '/finished in/ { print $4 }' "$h2" | tail -1)
        printf 'h2 path=%-8s c=%-4s m=%-3s requests_per_second=%s\n' \
            "$path" "$count" "$streams" "${h2_rate:-?}"
    done
done
