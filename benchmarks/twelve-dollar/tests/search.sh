#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
mkdir -p "$root/.work/tmp"
test_dir="$(mktemp -d "$root/.work/tmp/search.XXXXXXXX")"
runner=""
cleanup() {
  if [ -n "$runner" ]; then kill -KILL "$runner" 2>/dev/null || true; wait "$runner" 2>/dev/null || true; fi
  if [ -f "$test_dir/hang.pid" ]; then kill -KILL "$(cat "$test_dir/hang.pid")" 2>/dev/null || true; fi
  rm -rf "$test_dir"
}
trap cleanup EXIT
mkdir -p "$test_dir/bench" "$test_dir/state"
printf '%s\n' '{"status":"complete","modes":{"nginx":{"status":"complete","ceiling_vus":14000},"direct":{"status":"complete","ceiling_vus":25000}}}' > "$test_dir/state/ceiling.json"
cat > "$test_dir/bench/run.sh" <<'RUNNER'
#!/usr/bin/env bash
set -euo pipefail
vus=0
duration=""
while [ $# -gt 0 ]; do
  if [ "$1" = --vus ]; then vus="$2"; shift; fi
  if [ "$1" = --duration ]; then duration="$2"; shift; fi
  shift
done
state="$(cd "$(dirname "$0")/../state" && pwd)"
if [ "${SIM_HANG:-false}" = true ]; then printf '%s\n' "$$" > "$state/../hang.pid"; exec sleep 120; fi
dir="$state/results/$TWELVE_DOLLAR_RUN_ID"
mkdir -p "$dir"
exit_code=0
if [ "$vus" -ge 5000 ]; then exit_code=99; fi
valid=true
if [ "${SIM_INVALID:-false}" = true ]; then valid=false; fi
if [ "${SIM_INVALID_CONFIRM:-false}" = true ] && [ "$duration" = 5m ]; then
  if [ -e "$state/first-confirm" ]; then valid=false; else : > "$state/first-confirm"; fi
fi
printf '{"valid":%s,"k6":{"exit_code":%s}}\n' "$valid" "$exit_code" > "$dir/summary.json"
printf '%s\n' "$vus VUs" > "$dir/report.md"
[ "$exit_code" = 0 ]
RUNNER
bash "$root/benchmarks/twelve-dollar/tools/search.sh" search nginx "$test_dir/bench" "$test_dir/state" 2 >/dev/null
jaq -e '.status == "searched" and .low >= 4750 and .low < 5000 and .high == 5000 and .modified == false' "$test_dir/state/search-nginx.json" >/dev/null
bash "$root/benchmarks/twelve-dollar/tools/search.sh" confirm nginx "$test_dir/bench" "$test_dir/state" 2 >/dev/null
jaq -e '.status == "confirmed" and .score == .low and ([.runs[] | select(.phase == "confirm" and .pass)] | length) == 2 and (.runs | length) <= 14' "$test_dir/state/search-nginx.json" >/dev/null
if SIM_INVALID=true bash "$root/benchmarks/twelve-dollar/tools/search.sh" search direct "$test_dir/bench" "$test_dir/state" 2 >/dev/null 2>&1; then exit 1; fi
jaq -e '.status == "deferred" and .low == 0 and .runs[0].valid == false and .runs[0].pass == null' "$test_dir/state/search-direct.json" >/dev/null
bash "$root/benchmarks/twelve-dollar/tools/search.sh" search direct "$test_dir/bench" "$test_dir/state" 2 >/dev/null
jaq -e '.status == "searched" and .runs[0].valid == false and .low >= 4750 and .high == 5000' "$test_dir/state/search-direct.json" >/dev/null
if SIM_INVALID_CONFIRM=true bash "$root/benchmarks/twelve-dollar/tools/search.sh" confirm direct "$test_dir/bench" "$test_dir/state" 2 >/dev/null 2>&1; then exit 1; fi
jaq -e '.status == "deferred" and ([.runs[] | select(.phase == "confirm" and .valid)] | length) == 1' "$test_dir/state/search-direct.json" >/dev/null
bash "$root/benchmarks/twelve-dollar/tools/search.sh" confirm direct "$test_dir/bench" "$test_dir/state" 2 >/dev/null
jaq -e '.status == "confirmed" and ([.runs[] | select(.phase == "confirm" and .valid)] | length) == 2' "$test_dir/state/search-direct.json" >/dev/null
rm "$test_dir/state/search-direct.json"
SIM_HANG=true bash "$root/benchmarks/twelve-dollar/tools/search.sh" search direct "$test_dir/bench" "$test_dir/state" 2 > /dev/null 2>&1 &
runner=$!
for _ in $(seq 1 100); do
  [ -s "$test_dir/hang.pid" ] && break
  kill -0 "$runner" 2>/dev/null || exit 1
  sleep 0.1
done
[ -s "$test_dir/hang.pid" ]
kill -TERM "$runner"
wait "$runner" 2>/dev/null || true
runner=""
if kill -0 "$(cat "$test_dir/hang.pid")" 2>/dev/null; then echo 'probe survived cancellation' >&2; exit 1; fi
[ ! -e "$test_dir/state/search-direct.lock" ]
bash "$root/benchmarks/twelve-dollar/tools/search.sh" search direct "$test_dir/bench" "$test_dir/state" 2 >/dev/null
jaq -e '.status == "searched" and .runs[0].status == "interrupted"' "$test_dir/state/search-direct.json" >/dev/null
count="$(jaq '.runs | length' "$test_dir/state/search-direct.json")"
jaq '.status = "searching" | .runs[-1].status = "running" | .runs[-1].valid = false | .runs[-1].pass = null' "$test_dir/state/search-direct.json" > "$test_dir/state/recover.json"
mv "$test_dir/state/recover.json" "$test_dir/state/search-direct.json"
bash "$root/benchmarks/twelve-dollar/tools/search.sh" search direct "$test_dir/bench" "$test_dir/state" 2 >/dev/null
jaq -e --argjson count "$count" '.status == "searched" and .runs[-1].status == "complete" and (.runs | length) == $count' "$test_dir/state/search-direct.json" >/dev/null
