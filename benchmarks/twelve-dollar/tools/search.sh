#!/usr/bin/env bash
set -euo pipefail

action="$1"
mode="$2"
bench="$3"
state="$4"
readers="$5"
case "$mode" in nginx) cap=14000 ;; direct) cap=25000 ;; *) echo "unknown mode: $mode" >&2; exit 2 ;; esac
mkdir -p "$state/results"
lock="$state/search-$mode.lock"
mkdir "$lock" 2>/dev/null || { echo "search: another search may be running ($lock)" >&2; exit 1; }
trap 'rmdir "$lock" || echo "search: could not remove $lock" >&2' EXIT
ceiling_file="$state/ceiling.json"
[ -f "$ceiling_file" ] || { echo "search: missing measured ceiling $ceiling_file" >&2; exit 1; }
measured_cap="$(jaq -er --arg mode "$mode" 'select(.status == "complete" and .modes[$mode].status == "complete") | .modes[$mode].ceiling_vus | select(type == "number" and . > 0 and floor == .)' "$ceiling_file")" || { echo "search: no valid measured ceiling for $mode" >&2; exit 1; }
[ "$measured_cap" -ge "$cap" ] || cap="$measured_cap"
search_file="$state/search-$mode.json"
revision="$(git -C "$bench" rev-parse HEAD)"

save() {
  jaq --argjson runs "$runs" --argjson low "$low" --argjson high "${high:-null}" \
    --argjson modified "$modified" --arg status "$status" --arg stage "$stage" --argjson score "${score:-null}" \
    '.runs=$runs | .low=$low | .high=$high | .modified=$modified | .status=$status | .stage=$stage | .score=$score' \
    "$search_file" > "$search_file.tmp"
  mv "$search_file.tmp" "$search_file"
}

run_level() {
  local vus="$1" duration="$2" phase="$3" run_id run_dir rc summary
  if [ "$(jaq 'length' <<< "$runs")" -ge 14 ]; then
    echo 'search: 14-run limit reached' >&2
    return 2
  fi
  run_id="$(date -u +%Y%m%dT%H%M%SZ)-${mode}-${phase}-${vus}vu-$(jaq 'length' <<< "$runs")"
  run_dir="$state/results/$run_id"
  if TWELVE_DOLLAR_RUN_ID="$run_id" bash "$bench/run.sh" box smoke --mode "$mode" --vus "$vus" --ramp-up 60s --duration "$duration" --readers "$readers" > "$state/results/$run_id.log" 2>&1; then
    rc=0
  else
    rc=$?
  fi
  if [ ! -f "$run_dir/summary.json" ]; then
    echo "search: no summary for $run_id (exit $rc); see $state/results/$run_id.log" >&2
    runs="$(jaq -n --argjson runs "$runs" --arg id "$run_id" --arg phase "$phase" --argjson vus "$vus" '$runs + [{id:$id,phase:$phase,vus:$vus,valid:false,pass:null}]')"
    status=deferred
    save
    return 2
  fi
  summary="$run_dir/summary.json"
  if [ "$(jaq -r '.valid' "$summary")" != true ]; then
    echo "search: invalid run $run_id; see $summary" >&2
    runs="$(jaq -n --argjson runs "$runs" --arg id "$run_id" --arg phase "$phase" --argjson vus "$vus" '$runs + [{id:$id,phase:$phase,vus:$vus,valid:false,pass:null}]')"
    status=deferred
    save
    return 2
  fi
  run_pass=false
  if [ "$(jaq -r '.k6.exit_code' "$summary")" = 0 ]; then run_pass=true; fi
  runs="$(jaq -n --argjson runs "$runs" --arg id "$run_id" --arg phase "$phase" --argjson vus "$vus" --argjson pass "$run_pass" '$runs + [{id:$id,phase:$phase,vus:$vus,valid:true,pass:$pass}]')"
  save
  echo "== $phase $mode $vus VUs: pass=$run_pass ($run_dir)"
}

