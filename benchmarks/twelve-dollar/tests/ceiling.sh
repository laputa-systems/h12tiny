#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "$0")/../../.." && pwd)"
mkdir -p "$root/.work/tmp"
test_dir="$(mktemp -d "$root/.work/tmp/ceiling.XXXXXXXX")"
trap 'rm -rf "$test_dir"' EXIT
mkdir -p "$test_dir/bin" "$test_dir/challenge/bench" "$test_dir/challenge/seed" "$test_dir/templates"
printf '%s\n' 'export default function() {}' > "$test_dir/challenge/bench/load.js"
printf '%s\n' '[]' > "$test_dir/challenge/seed/tokens.json"
git -c init.templateDir="$test_dir/templates" -C "$test_dir/challenge" init -q
git -C "$test_dir/challenge" add bench/load.js seed/tokens.json
git -C "$test_dir/challenge" -c user.name=test -c user.email=test@example.invalid commit -qm fixture
cat > "$test_dir/bin/perf-gate-ready" <<'GATE'
#!/bin/sh
exit 0
GATE
cat > "$test_dir/bin/docker" <<'DOCKER'
#!/bin/sh
args=" $* "
case "$args" in
  *' inspect '*'.State.Running'*) printf 'true\n' ;;
  *' inspect '*'.NetworkSettings.Networks'*) printf '127.0.0.1\n' ;;
  *' stats '*) printf '%s\n' '{"CPUPerc":"1.00%","MemPerc":"5.00%","MemUsage":"512MiB / 10GiB"}' ;;
  *' exec '*' pgrep '*) exit 1 ;;
  *' exec '*' ss '*) exit 0 ;;
  *' exec '*' cat /runs/'*'/app.pid '*) printf '123\n' ;;
  *' exec '*' awk '*) printf 'h12tiny-static shutdown accepted=1 rejected=0 responses=2\n' ;;
  *' cp h12t-twelve-box:'*'/k6-summary.json '*) cp "$TWELVE_TEST_DIR/summary.json" "$3" ;;
  *' run '*' --name h12t-twelve-k6 '*)
    vus=0
    for arg in "$@"; do case "$arg" in VUS=*) vus="${arg#VUS=}" ;; esac; done
    jaq -n --argjson vus "$vus" '{metrics:{http_reqs:{count:1,rate:1},http_req_duration:{med:0.3,"p(95)":0.4,"p(99)":0.9,max:1},http_req_failed:{value:0},vus_max:{max:$vus}}}' > "$TWELVE_TEST_DIR/summary.json"
    if [ "${TWELVE_TEST_INVALID:-false}" = true ]; then
      jaq 'del(.metrics.vus_max)' "$TWELVE_TEST_DIR/summary.json" > "$TWELVE_TEST_DIR/invalid.json"
      mv "$TWELVE_TEST_DIR/invalid.json" "$TWELVE_TEST_DIR/summary.json"
    fi
    sleep 2
    printf '%s\n' 'http_reqs....: 1'
    ;;
esac
exit 0
DOCKER
chmod +x "$test_dir/bin/"*
run_ceiling() {
  PATH="$test_dir/bin:$PATH" TWELVE_TEST_DIR="$test_dir" TWELVE_TEST_INVALID="$1" /bin/bash \
    "$root/benchmarks/twelve-dollar/tools/ceiling.sh" --out "$test_dir/$2" \
    --challenge "$test_dir/challenge" --bench "$root/benchmarks/twelve-dollar" --ramp-up 1 --duration 1 > "$test_dir/$2.log" 2>&1
}
run_ceiling false valid || { cat "$test_dir/valid.log" >&2; exit 1; }
jaq -e '.status == "complete" and .modes.nginx.ceiling_vus == 14000 and .modes.direct.ceiling_vus == 25000 and (.probes | length) == 2 and all(.probes[]; .valid and (.saturated | not))' "$test_dir/valid/ceiling.json" >/dev/null
if run_ceiling true invalid; then echo 'missing VU metric was accepted' >&2; exit 1; fi
jaq -e '.status == "incomplete" and all(.probes[]; .valid == false and (.invalid_reasons | any(contains("malformed"))))' "$test_dir/invalid/ceiling.json" >/dev/null
