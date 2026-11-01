#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
source "$root/benchmarks/twelve-dollar/tools/k6-summary.sh"
mkdir -p "$root/.work/tmp"
test_dir="$(mktemp -d "$root/.work/tmp/k6-summary.XXXXXXXX")"
trap 'rm -rf "$test_dir"' EXIT
printf '%s\n' '{"metrics":{"http_reqs":{"count":1,"rate":1},"http_req_duration":{"med":0.3,"p(95)":0.4,"p(99)":0.9,"max":1},"http_req_failed":{"value":0},"vus_max":{"max":1}}}' > "$test_dir/valid.json"
valid_k6_request_metrics "$test_dir/valid.json"
valid_k6_generator_metrics "$test_dir/valid.json"
jaq 'del(.metrics.vus_max)' "$test_dir/valid.json" > "$test_dir/no-vus.json"
valid_k6_request_metrics "$test_dir/no-vus.json"
if valid_k6_generator_metrics "$test_dir/no-vus.json"; then exit 1; fi
jaq 'del(.metrics.http_req_duration["p(99)"])' "$test_dir/valid.json" > "$test_dir/no-latency.json"
if valid_k6_request_metrics "$test_dir/no-latency.json"; then exit 1; fi
if valid_k6_generator_metrics "$test_dir/no-latency.json"; then exit 1; fi
jaq '.metrics.http_reqs.count = 0' "$test_dir/valid.json" > "$test_dir/no-requests.json"
if valid_k6_generator_metrics "$test_dir/no-requests.json"; then exit 1; fi
