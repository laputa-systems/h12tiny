quiet_perf_gate() {
  local output result hot_pids pid command external in_hot_tree
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
      return 75
    fi
  fi
  return "$result"
}