if [ "$action" = search ]; then
  stage=search
  if [ -e "$search_file" ]; then
    [ "$(jaq -r '.stage' "$search_file")" = search ] && [ "$(jaq -r '.status' "$search_file")" != searched ] || { echo "search: existing completed or confirming search in $search_file" >&2; exit 1; }
    [ "$(jaq -r '.readers' "$search_file")" = "$readers" ] && [ "$(jaq -r '.cap' "$search_file")" = "$cap" ] && [ "$(jaq -r '.revision' "$search_file")" = "$revision" ] || { echo 'search: revision, reader count, or ceiling changed' >&2; exit 1; }
    runs="$(jaq -c '.runs' "$search_file")"
  else
    runs='[]'
    jaq -n --arg mode "$mode" --argjson cap "$cap" --argjson readers "$readers" --arg revision "$revision" '{mode:$mode,cap:$cap,readers:$readers,revision:$revision,runs:[],low:0,high:null,modified:false,status:"searching",stage:"search",score:null}' > "$search_file.tmp"
    mv "$search_file.tmp" "$search_file"
  fi
  low=0
  high=""
  modified=false
  if [ "$(jaq -r '[.[] | select(.phase == "probe" and .valid)] | if length > 0 then .[0].pass else true end' <<< "$runs")" = false ]; then modified=true; fi
  status=searching
  score=""
  warmup=1000
  [ "$warmup" -le "$cap" ] || warmup="$cap"
  if [ "$cap" -lt 2500 ]; then modified=true; fi
  save
  if [ "$(jaq '[.[] | select(.phase == "warmup" and .valid)] | length' <<< "$runs")" -eq 0 ]; then
    run_level "$warmup" 2m warmup || exit $?
  fi
  while :; do
    low=0
    high=""
    while read -r previous passed; do
      [ -n "$previous" ] || continue
      if [ "$passed" = true ]; then
        [ "$previous" -le "$low" ] || low="$previous"
      else
        if [ -z "$high" ] || [ "$previous" -lt "$high" ]; then high="$previous"; fi
      fi
    done <<< "$(jaq -r '.[] | select(.phase == "probe" and .valid) | "\(.vus) \(.pass)"' <<< "$runs")"
    if [ "$low" -eq 0 ] && [ -n "$high" ]; then
      modified=true
      [ "$high" -gt 1 ] || { status=no_pass; save; echo 'search: no passing level' >&2; exit 1; }
      vus=$((high / 2))
    elif [ "$low" -eq 0 ]; then
      vus=2500
      [ "$vus" -le "$cap" ] || vus="$cap"
    elif [ -z "$high" ]; then
      [ "$low" -lt "$cap" ] || break
      vus=$((low * 2))
      [ "$vus" -le "$cap" ] || vus="$cap"
    else
      [ $((high - low)) -gt 250 ] || break
      vus=$(((low + high) / 2))
    fi
    run_level "$vus" 2m probe || exit $?
  done
  status=searched
  save
  echo "== search $mode: highest passing probe $low VUs; confirmation pending"
elif [ "$action" = confirm ]; then
  [ -f "$search_file" ] || { echo "search: missing $search_file" >&2; exit 1; }
  previous_status="$(jaq -r '.status' "$search_file")"
  previous_stage="$(jaq -r '.stage' "$search_file")"
  if [ "$previous_status" != searched ] && { [ "$previous_stage" != confirm ] || { [ "$previous_status" != deferred ] && [ "$previous_status" != confirming ]; }; }; then
    echo 'search: no completed search to confirm' >&2; exit 1
  fi
  [ "$(jaq -r '.readers' "$search_file")" = "$readers" ] || { echo 'search: reader count differs from the search' >&2; exit 1; }
  [ "$(jaq -r '.cap' "$search_file")" = "$cap" ] || { echo 'search: measured ceiling differs from the search' >&2; exit 1; }
  [ "$(jaq -r '.revision' "$search_file")" = "$revision" ] || { echo 'search: revision differs from the search' >&2; exit 1; }
  runs="$(jaq -c '.runs' "$search_file")"
  low="$(jaq -r '.low' "$search_file")"
  high="$(jaq -r '.high // empty' "$search_file")"
  modified="$(jaq -r '.modified' "$search_file")"
  score=""
  status=confirming
  stage=confirm
  save
  vus="$low"
  while [ "$vus" -gt 0 ]; do
    completed="$(jaq -c --argjson vus "$vus" '[.[] | select(.phase == "confirm" and .valid and .vus == $vus)]' <<< "$runs")"
    while [ "$(jaq 'length' <<< "$completed")" -lt 2 ]; do
      run_level "$vus" 5m confirm || exit $?
      completed="$(jaq -c --argjson vus "$vus" '[.[] | select(.phase == "confirm" and .valid and .vus == $vus)]' <<< "$runs")"
    done
    if [ "$(jaq -r 'all(.[]; .pass)' <<< "$completed")" = true ]; then
      score="$vus"
      status=confirmed
      save
      if [ "$vus" -eq "$cap" ]; then
        echo "== $mode: passes at the ceiling, generator-limited ($vus VUs)"
      else
        echo "== $mode confirmed score: $vus VUs"
      fi
      for run_id in $(jaq -r --argjson vus "$vus" '[.runs[] | select(.phase == "confirm" and .valid and .vus == $vus)] | .[-2:] | .[].id' "$search_file"); do
        cat "$state/results/$run_id/report.md"
      done
      exit 0
    fi
    vus=$((vus - 250))
  done
  status=no_pass
  save
  echo 'search: no confirmed passing level' >&2
  exit 1
else
  echo "unknown search action: $action" >&2
  exit 2
fi
