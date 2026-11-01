quiet_perf_gate() {
  local output result hot_pids pid command external in_hot_tree
  PERF_GATE_REASON=""
  if output="$(perf-gate-ready "$@" 2>&1)"; then
    result=0
  else
    result=$?
  fi
  if [ -n "$output" ]; then printf '%s\n' "$output" >&2; fi
  in_hot_tree=false
  case "$output" in
    *'perf-gate-ready: hot process tree'*) in_hot_tree=true ;;
  esac
  if [ "$in_hot_tree" = true ]; then
    hot_pids="$(printf '%s\n' "$output" | sed -n 's/^[[:space:]]*\* pid=\([0-9][0-9]*\).*/\1/p')"
    external=false
    for pid in $hot_pids; do
      command="$(ps -p "$pid" -o command= 2>/dev/null || true)"
      case "$command" in
        *"${PERF_GATE_IGNORE_PROCESS-OrbStack Helper}"*) ;;
        *) external=true ;;
      esac
    done
    if [ -z "$hot_pids" ] || [ "$external" = true ]; then
      PERF_GATE_REASON=hot
      return 75
    fi
  fi
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
