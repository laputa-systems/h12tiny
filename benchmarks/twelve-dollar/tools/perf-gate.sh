quiet_perf_gate() {
  local output result
  if output="$(perf-gate-ready "$@" 2>&1)"; then
    result=0
  else
    result=$?
  fi
  if [ -n "$output" ]; then printf '%s\n' "$output" >&2; fi
  case "$output" in
    *'perf-gate-ready: hot process tree'*) return 75 ;;
  esac
  return "$result"
}
