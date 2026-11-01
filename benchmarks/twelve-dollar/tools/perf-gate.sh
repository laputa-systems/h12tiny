quiet_perf_gate() {
  local output result
  PERF_GATE_REASON=""
  if output="$(perf-gate-ready "$@" 2>&1)"; then
    result=0
  else
    result=$?
  fi
  if [ -n "$output" ]; then printf '%s\n' "$output" >&2; fi
  case "$output" in
    *'perf-gate-ready: hot process tree'*) PERF_GATE_REASON=hot; return 75 ;;
  esac
  if [ "$result" -eq 75 ]; then PERF_GATE_REASON=load; fi
  return "$result"
}

settled_perf_gate() {
  local attempts="${PERF_GATE_SETTLE_ATTEMPTS:-6}"
  local wait_s="${PERF_GATE_SETTLE_WAIT:-5}"
  local i=1 result
  while :; do
    if quiet_perf_gate "$@"; then
      return 0
    else
      result=$?
    fi
    [ "$PERF_GATE_REASON" = hot ] || return "$result"
    [ "$i" -ge "$attempts" ] && return "$result"
    i=$((i + 1))
    sleep "$wait_s"
  done
}
